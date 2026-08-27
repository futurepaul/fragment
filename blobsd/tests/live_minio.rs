//! Live end-to-end proof against a real S3 endpoint. Opt-in: `#[ignore]`d by
//! default; run with
//!
//! ```text
//! cargo test --test live_minio -- --ignored --nocapture
//! ```
//!
//! Defaults target the project dev stack (scripts/dev up): local MinIO on
//! 127.0.0.1:9000, creds minio/miniosecret, bucket fragment-dev. Every
//! default can be overridden through the standard environment variables
//! (S3_ENDPOINT, AWS_*, BLOBSD_BUCKET) without code changes.

use axum::body::Body;
use axum::http::Request;
use blobsd::app::{build_router, AppState};
use blobsd::auth::tests_fixtures::{self as fx, Override};
use blobsd::bucket::Bucket;
use blobsd::config::Config;
use http_body_util::BodyExt as _;
use std::collections::HashMap;
use tower::ServiceExt as _;

const SERVER_URL: &str = "http://127.0.0.1:8091";

fn live_vars() -> HashMap<String, String> {
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("BLOBSD_LISTEN".into(), "127.0.0.1:0".into());
    let dir = std::env::temp_dir().join(format!("blobsd-live-{}", std::process::id()));
    vars.insert("BLOBSD_DATA_DIR".into(), dir.display().to_string());
    vars.insert("BLOBSD_PUBLIC_URL".into(), SERVER_URL.into());
    vars.insert(
        "BLOBSD_BUCKET".into(),
        std::env::var("BLOBSD_BUCKET").unwrap_or_else(|_| "fragment-dev".into()),
    );
    vars.insert(
        "S3_ENDPOINT".into(),
        std::env::var("S3_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:9000".into()),
    );
    vars.insert(
        "AWS_ACCESS_KEY_ID".into(),
        std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "minio".into()),
    );
    vars.insert(
        "AWS_SECRET_ACCESS_KEY".into(),
        std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_else(|_| "miniosecret".into()),
    );
    vars.insert(
        "AWS_REGION".into(),
        std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into()),
    );
    vars.insert("BLOBSD_ALLOW_NPUBS".into(), fx::test_npub());
    vars.insert(
        "BLOBSD_INTERNAL_TOKEN".into(),
        "live-minio-internal-token-0123456789".into(),
    );
    vars.insert("BLOBSD_PUBLIC_GET".into(), "true".into());
    vars
}

/// The stack boots against the REAL S3 client — this is the point of the
/// test, so no in-memory swap happens here.
async fn live_router() -> (axum::Router, Bucket) {
    let cfg = Config::from_map(&live_vars()).expect("live config valid");
    let state = AppState::boot(cfg).await.expect("live boot");
    let bucket = state.bucket.clone();
    (build_router(state), bucket)
}

async fn call(
    router: &axum::Router,
    req: Request<Body>,
) -> (axum::http::StatusCode, axum::http::HeaderMap, bytes::Bytes) {
    let response = router.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, body)
}

fn sha_hex(data: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(data))
}

#[tokio::test]
#[ignore = "live MinIO required; run with --ignored against scripts/dev up"]
async fn upload_get_delete_roundtrip_hits_real_bucket() {
    let (router, bucket) = live_router().await;

    // Unique content per run so the object key is fresh.
    let payload = format!(
        "blobsd live-minio proof {} {}\n",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
        std::process::id()
    )
    .into_bytes();
    let sha = sha_hex(&payload);

    // ---- upload through the full HTTP stack --------------------------------
    let auth = fx::build_auth_header(SERVER_URL, "upload", &Override::default());
    let put_req = Request::builder()
        .method("PUT")
        .uri("/upload")
        .header("content-length", payload.len())
        .header("authorization", auth.clone())
        .header("content-type", "text/x-live-proof")
        .body(Body::from(payload.clone()))
        .unwrap();
    let (status, _, descriptor_bytes) = call(&router, put_req).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{}", String::from_utf8_lossy(&descriptor_bytes));
    let descriptor: serde_json::Value = serde_json::from_slice(&descriptor_bytes).unwrap();
    assert_eq!(descriptor["sha256"], sha);

    // ---- the bytes REALLY landed in MinIO ----------------------------------
    let info = bucket
        .head(&sha)
        .await
        .expect("bucket reachable")
        .expect("object present in the live bucket");
    assert_eq!(info.size, payload.len() as u64);
    assert_eq!(info.content_type.as_deref(), Some("text/x-live-proof"));

    // ---- GET back through the server, streamed off S3 ----------------------
    let (status, headers, body) = call(&router, Request::builder().method("GET").uri(format!("/{sha}")).body(Body::empty()).unwrap()).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(&body[..], &payload[..]);
    assert_eq!(headers["etag"], format!("\"{sha}\""));

    // ---- replay through the full stack: identical descriptor ---------------
    let auth = fx::build_auth_header(SERVER_URL, "upload", &Override::default());
    let replay = Request::builder()
        .method("PUT")
        .uri("/upload")
        .header("content-length", payload.len())
        .header("authorization", auth)
        .header("content-type", "text/x-live-proof")
        .body(Body::from(payload.clone()))
        .unwrap();
    let (status, _, body2) = call(&router, replay).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(descriptor_bytes, body2, "replay descriptor identical");

    // ---- direct CAS probe: MinIO enforces the etag conditional dialect -----
    // A create over an existing key must be CLEANLY rejected (this is the
    // achieved race guarantee: provider-enforced put-if-absent).
    let outcome = bucket
        .put_create(&sha, object_store::PutPayload::from(payload.clone()), "text/x-live-proof")
        .await
        .expect("clean classification, no ambiguity");
    assert_eq!(outcome, blobsd::bucket::PutCreate::AlreadyExists);

    // ---- concurrent identical uploads through HTTP converge ----------------
    // Distinct hash: concurrent first-writes race the real conditional PUTs.
    let other = format!("blobsd concurrent {}\n", uuid_of_run()).into_bytes();
    let other_sha = sha_hex(&other);
    let auth2 = fx::build_auth_header(SERVER_URL, "upload", &Override::default());
    let mk_other = |auth: &str| {
        Request::builder()
            .method("PUT")
            .uri("/upload")
            .header("content-length", other.len())
            .header("authorization", auth.to_owned())
            .header("content-type", "text/plain")
            .body(Body::from(other.clone()))
            .unwrap()
    };
    let (ra, rb) = tokio::join!(router.clone().oneshot(mk_other(&auth2)), router.clone().oneshot(mk_other(&auth2)));
    let ra = ra.unwrap();
    let rb = rb.unwrap();
    assert_eq!(ra.status(), axum::http::StatusCode::OK, "racer A ok");
    assert_eq!(rb.status(), axum::http::StatusCode::OK, "racer B ok");
    let ba = ra.into_body().collect().await.unwrap().to_bytes();
    let bb = rb.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(ba, bb, "converged descriptors");
    assert!(ba.windows(sha_hex(&other).len()).any(|w| w == other_sha.as_bytes()));

    // ---- cleanup: DELETE removes descriptor AND live object ----------------
    let auth = fx::build_auth_header(SERVER_URL, "delete", &Override::default());
    let del = Request::builder()
        .method("DELETE")
        .uri(format!("/{sha}"))
        .header("authorization", auth)
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = call(&router, del).await;
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT);
    assert!(bucket.head(&sha).await.unwrap().is_none(), "object gone from MinIO");
}

fn uuid_of_run() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}
