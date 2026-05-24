//! xaft — autonomous coding agent binary entry point.

use std::sync::Arc;

use xaft_config::CliOverrides;
use xaft_config::loader::ConfigLoader;
use xaft_runtime::runtime::XaftRuntime;

#[tokio::main]
async fn main() {
    let config = ConfigLoader::load(&CliOverrides::default()).unwrap_or_else(|e| {
        eprintln!("xaft: configuration error: {e}");
        std::process::exit(3);
    });

    let runtime = XaftRuntime::bootstrap(config).await.unwrap_or_else(|e| {
        eprintln!("xaft: failed to start runtime: {e}");
        std::process::exit(1);
    });

    xaft_cli::run(Arc::new(runtime)).await;
}
