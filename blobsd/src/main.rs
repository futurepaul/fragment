//! blobsd — the blob tier's server. Boot validates configuration, opens the
//! descriptor store (fail-closed on corruption), binds the bucket client and
//! serves. Any boot failure prints one reason to stderr and exits non-zero.

use std::process::ExitCode;

use blobsd::{app, config, error};

fn main() -> ExitCode {
    // tracing init first: every later failure should be observable too.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // init() installs the global default; finish() alone builds and drops it.
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .init();

    let cfg = match config::Config::from_env() {
        Ok(cfg) => cfg,
        Err(err) => return fail(err),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime constructs unconditionally");

    let (state, listen_addr) = match runtime.block_on(app::AppState::boot(cfg)) {
        Ok(state) => {
            let addr = state.cfg.listen;
            (state, addr)
        }
        Err(err) => return fail(err),
    };
    let router = app::build_router(state);
    tracing::info!(addr = %listen_addr, "blobsd listening");

    let serve_result = runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(listen_addr)
            .await
            .map_err(|e| error::BootError::Config(format!("listen {listen_addr}: {e}")))?;
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| error::BootError::Config(format!("serve: {e}")))
    });
    if let Err(err) = serve_result {
        return fail(err);
    }

    tracing::info!("blobsd stopped");
    ExitCode::SUCCESS
}

/// One-line human failure, non-zero exit: boot errors are operator-facing.
fn fail(err: error::BootError) -> ExitCode {
    eprintln!("blobsd: {err}");
    if matches!(err, error::BootError::CorruptState(_)) {
        eprintln!(
            "blobsd: descriptor store is corrupt — nothing was written; \
             restore data/blobsd.db or start from an empty data dir"
        );
    }
    ExitCode::FAILURE
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                term.recv().await;
            }
            // Signal registration failing is not fatal: ctrl-c still works
            // and the unit file drives the lifecycle anyway.
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
