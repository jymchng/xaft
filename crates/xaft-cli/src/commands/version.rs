//! Handler for `xaft version`.

use xaft_runtime::ExitCode;

use crate::args::VersionArgs;
use crate::error::XaftError;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PKG_NAME: &str = env!("CARGO_PKG_NAME");

/// Version information.
#[derive(Debug, serde::Serialize)]
pub struct VersionInfo {
    /// Package version.
    pub version: String,
    /// Build date (if available).
    pub build_date: Option<String>,
    /// Git commit (if available).
    pub git_commit: Option<String>,
    /// Rust edition.
    pub rust_edition: String,
}

impl VersionInfo {
    /// Gather version information.
    pub fn gather() -> Self {
        Self {
            version: VERSION.to_string(),
            build_date: option_env!("XAFT_BUILD_DATE").map(|s| s.to_string()),
            git_commit: option_env!("XAFT_GIT_COMMIT").map(|s| s.to_string()),
            rust_edition: "2024".to_string(),
        }
    }
}

/// Execute `xaft version`.
pub async fn handle_version(args: &VersionArgs) -> Result<ExitCode, XaftError> {
    let info = VersionInfo::gather();

    if args.json {
        println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
    } else {
        println!("{PKG_NAME} {}", info.version);
        if let Some(ref date) = info.build_date {
            println!("  built:  {date}");
        }
        if let Some(ref commit) = info.git_commit {
            println!("  commit: {commit}");
        }
    }

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn version_pretty() {
        let args = VersionArgs { json: false };
        let code = handle_version(&args).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn version_json() {
        let args = VersionArgs { json: true };
        let code = handle_version(&args).await.unwrap();
        assert!(code.is_success());
    }

    #[test]
    fn version_info_has_version() {
        let info = VersionInfo::gather();
        assert!(!info.version.is_empty());
    }
}
