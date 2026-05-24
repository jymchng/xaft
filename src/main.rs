//! xaft — autonomous coding agent binary entry point.

use std::sync::Arc;

use xaft_runtime::dispatch::StubRuntime;

#[tokio::main]
async fn main() {
    let runtime = Arc::new(StubRuntime);
    xaft_cli::run(runtime).await;
}
