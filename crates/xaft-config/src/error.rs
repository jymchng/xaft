//! Configuration error types.

use std::path::PathBuf;

/// All errors that can occur during configuration loading and validation.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// I/O error reading a config file.
    #[error("I/O error reading {path}: {source}")]
    Io {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// TOML parse error in a config file.
    #[error("parse error in {path}: {source}")]
    Parse {
        /// The file that failed to parse.
        path: PathBuf,
        /// The TOML parse error.
        source: toml::de::Error,
    },

    /// Serialization error during config merging.
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),

    /// Validation error with section and message.
    #[error("validation error in [{section}]: {message}")]
    Validation {
        /// The config section that failed validation.
        section: String,
        /// Human-readable validation message.
        message: String,
    },

    /// Referenced agent preset does not exist.
    #[error("unknown agent preset: '{name}'")]
    UnknownPreset {
        /// The preset name that was requested.
        name: String,
    },

    /// Referenced provider does not exist.
    #[error("unknown provider: '{name}'")]
    UnknownProvider {
        /// The provider name that was referenced.
        name: String,
    },

    /// API key is missing for a provider.
    #[error(
        "missing API key for provider '{provider}': set {env_var} or provider.{provider}.api_key"
    )]
    MissingApiKey {
        /// The provider missing its key.
        provider: String,
        /// The environment variable name to set.
        env_var: String,
    },

    /// Tool config deserialization error.
    #[error("tool config error for '{tool}': {source}")]
    ToolConfig {
        /// The tool name.
        tool: String,
        /// The deserialization error.
        source: serde_json::Error,
    },

    /// Environment variable parse error.
    #[error("environment variable parse error: {var} expected {expected}")]
    EnvParse {
        /// The variable name.
        var: String,
        /// The expected type.
        expected: &'static str,
    },

    /// Invalid human-readable size string (e.g. "10XB").
    #[error("invalid size string: '{0}' (expected format: '10MB', '1GB', '512KB', '1024B')")]
    InvalidSize(String),

    /// Key binding parse error.
    #[error("invalid key binding '{key}': {reason}")]
    KeyParse {
        /// The key string that failed to parse.
        key: String,
        /// Why it failed.
        reason: String,
    },

    /// Config file already exists and `--force` was not given.
    #[error("config file already exists at {path}")]
    AlreadyExists {
        /// The conflicting path.
        path: PathBuf,
    },

    /// Current directory could not be determined.
    #[error("could not determine current directory: {0}")]
    CurrentDir(std::io::Error),
}

impl ConfigError {
    /// Create a validation error.
    pub fn validation(section: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation {
            section: section.into(),
            message: message.into(),
        }
    }
}
