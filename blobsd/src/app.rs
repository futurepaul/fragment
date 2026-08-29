//! Application assembly: validated config -> opened store and bucket ->
//! router. Shared by `main` and every integration test so tests exercise the
//! production wiring with only the inputs swapped.

use std::sync::Arc;

use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use crate::bucket::Bucket;
use crate::config::Config;
use crate::db::Db;
use crate::error::BootError;
use crate::{handlers, logging};

/// Cap on simultaneous object-publishes. Each publish holds one transient
/// in-memory copy of the staged bytes (object_store's CAS payload is
/// buffered), so worst-case publish RSS is bounded at
/// MAX_CONCURRENT_PUBLISHES x BLOBSD_MAX_BLOB_BYTES.
pub const MAX_CONCURRENT_PUBLISHES: usize = 4;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub db: Db,
    pub bucket: Bucket,
    pub publish_permits: Arc<tokio::sync::Semaphore>,
}

impl AppState {
    /// Open all durable resources for one configured instance. Fails closed
    /// on corrupt descriptors via [`BootError::CorruptState`].
    pub async fn boot(cfg: Config) -> Result<AppState, BootError> {
        std::fs::create_dir_all(&cfg.data_dir)
            .map_err(|e| BootError::DataDir(format!("{}: {e}", cfg.data_dir)))?;
        let staging = cfg.staging_dir();
        std::fs::create_dir_all(&staging)
            .map_err(|e| BootError::DataDir(format!("{staging}: {e}")))?;

        let db_path = format!("{}/blobsd.db", cfg.data_dir);
        let db = Db::open(&db_path).map_err(|err| match err {
            crate::error::StoreError::CorruptState => BootError::CorruptState(
                "quick_check rejected the descriptor store; refusing to serve".to_string(),
            ),
            other => BootError::Db(other.to_string()),
        })?;

        let bucket = Bucket::open(&cfg)?;

        Ok(AppState {
            cfg,
            db,
            bucket,
            publish_permits: Arc::new(tokio::sync::Semaphore::new(
                MAX_CONCURRENT_PUBLISHES,
            )),
        })
    }
}

/// Public blob GETs are unauthenticated by design, and pages fetch them via
/// cross-origin redirects from fragment hosts; mark every response readable
/// from any origin. Writes stay bearer-gated — browsers cannot send an
/// Authorization header cross-origin without a preflight, and we answer no
/// preflights, so this widens reads only.
async fn cors(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if req.method() == http::Method::OPTIONS {
        return axum::http::StatusCode::NO_CONTENT.into_response();
    }
    let mut res = next.run(req).await;
    res.headers_mut().insert(
        http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
        axum::http::HeaderValue::from_static("*"),
    );
    res
}

/// The entire HTTP surface plus the request-log middleware.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(handlers::health))
        .route("/upload", axum::routing::put(handlers::upload))
        .route(
            "/:sha",
            get(handlers::get_blob).delete(handlers::delete_blob),
        )
        .route("/list/:npub", get(handlers::list_blobs))
        // Upload bodies are enforced by handler staging logic against
        // BLOBSD_MAX_BLOB_BYTES with Content-Length checked pre-read; this
        // guards against runaway buffering inside the framework itself.
        .layer(axum::extract::DefaultBodyLimit::max(
            usize::try_from(state.cfg.max_blob_bytes).unwrap_or(usize::MAX),
        ))
        .layer(axum::middleware::from_fn(cors))
        .layer(axum::middleware::from_fn(logging::track))
        .with_state(state)
}
