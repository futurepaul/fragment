//! The single object-store surface blobsd speaks, bound to one bucket and the
//! `blobs/<sha256>` prefix — the same architecture celld's bucket.rs uses
//! (one ordinary client plus one no-retry CAS client, S3 etag dialect), built
//! here directly over `object_store` without depending on celld.
//!
//! Conditional-write contract, relied on by the upload convergence path:
//! [`Bucket::put_create`] answers `Ok(true)` applied, `Ok(false)` for a clean
//! provider-enforced conflict (the key already existed), and any other
//! failure stays an `Err` because the write may have committed. That
//! distinction drives whether the upload handler may retry content itself
//! versus reconcile through the database instead.

use object_store::path::Path;
use object_store::aws::{AmazonS3Builder, S3ConditionalPut};
use object_store::{
    Attribute, Attributes, ClientOptions, GetOptions, ObjectStore, PutMode, PutOptions,
    PutPayload, RetryConfig,
};
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::error::BootError;

/// Bucket-level failure taxonomy. `object_store`'s own error enum stays
/// intact underneath so callers can still match specific conditions.
#[derive(Debug, thiserror::Error)]
pub enum BucketError {
    #[error("object store: {0}")]
    Store(#[from] object_store::Error),
    /// A successful conditional write that carries no etag leaves nothing to
    /// fence future writes against; treat it as failed rather than inventing
    /// a placeholder token a later request would trust.
    #[error("conditional write succeeded without an etag")]
    MissingCasToken,
}

/// Metadata a read path needs about one object. `content_type` may be absent
/// on stores that dropped the attribute; callers fall back to the descriptor.
#[derive(Debug, Clone)]
pub struct ObjectInfo {
    pub size: u64,
    pub content_type: Option<String>,
}

/// Outcome of a put-if-absent. The bool distinction is the whole contract:
/// losing the race cleanly is normal convergence, not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutCreate {
    Applied,
    AlreadyExists,
}

#[derive(Clone)]
pub struct Bucket {
    /// Ordinary lane: bounded retries ON (reads, plain deletes).
    store: Arc<dyn ObjectStore>,
    /// CAS lane: retries OFF. A retried put-if-absent can race against its
    /// own first attempt's etag change and report a false clean rejection,
    /// so the ambiguity must reach the handler undisturbed.
    cas_store: Arc<dyn ObjectStore>,
    prefix: String,
}

impl Bucket {
    /// Build the S3-compatible client pair from validated config. Always
    /// path-style (`with_virtual_hosted_style_request(false)`): MinIO dev,
    /// Latitude and R2-style endpoints are all addressed by path, never by
    /// DNS-subdomain buckets.
    pub fn open_s3(cfg: &Config) -> Result<Bucket, BootError> {
        // Local/plain-HTTP endpoints are normal for this service (MinIO in
        // dev and CI); TLS-terminating gateways sit in front in production.
        let options = ClientOptions::new()
            .with_timeout(Duration::from_secs(15))
            .with_connect_timeout(Duration::from_secs(3))
            .with_allow_http(true);
        let builder = AmazonS3Builder::from_env()
            .with_bucket_name(&cfg.bucket)
            .with_region(&cfg.aws_region)
            .with_access_key_id(&cfg.aws_access_key_id)
            .with_secret_access_key(&cfg.aws_secret_access_key)
            .with_endpoint(&cfg.s3_endpoint)
            .with_virtual_hosted_style_request(false)
            .with_conditional_put(S3ConditionalPut::ETagMatch)
            .with_retry(Self::retry())
            .with_client_options(options.clone());
        let cas_builder = builder.clone().with_retry(Self::cas_retry());
        let store = builder.build().map_err(|e| BootError::Bucket(format!("build s3 client: {e}")))?;
        let cas_store = cas_builder.build().map_err(|e| BootError::Bucket(format!("build s3 cas client: {e}")))?;
        Ok(Bucket {
            store: Arc::new(store),
            cas_store: Arc::new(cas_store),
            prefix: cfg.bucket_prefix.to_string(),
        })
    }

    /// In-memory store, standing in for the bucket in the full test matrix.
    /// Identical code paths run above this seam in every test but the live
    /// MinIO one. Unused by the binary; kept for tests/ (hence the allow).
    #[allow(dead_code)]
    pub fn in_memory() -> Bucket {
        let store: Arc<dyn ObjectStore> =
            Arc::new(object_store::memory::InMemory::new());
        Bucket {
            store: store.clone(),
            cas_store: store,
            prefix: "blobs".to_string(),
        }
    }

