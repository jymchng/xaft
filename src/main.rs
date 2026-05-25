//! xaft — autonomous coding agent binary entry point.

use std::sync::Arc;

use xaft_config::CliOverrides;
use xaft_config::loader::ConfigLoader;
use xaft_runtime::runtime::XaftRuntime;
use xaft_session::SessionManager;

#[tokio::main]
async fn main() {
    let config = ConfigLoader::load(&CliOverrides::default()).unwrap_or_else(|e| {
        eprintln!("xaft: configuration error: {e}");
        std::process::exit(3);
    });

    // Bootstrap SQLite session persistence. Falls back to FsSessionStore if
    // the database can't be opened (permissions, disk full, etc.).
    let runtime = bootstrap_runtime(config).await.unwrap_or_else(|e| {
        eprintln!("xaft: failed to start runtime: {e}");
        std::process::exit(1);
    });

    xaft_cli::run(Arc::new(runtime)).await;
}

async fn bootstrap_runtime(
    config: xaft_config::XaftConfig,
) -> Result<XaftRuntime, xaft_runtime::RuntimeError> {
    let runtime = XaftRuntime::bootstrap(config.clone()).await?;

    // Upgrade to SQLite session store if possible
    match SessionManager::new(&config.core.data_dir).await {
        Ok(mgr) => {
            tracing::info!("xaft: SQLite session store active");
            Ok(runtime.with_stores(mgr.session_store(), mgr.conversation_store()))
        }
        Err(e) => {
            tracing::warn!(error = %e, "xaft: SQLite session store unavailable, using FsSessionStore");
            Ok(runtime)
        }
    }
}
