//! Integration matrix: full HTTP behavior through the production router
//! (`build_router`), with the bucket swapped for the in-memory backend. The
//! live MinIO path is covered by `tests/live_minio.rs` behind `#[ignore]`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse as _;
use blobsd::app::{build_router, AppState};
use blobsd::auth::tests_fixtures::{self as fx, Override};
use blobsd::bucket::Bucket;
use blobsd::config::Config;
use blobsd::error::BootError;
use http_body_util::BodyExt as _;
use std::collections::HashMap;
use tower::ServiceExt as _;

const SERVER_URL: &str = "https://blobsd.test";
const INTERNAL_TOKEN: &str = "integration-internal-token-0123456789";

struct World {
    router: axum::Router,
    /// Keeps the sqlite staging dir alive for the test's duration.
    #[allow(dead_code)]
    dir: tempfile::TempDir,
    bucket: Bucket,
}

async fn world() -> World {
    world_with_public_get("true").await
}

async fn world_with_public_get(public_get: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("BLOBSD_LISTEN".into(), "127.0.0.1:0".into());
    vars.insert(
        "BLOBSD_DATA_DIR".into(),
        dir.path().display().to_string(),
    );
    vars.insert("BLOBSD_PUBLIC_URL".into(), SERVER_URL.into());
    vars.insert("BLOBSD_BUCKET".into(), "fragment-dev".into());
    vars.insert("S3_ENDPOINT".into(), "http://127.0.0.1:9000".into());
    vars.insert("AWS_ACCESS_KEY_ID".into(), "minio".into());
    vars.insert("AWS_SECRET_ACCESS_KEY".into(), "miniosecret".into());
    vars.insert("AWS_REGION".into(), "us-east-1".into());
    vars.insert("BLOBSD_ALLOW_NPUBS".into(), fx::test_npub());
    vars.insert("BLOBSD_INTERNAL_TOKEN".into(), INTERNAL_TOKEN.into());
    vars.insert("BLOBSD_PUBLIC_GET".into(), public_get.into());

    let cfg = Config::from_map(&vars).expect("test config valid");
    let state = AppState::boot(cfg.clone())
        .await
        .expect("boot over in-memory plumbing");
    // Swap the S3 client for the in-memory store: identical code paths, no
    // network dependency for the whole matrix below.
    let state = AppState {
        bucket: Bucket::in_memory(),
        ..state
    };
    let router = build_router(state.clone());
    World {
        router,
        dir,
        bucket: state.bucket.clone(),
    }
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

fn head(path: &str) -> Request<Body> {
    Request::builder()
        .method("HEAD")
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

fn delete(path: &str, auth: Option<String>) -> Request<Body> {
    let mut builder = Request::builder().method("DELETE").uri(path);
    if let Some(auth) = auth {
        builder = builder.header("authorization", auth);
    }
    builder.body(Body::empty()).unwrap()
}

fn put(path: &str, auth: &str, content_type: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("PUT")
        .uri(path)
        .header("content-length", body.len())
        .header("authorization", auth);
    if let Some(ct) = content_type {
        builder = builder.header("content-type", ct);
    }
    builder.body(Body::from(body)).unwrap()
}

async fn call(router: &axum::Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, bytes::Bytes) {
    let response = router.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, body)
}

async fn upload_bytes(world: &World, data: &[u8], mime: &str) -> (StatusCode, bytes::Bytes) {
    let auth = fx::build_auth_header(SERVER_URL, "upload", &Override::default());
    let (status, _, body) = call(&world.router, put("/upload", &auth, Some(mime), data.to_vec())).await;
    (status, body)
}

fn sha_hex(data: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(data))
}

// ------------------------------------------------------------------ flow ---

