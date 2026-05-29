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
    let runtime = match SessionManager::new(&config.core.data_dir).await {
        Ok(mgr) => {
            tracing::info!("xaft: SQLite session store active");
            runtime.with_stores(mgr.session_store(), mgr.conversation_store())
        }
        Err(e) => {
            tracing::warn!(error = %e, "xaft: SQLite session store unavailable, using FsSessionStore");
            runtime
        }
    };

    // Bootstrap memory system if enabled
    let runtime = if config.memory.enabled {
        let backend = match config.memory.backend.as_str() {
            "in_memory" => xaft_memory::config::MemoryBackend::InMemory,
            _ => xaft_memory::config::MemoryBackend::Sqlite,
        };
        let mem_config = xaft_memory::MemoryConfig {
            enabled: config.memory.enabled,
            backend: backend.clone(),
            auto_remember: config.memory.auto_remember,
            auto_summarize: config.memory.auto_summarize,
            project_scope_default: config.memory.project_scope_default,
            max_entries: config.memory.max_entries,
            auto_remember_ttl_secs: None,
            max_search_results: config.memory.max_search_results,
        };
        let workspace_id = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "default".into());

        match xaft_memory::XaftMemoryManager::new(
            backend,
            mem_config,
            workspace_id,
            Some(runtime.signals().clone()),
        )
        .await
        {
            Ok(mgr) => {
                tracing::info!("xaft: memory system active");
                runtime.with_memory(Arc::new(mgr))
            }
            Err(e) => {
                tracing::warn!(error = %e, "xaft: memory system unavailable");
                runtime
            }
        }
    } else {
        tracing::info!("xaft: memory system disabled");
        runtime
    };

    Ok(runtime)
}