    fn retry() -> RetryConfig {
        RetryConfig {
            max_retries: 2,
            retry_timeout: Duration::from_secs(30),
            ..RetryConfig::default()
        }
    }

    fn cas_retry() -> RetryConfig {
        RetryConfig {
            max_retries: 0,
            retry_timeout: Duration::from_secs(30),
            ..RetryConfig::default()
        }
    }

    /// The storage key for a content hash. Callers pass validated lowercase
    /// hex; this is the single place prefixes are spliced on.
    fn key(&self, sha_hex: &str) -> Path {
        debug_assert!(
            sha_hex.len() == 64 && sha_hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "bucket keys receive validated sha256 hex"
        );
        Path::parse(format!("{}/{}", self.prefix, sha_hex))
            .expect("ascii hex forms a valid object_store path")
    }

    /// Size + content-type probe, or `None` when absent.
    pub async fn head(&self, sha_hex: &str) -> Result<Option<ObjectInfo>, BucketError> {
        let key = self.key(sha_hex);
        let options = GetOptions {
            head: true,
            ..GetOptions::default()
        };
        match self.store.get_opts(&key, options).await {
            Ok(result) => Ok(Some(ObjectInfo {
                size: result.meta.size as u64,
                content_type: result
                    .attributes
                    .get(&Attribute::ContentType)
                    .map(|value| value.to_string()),
            })),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(BucketError::Store(error)),
        }
    }

    /// Open an object (or a byte interval of it) for streaming. The returned
    /// payload streams off the wire; callers never buffer whole blobs.
    /// `range` is the inclusive [start, end] byte interval resolved against a
    /// known object size; `None` streams everything.
    pub async fn get_range(
        &self,
        sha_hex: &str,
        range: Option<std::ops::Range<u64>>,
    ) -> Result<Option<object_store::GetResult>, BucketError> {
        let key = self.key(sha_hex);
        // GetRange::Bounded is usize-based; the config ceiling on blob size
        // (MAX_MAX_BLOB_BYTES) keeps every object far inside usize range.
        let bounded = range.and_then(|interval| {
            let start = usize::try_from(interval.start).ok()?;
            let end = usize::try_from(interval.end).ok()?;
            Some(object_store::GetRange::Bounded(start..end))
        });
        let options = GetOptions {
            range: bounded,
            ..GetOptions::default()
        };
        match self.store.get_opts(&key, options).await {
            Ok(result) => Ok(Some(result)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(BucketError::Store(error)),
        }
    }

    /// Normalize a GetResult payload into one byte stream. Only the
    /// `Stream` arm occurs for the S3 and memory backends; the `File` arm
    /// exists for local-filesystem stores and is converted rather than
    /// refused so the read path stays backend-total.
    pub fn payload_stream(
        payload: object_store::GetResultPayload,
    ) -> futures_util::stream::BoxStream<'static, Result<bytes::Bytes, object_store::Error>> {
        use futures_util::StreamExt as _;
        match payload {
            object_store::GetResultPayload::Stream(stream) => stream,
            object_store::GetResultPayload::File(file, _path) => {
                let file = tokio::fs::File::from(file);
                futures_util::stream::unfold(file, |mut file| async move {
                    use tokio::io::AsyncReadExt as _;
                    let mut buffer = vec![0u8; 256 * 1024];
                    match file.read(&mut buffer).await {
                        Ok(0) => None,
                        Ok(n) => {
                            buffer.truncate(n);
                            let bytes = bytes::Bytes::from(buffer);
                            Some((Ok(bytes), file))
                        }
                        Err(err) => Some((
                            Err(object_store::Error::Generic {
                                store: "blobsd",
                                source: Box::new(err),
                            }),
                            file,
                        )),
                    }
                })
                .boxed()
            }
        }
    }

    /// Put-if-absent. See the module contract: `Ok(PutCreate::AlreadyExists)`
    /// is ONLY returned for a provider-enforced precondition rejection;
    /// anything ambiguous is an `Err`.
    pub async fn put_create(
        &self,
        sha_hex: &str,
        payload: PutPayload,
        content_type: &str,
    ) -> Result<PutCreate, BucketError> {
        let key = self.key(sha_hex);
        let mut attributes = Attributes::new();
        // Content-type strings were validated ASCII/length at the handler
        // boundary; AttributeValue takes the plain string form.
        attributes.insert(
            Attribute::ContentType,
            object_store::AttributeValue::from(content_type.to_string()),
        );
        let options = PutOptions {
            mode: PutMode::Create,
            attributes,
            ..PutOptions::default()
        };
        match self.cas_store.put_opts(&key, payload, options).await {
            Ok(result) => {
                if result.e_tag.is_none() {
                    return Err(BucketError::MissingCasToken);
                }
                Ok(PutCreate::Applied)
            }
            Err(error)
                if matches!(
                    &error,
                    object_store::Error::Precondition { .. }
                        | object_store::Error::AlreadyExists { .. }
                ) =>
            {
                Ok(PutCreate::AlreadyExists)
            }
            Err(error) => Err(BucketError::Store(error)),
        }
    }