#[tokio::test]
async fn upload_head_get_range_delete_full_cycle() {
    let world = world().await;
    let data = b"the quick brown fragment jumps over the lazy blob".to_vec();
    let sha = sha_hex(&data);

    // ---- upload -> descriptor ---------------------------------------------
    let (status, body) = upload_bytes(&world, &data, "text/markdown").await;
    assert_eq!(status, StatusCode::OK, "body: {}", String::from_utf8_lossy(&body));
    let descriptor: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(descriptor["sha256"], sha);
    assert_eq!(descriptor["size"], data.len() as u64);
    assert_eq!(descriptor["type"], "text/markdown");
    assert!(
        descriptor["uploaded"].as_i64().unwrap() > 1_700_000_000,
        "uploaded is unix seconds"
    );

    // ---- HEAD: specced headers, no body ------------------------------------
    let (status, headers, body) = call(&world.router, head(&format!("/{sha}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty(), "HEAD carries no body");
    assert_eq!(headers["etag"], format!("\"{sha}\""));
    assert_eq!(headers["cache-control"], "public, max-age=31536000, immutable");
    assert_eq!(headers["content-type"], "text/markdown");
    assert_eq!(headers["accept-ranges"], "bytes");

    // ---- GET full -----------------------------------------------------------
    let (status, headers, body) = call(&world.router, get(&format!("/{sha}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], &data[..], "body round-trips byte-exact");
    assert_eq!(headers["content-length"], data.len().to_string());

    // ---- GET single range ---------------------------------------------------
    let (status, headers, body) =
        call(&world.router, get(&format!("/{sha}")).tap_range(4, 9)).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body[..], &data[4..=9]);
    assert_eq!(headers["content-range"], format!("bytes 4-9/{}", data.len()));
    assert_eq!(headers["etag"], format!("\"{sha}\""));

    // Suffix form.
    let (status, _, body) = call(&world.router, get(&format!("/{sha}")).tap_suffix(5)).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body[..], &data[data.len() - 5..]);

    // Unsatisfiable start -> 416 with the object size.
    let (status, _, _) = call(&world.router, get(&format!("/{sha}")).tap_range(9999, 10000)).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);

    // Multi-interval refused outright.
    let (status, _, _) = call(&world.router, get(&format!("/{sha}")).tap_multi()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // ---- unknown sha -> 404, malformed -> 400 ------------------------------
    let (status, _, _) = call(&world.router, get(&"a".repeat(64))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(&world.router, get("/not-a-sha")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // ---- DELETE then gone ---------------------------------------------------
    let auth = fx::build_auth_header(SERVER_URL, "delete", &Override::default());
    let (status, _, _) = call(&world.router, delete(&format!("/{sha}"), Some(auth))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = call(&world.router, get(&format!("/{sha}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "object gone with the row");
    // Deleting again: row already gone -> 404 (absence is truth).
    let auth = fx::build_auth_header(SERVER_URL, "delete", &Override::default());
    let (status, _, _) = call(&world.router, delete(&format!("/{sha}"), Some(auth))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

trait RequestExt {
    fn with_header(self, name: &str, value: String) -> Request<Body>;
    fn tap_range(self, start: u64, end: u64) -> Request<Body>;
    fn tap_suffix(self, n: u64) -> Request<Body>;
    fn tap_multi(self) -> Request<Body>;
}

impl RequestExt for Request<Body> {
    fn with_header(mut self, name: &str, value: String) -> Request<Body> {
        self.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        self
    }
    fn tap_range(self, start: u64, end: u64) -> Request<Body> {
        self.with_header("range", format!("bytes={start}-{end}"))
    }
    fn tap_suffix(self, n: u64) -> Request<Body> {
        self.with_header("range", format!("bytes=-{n}"))
    }
    fn tap_multi(self) -> Request<Body> {
        self.with_header("range", "bytes=0-1,5-6".to_string())
    }
}

// ---------------------------------------------------------------- replay ---

#[tokio::test]
async fn upload_is_idempotent_replay_returns_identical_descriptor() {
    let world = world().await;
    let data = b"replay-me-once-please".to_vec();

    let (s1, b1) = upload_bytes(&world, &data, "application/pdf").await;
    let (s2, b2) = upload_bytes(&world, &data, "application/pdf").await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    // Byte-identical descriptors prove no rewrite happened (uploaded stamp
    // would move on a re-commit path).
    assert_eq!(b1, b2, "replay returns the first commit verbatim");

    // The bucket holds exactly one object for the hash: a rewrite would have
    // violated put-if-absent and failed the whole upload instead.
    let sha = sha_hex(&data);
    assert!(world.bucket.head(&sha).await.unwrap().is_some());
}

#[tokio::test]
async fn concurrent_identical_uploads_converge() {
    let world = world().await;
    let data = b"two-writers-one-hash".to_vec();
    let auth = fx::build_auth_header(SERVER_URL, "upload", &Override::default());
    let req_a = put("/upload", &auth, Some("text/plain"), data.clone());
    let req_b = put("/upload", &auth, Some("text/plain"), data.clone());

    let (ra, rb) = tokio::join!(world.router.clone().oneshot(req_a), world.router.clone().oneshot(req_b));
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    let (sa, sb) = (ra.status(), rb.status());
    let body_a = ra.into_body().collect().await.unwrap().to_bytes();
    let body_b = rb.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(sa, StatusCode::OK);
    assert_eq!(sb, StatusCode::OK);
    assert_eq!(body_a, body_b, "both writers converge on one descriptor");
}

// ------------------------------------------------------------------ auth ---

#[tokio::test]
async fn upload_auth_failures_map_to_typed_statuses() {
    let world = world().await;
    let data = b"x".to_vec();

    struct Case {
        label: &'static str,
        auth: String,
        expect: StatusCode,
    }
    let cases = [
        Case {
            label: "expired",
            auth: fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { expiration_offset_secs: Some(-5), ..Default::default() },
            ),
            expect: StatusCode::UNAUTHORIZED,
        },
        Case {
            label: "wrong verb tag on upload",
            auth: fx::build_auth_header(
                SERVER_URL,
                "delete",
                &Override { t: Some("list".into()), ..Default::default() },
            ),
            expect: StatusCode::FORBIDDEN,
        },
        Case {
            label: "wrong server url",
            auth: fx::build_auth_header(
                "https://elsewhere.example",
                "upload",
                &Override::default(),
            ),
            expect: StatusCode::FORBIDDEN,
        },
        Case {
            label: "bad signature",
            auth: fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { corrupt_sig: true, ..Default::default() },
            ),
            expect: StatusCode::UNAUTHORIZED,
        },
        Case {
            label: "tampered id",
            auth: fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { corrupt_id: true, ..Default::default() },
            ),
            expect: StatusCode::UNAUTHORIZED,
        },
        Case {
            label: "wrong kind",
            auth: fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { kind: Some(27235), ..Default::default() },
            ),
            expect: StatusCode::UNAUTHORIZED,
        },
        Case {
            label: "non-allowlisted npub",
            auth: fx::build_auth_header(
                SERVER_URL,
                "upload",
                &Override { secret: Some(fx::STRANGER_SECRET_HEX), ..Default::default() },
            ),
            expect: StatusCode::FORBIDDEN,
        },
        Case {
            label: "missing auth header",
            auth: String::new(),
            expect: StatusCode::UNAUTHORIZED,
        },
    ];
    for case in cases {
        let req = put("/upload", &case.auth, None, data.clone());
        let (status, _, body) = call(&world.router, req).await;
        assert_eq!(status, case.expect, "{}: body {}", case.label, String::from_utf8_lossy(&body));
    }
}

// ------------------------------------------------------- hash and limits ---

#[tokio::test]
async fn declared_hash_mismatch_is_bad_hash() {
    let world = world().await;
    let auth = fx::build_auth_header(SERVER_URL, "upload", &Override::default());
    let (status, _, body) = call(
        &world.router,
        put(
            &format!("/upload?sha256={}", "a".repeat(64)),
            &auth,
            None,
            b"payload whose hash is not aaaa".to_vec(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("bad_hash"));

    // Header form too.
    let (status, _, _) = call(
        &world.router,
        put("/upload", &auth, None, b"payload whose hash is not aaaa".to_vec())
            .with_header("x-sha256", "a".repeat(64)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oversize_body_is_rejected_with_413() {
    // Fresh world with a small ceiling.
    let dir = tempfile::tempdir().unwrap();
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("BLOBSD_LISTEN".into(), "127.0.0.1:0".into());
    vars.insert("BLOBSD_DATA_DIR".into(), dir.path().display().to_string());
    vars.insert("BLOBSD_PUBLIC_URL".into(), SERVER_URL.into());
    vars.insert("BLOBSD_BUCKET".into(), "fragment-dev".into());
    vars.insert("S3_ENDPOINT".into(), "http://127.0.0.1:9000".into());
    vars.insert("AWS_ACCESS_KEY_ID".into(), "minio".into());
    vars.insert("AWS_SECRET_ACCESS_KEY".into(), "miniosecret".into());
    vars.insert("AWS_REGION".into(), "us-east-1".into());
    vars.insert("BLOBSD_ALLOW_NPUBS".into(), fx::test_npub());
    vars.insert("BLOBSD_INTERNAL_TOKEN".into(), INTERNAL_TOKEN.into());
    vars.insert("BLOBSD_PUBLIC_GET".into(), "true".into());
    vars.insert("BLOBSD_MAX_BLOB_BYTES".into(), "1024".into());
    let cfg = Config::from_map(&vars).unwrap();
    let state = AppState::boot(cfg.clone()).await.unwrap();
    let state = AppState { bucket: Bucket::in_memory(), ..state };
    let router = build_router(state);

    let auth = fx::build_auth_header(SERVER_URL, "upload", &Override::default());
    // Pre-read rejection: declared length already over the ceiling.
    let (status, _, body) = call(
        &router,
        put("/upload", &auth, None, vec![0u8; 2000]),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{}", String::from_utf8_lossy(&body));
}

// ------------------------------------------------------------------ list ---

#[tokio::test]
async fn list_is_self_only_bounded_and_ordered() {
    let world = world().await;
    for i in 0..4u8 {
        let data = format!("list-item-{i}").into_bytes();
        let (status, _) = upload_bytes(&world, &data, "text/plain").await;
        assert_eq!(status, StatusCode::OK);
    }
    let npub = fx::test_npub();

    let auth = fx::build_auth_header(SERVER_URL, "list", &Override::default());
    let (status, _, body) = call(&world.router, get(&format!("/list/{npub}")).with_header("authorization", auth)).await;
    assert_eq!(status, StatusCode::OK);
    let items: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(items.as_array().unwrap().len(), 4);

    // Bounded page.
    let auth = fx::build_auth_header(SERVER_URL, "list", &Override::default());
    let (status, _, body) =
        call(&world.router, get(&format!("/list/{npub}?limit=2")).with_header("authorization", auth)).await;
    assert_eq!(status, StatusCode::OK);
    let items: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(items.as_array().unwrap().len(), 2);

    // Over-cap refused.
    let auth = fx::build_auth_header(SERVER_URL, "list", &Override::default());
    let (status, _, _) =
        call(&world.router, get(&format!("/list/{npub}?limit=501")).with_header("authorization", auth)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Listing as someone else is refused even when authenticated as self.
    let auth = fx::build_auth_header(SERVER_URL, "list", &Override::default());
    let stranger_npub = fx::stranger_npub();
    let (status, _, _) =
        call(&world.router, get(&format!("/list/{stranger_npub}")).with_header("authorization", auth)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // list auth required.
    let (status, _, _) = call(&world.router, get(&format!("/list/{npub}"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------- private reads ---

#[tokio::test]
async fn private_instance_gates_reads_on_internal_bearer() {
    let world = world_with_public_get("false").await;
    let data = b"private-bytes".to_vec();
    let (status, _) = upload_bytes(&world, &data, "text/plain").await;
    assert_eq!(status, StatusCode::OK, "writes unaffected by read gate");
    let sha = sha_hex(&data);

    let (status, _, _) = call(&world.router, get(&format!("/{sha}"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token, no bytes");

    let (status, _, _) = call(
        &world.router,
        get(&format!("/{sha}")).with_header("authorization", "Bearer wrong-token-value-0000".to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "wrong token refused");

    let (status, _, body) = call(
        &world.router,
        get(&format!("/{sha}")).with_header("authorization", format!("Bearer {INTERNAL_TOKEN}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "internal reader passes");
    assert_eq!(&body[..], &data[..]);
}

// ---------------------------------------------------------------- restart ---

#[tokio::test]
async fn descriptors_persist_across_app_restart() {
    let dir = tempfile::tempdir().unwrap();
    let shared_base = {
        let mut vars: HashMap<String, String> = HashMap::new();
        vars.insert("BLOBSD_LISTEN".into(), "127.0.0.1:0".into());
        vars.insert("BLOBSD_DATA_DIR".into(), dir.path().display().to_string());
        vars.insert("BLOBSD_PUBLIC_URL".into(), SERVER_URL.into());
        vars.insert("BLOBSD_BUCKET".into(), "fragment-dev".into());
        vars.insert("S3_ENDPOINT".into(), "http://127.0.0.1:9000".into());
        vars.insert("AWS_ACCESS_KEY_ID".into(), "minio".into());
        vars.insert("AWS_SECRET_ACCESS_KEY".into(), "miniosecret".into());
        vars.insert("AWS_REGION".into(), "us-east-1".into());
        vars.insert("BLOBSD_ALLOW_NPUBS".into(), fx::test_npub());
        vars.insert("BLOBSD_INTERNAL_TOKEN".into(), INTERNAL_TOKEN.into());
        vars.insert("BLOBSD_PUBLIC_GET".into(), "true".into());
        let cfg = Config::from_map(&vars).unwrap();
        AppState::boot(cfg.clone()).await.unwrap()
    };
    // The bucket outlives the "process" (it is the remote); keep one Arc.
    let shared_bucket: Bucket = shared_base.bucket.clone();
    drop(shared_base);

    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("BLOBSD_LISTEN".into(), "127.0.0.1:0".into());
    vars.insert("BLOBSD_DATA_DIR".into(), dir.path().display().to_string());
    vars.insert("BLOBSD_PUBLIC_URL".into(), SERVER_URL.into());
    vars.insert("BLOBSD_BUCKET".into(), "fragment-dev".into());
    vars.insert("S3_ENDPOINT".into(), "http://127.0.0.1:9000".into());
    vars.insert("AWS_ACCESS_KEY_ID".into(), "minio".into());
    vars.insert("AWS_SECRET_ACCESS_KEY".into(), "miniosecret".into());
    vars.insert("AWS_REGION".into(), "us-east-1".into());
    vars.insert("BLOBSD_ALLOW_NPUBS".into(), fx::test_npub());
    vars.insert("BLOBSD_INTERNAL_TOKEN".into(), INTERNAL_TOKEN.into());
    vars.insert("BLOBSD_PUBLIC_GET".into(), "true".into());
    let cfg = Config::from_map(&vars).unwrap();
    let state = AppState::boot(cfg).await.unwrap();
    let state = AppState { bucket: shared_bucket.clone(), ..state };
    let router1 = build_router(state);

    // First "process": upload.
    let data = b"survive-the-restart".to_vec();
    let auth = fx::build_auth_header(SERVER_URL, "upload", &Override::default());
    let (status, _, _) = call(&router1, put("/upload", &auth, Some("text/plain"), data.clone())).await;
    assert_eq!(status, StatusCode::OK);
    drop(router1);

    // Second "process": fresh router over the same db file + same bucket.
    let cfg = {
        let mut vars: HashMap<String, String> = HashMap::new();
        vars.insert("BLOBSD_LISTEN".into(), "127.0.0.1:0".into());
        vars.insert("BLOBSD_DATA_DIR".into(), dir.path().display().to_string());
        vars.insert("BLOBSD_PUBLIC_URL".into(), SERVER_URL.into());
        vars.insert("BLOBSD_BUCKET".into(), "fragment-dev".into());
        vars.insert("S3_ENDPOINT".into(), "http://127.0.0.1:9000".into());
        vars.insert("AWS_ACCESS_KEY_ID".into(), "minio".into());
        vars.insert("AWS_SECRET_ACCESS_KEY".into(), "miniosecret".into());
        vars.insert("AWS_REGION".into(), "us-east-1".into());
        vars.insert("BLOBSD_ALLOW_NPUBS".into(), fx::test_npub());
        vars.insert("BLOBSD_INTERNAL_TOKEN".into(), INTERNAL_TOKEN.into());
        vars.insert("BLOBSD_PUBLIC_GET".into(), "true".into());
        Config::from_map(&vars).unwrap()
    };
    let state = AppState::boot(cfg).await.unwrap();
    let state = AppState { bucket: shared_bucket, ..state };
    let router2 = build_router(state);

    let sha = sha_hex(&data);
    let (status, _, body) = call(&router2, get(&format!("/{sha}"))).await;
    assert_eq!(status, StatusCode::OK, "bytes readable after restart");
    assert_eq!(&body[..], &data[..]);

    let npub = fx::test_npub();
    let auth = fx::build_auth_header(SERVER_URL, "list", &Override::default());
    let (status, _, body) =
        call(&router2, get(&format!("/list/{npub}")).with_header("authorization", auth)).await;
    assert_eq!(status, StatusCode::OK);
    let items: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(items.as_array().unwrap().len(), 1, "descriptor row survived");

    // Replay on the restarted store still returns the identical descriptor.
    let auth = fx::build_auth_header(SERVER_URL, "upload", &Override::default());
    let (status, body2) = {
        let (s, _, b) = call(&router2, put("/upload", &auth, Some("text/plain"), data.clone())).await;
        (s, b)
    };
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body2).contains(&sha), "replay echoes original commit");
}

// ------------------------------------------------------------- corruption ---

#[tokio::test]
async fn corrupt_descriptor_store_fails_boot_closed_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = format!("{}/blobsd.db", dir.path().display());
    // Build a healthy store first.
    {
        let db = blobsd::db::Db::open(&db_path).unwrap();
        db.insert_if_absent(blobsd::db::UploadRow {
            pubkey: "aa11".repeat(16),
            sha256: "bb22".repeat(16),
            size: 1,
            mime: String::new(),
            uploaded_at: 1,
        })
        .await
        .unwrap();
    }
    // Corrupt beyond recognition.
    std::fs::write(&db_path, b"garbage not a database").unwrap();

    let cfg = {
        let mut vars: HashMap<String, String> = HashMap::new();
        vars.insert("BLOBSD_LISTEN".into(), "127.0.0.1:0".into());
        vars.insert("BLOBSD_DATA_DIR".into(), dir.path().display().to_string());
        vars.insert("BLOBSD_PUBLIC_URL".into(), SERVER_URL.into());
        vars.insert("BLOBSD_BUCKET".into(), "fragment-dev".into());
        vars.insert("S3_ENDPOINT".into(), "http://127.0.0.1:9000".into());
        vars.insert("AWS_ACCESS_KEY_ID".into(), "minio".into());
        vars.insert("AWS_SECRET_ACCESS_KEY".into(), "miniosecret".into());
        vars.insert("AWS_REGION".into(), "us-east-1".into());
        vars.insert("BLOBSD_ALLOW_NPUBS".into(), fx::test_npub());
        vars.insert("BLOBSD_INTERNAL_TOKEN".into(), INTERNAL_TOKEN.into());
        vars.insert("BLOBSD_PUBLIC_GET".into(), "true".into());
        Config::from_map(&vars).unwrap()
    };
    let outcome = AppState::boot(cfg).await;
    match outcome {
        Err(BootError::CorruptState(reason)) => {
            assert!(reason.contains("quick_check"), "reason names the probe: {reason}");
        }
        Ok(_) => panic!("boot must fail closed on a corrupt descriptor store"),
        Err(other) => panic!("expected CorruptState, got {other}"),
    }

    // And the request-path mapping renders the typed 500, not a panic:
    let response = blobsd::error::ApiError::CorruptState.into_response();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("corrupt_state"));
}

// ----------------------------------------------------------------- health ---

#[tokio::test]
async fn health_names_the_server() {
    let world = world().await;
    let (status, _, body) = call(&world.router, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["name"], "blobsd");
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
}
