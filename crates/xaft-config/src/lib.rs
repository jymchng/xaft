//! `xaft-config` — Configuration loading, validation, and hot-reload for xaft.
//!
//! # Overview
//!
//! Configuration is loaded from multiple sources in precedence order:
//!
//! ```text
//! Priority    Source
//! ────────    ──────────────────────────────────────────
//! 1 (highest) CLI flags
//! 2           Environment variables (XAFT_*)
//! 3           Session overrides (~/.xaft/sessions/<id>/)
//! 4           Project config (.xaft/xaft.toml)
//! 5           User global config (~/.config/xaft/xaft.toml)
//! 6 (lowest)  Built-in defaults
//! ```
//!
//! # Quick start
//!
//! ```rust,no_run
//! use xaft_config::{ConfigLoader, CliOverrides};
//!
//! let overrides = CliOverrides::default();
//! let config = ConfigLoader::load(&overrides).expect("failed to load config");
//!
//! println!("log level: {}", config.core.log_level);
//! println!("default model: {}", config.agent["default"].model);
//! ```
//!
//! # Hot-reload
//!
//! ```rust,no_run
//! use std::time::Duration;
//! use xaft_config::{ConfigLoader, CliOverrides, watcher::{ConfigWatcher, watched_paths}};
//!
//! let overrides = CliOverrides::default();
//! let initial = ConfigLoader::load(&overrides).unwrap();
//! let paths = watched_paths(&overrides);
//!
//! let (mut rx, _handle) = ConfigWatcher::spawn(initial, paths, overrides, Duration::from_secs(2));
//!
//! // In your application loop:
//! // if rx.changed().await.is_ok() {
//! //     let new_config = rx.borrow().clone();
//! // }
//! ```

#![deny(missing_docs)]

pub mod defaults;
pub mod error;
pub mod interpolate;
pub mod keybinding;
pub mod loader;
pub mod merge;
pub mod preset;
pub mod size;
pub mod tui_layout;
pub mod types;
pub mod validate;
pub mod watcher;

// ── Flat re-exports ───────────────────────────────────────────────────────────

pub use error::ConfigError;
pub use keybinding::{KeyCode, KeyModifiers, KeybindingParser, KeybindingRegistry, ParsedKeyEvent};
pub use loader::ConfigLoader;
pub use preset::AgentPresetResolver;
pub use size::parse_size;
pub use tui_layout::{TuiLayoutPersistence, spawn_layout_saver};
pub use types::{
    AgentPreset, CliOverrides, CoreConfig, FileEditToolConfig, FileReadToolConfig,
    FocusedPanel, GrepToolConfig, GuardrailConfig, KeyAction, KeybindingConfig,
    LogLevel, McpClientConfig, McpConfig, McpServerConfig, PluginConfig, ProviderConfig,
    ProviderType, ResolvedAgentPreset, SecretAction, ShellToolConfig, SidebarPanel,
    ToolConfig, TuiConfig, TuiLayoutConfig, TuiLayoutState, TuiTheme, XaftConfig,
    glob_matches,
};
pub use validate::validate;
pub use watcher::{ConfigWatcher, watched_paths};