    /// Delete one object. Idempotent: an absent key is already gone.
    pub async fn delete(&self, sha_hex: &str) -> Result<(), BucketError> {
        let key = self.key(sha_hex);
        match self.store.delete(&key).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(BucketError::Store(error)),
        }
    }

    /// Drain a streamed GetResult payload fully. Used by tests; production
    /// GETs stream the payload straight into the HTTP response instead.
    #[cfg(test)]
    pub async fn collect_body(result: object_store::GetResult) -> Result<Vec<u8>, BucketError> {
        use futures_util::StreamExt as _;
        let mut buffer = Vec::new();
        let mut stream = Self::payload_stream(result.payload);
        while let Some(chunk) = stream.next().await {
            buffer.extend_from_slice(&chunk.map_err(BucketError::Store)?);
        }
        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest as _;

    const HEX_A: &str = "a6b3c024f7d54c9cbdbb7bb8ea66a41e95a4bb2ff1c123ad4c3ac9ce97fd4a91";

    fn static_payload(data: &[u8]) -> PutPayload {
        PutPayload::from(data.to_vec())
    }

    async fn put_simple(bucket: &Bucket, data: &[u8], mime: &str) -> PutCreate {
        let mut hasher = sha2::Sha256::new();
        hasher.update(data);
        let digest = format!("{:x}", hasher.finalize());
        bucket.put_create(&digest, static_payload(data), mime).await.expect("put")
    }

    #[tokio::test]
    async fn create_then_conflict_roundtrip() {
        let bucket = Bucket::in_memory();
        // First put applies...
        assert_eq!(put_simple(&bucket, b"hello", "text/plain").await, PutCreate::Applied);
        // ...and an identical re-put is a CLEAN conflict, not an overwrite
        // and not an ambiguity. This is exactly the upload-converge case.
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"hello");
        let digest = format!("{:x}", hasher.finalize());
        let outcome = bucket
            .put_create(&digest, static_payload(b"hello"), "text/plain")
            .await
            .expect("clean classifying");
        assert_eq!(outcome, PutCreate::AlreadyExists);
        // Egress check: the object survived the loser untouched.
        assert_eq!(bucket.head(&digest).await.unwrap().unwrap().size, 5);
    }

    #[tokio::test]
    async fn head_reports_size_and_content_type_or_none() {
        let bucket = Bucket::in_memory();
        assert!(bucket.head("0".repeat(64).as_str()).await.unwrap().is_none());
        put_simple(&bucket, b"abcdef", "font/woff2").await;
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"abcdef");
        let digest = format!("{:x}", hasher.finalize());
        let info = bucket.head(&digest).await.unwrap().expect("present");
        assert_eq!(info.size, 6);
        assert_eq!(info.content_type.as_deref(), Some("font/woff2"));
        assert_ne!(HEX_A, digest, "sanity: fixture constant stays distinct");
    }

    #[tokio::test]
    async fn ranged_get_slices_exactly_and_missing_is_none() {
        let bucket = Bucket::in_memory();
        assert!(bucket.get_range(&"0".repeat(64), Some(0..2)).await.unwrap().is_none());
        put_simple(&bucket, b"abcdefgh", "application/octet-stream").await;
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"abcdefgh");
        let digest = format!("{:x}", hasher.finalize());

        let result = bucket
            .get_range(&digest, Some(2..5)) // half-open: c d e
            .await
            .unwrap()
            .expect("ranged hit");
        let body = Bucket::collect_body(result).await.unwrap();
        assert_eq!(body, b"cde");

        let whole = bucket.get_range(&digest, None).await.unwrap().unwrap();
        assert_eq!(Bucket::collect_body(whole).await.unwrap(), b"abcdefgh");
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let bucket = Bucket::in_memory();
        put_simple(&bucket, b"gone", "text/plain").await;
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"gone");
        let digest = format!("{:x}", hasher.finalize());
        bucket.delete(&digest).await.expect("first delete");
        bucket.delete(&digest).await.expect("second delete is a no-op");
        assert!(bucket.head(&digest).await.unwrap().is_none());
    }
}
