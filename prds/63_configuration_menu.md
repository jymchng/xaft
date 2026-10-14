---
title: "PRD-63: Configuration Menu — Live Editing of XaftConfig via /config"
status: draft
version: 0.1.0
created: 2026-06-13
depends-on: 61_menu_widget_system.md, 62_command_menu_dispatch.md
---

# PRD-63: Configuration Menu — Live Editing of XaftConfig via /config

## 1. Problem

PRD 55 reworked `/config` into a read-only display that commits formatted
`StyledLine`s into the scrollback transcript. It deliberately deferred the
interactive editing problem. The result is that to change a setting a user must:

1. Type `/config` to find the key path
2. Type `/config set core.log_level debug` blindly
3. Repeat `/config` to verify the change took effect

There is no way to browse, compare to defaults, or edit atomically without
leaving the transcript cluttered with repetitive `/config` output.

PRD 61 introduces `MenuWidget` — a trait for full-screen overlay panels driven
by a `MenuDriver` event loop that captures all keypresses while active, renders
into a fixed-height region above the input bar, and returns a `MenuResult` on
close. PRD 62 introduces `CommandMenuRegistry` and the `CommandResult::OpenMenu`
variant, allowing any slash command to open a `MenuWidget` instead of appending
transcript lines.

PRD 63 delivers the `ConfigurationMenu`: the concrete `MenuWidget` implementation
that `/config` opens when called without arguments, providing interactive
navigation, live editing, type-validated field mutation, and save-to-disk.

---

## 2. Goals

1. `/config` (no args) opens a full-screen `ConfigurationMenu` overlay instead of
   printing the read-only table.
2. The menu shows all `XaftConfig` fields grouped into collapsible sections, with
   current values, default values, and visual change indicators.
3. Users navigate with arrow keys, toggle booleans with `Enter`, edit strings and
   integers inline, and save to the project `.xaft.toml` with `s`.
4. On save, the snapshot config is written to disk and a `XaftConfigPatched`
   signal is broadcast so the running agent picks up the change on its next turn.
5. All changes are validated by type before being committed to the snapshot.
6. Redacted fields (`api_key`, raw key material) are never editable through the
   menu.

---

## 3. Non-Goals

- Editing `HashMap` / `Vec` fields inline (they display as read-only `Complex`
  entries; structural editing is out of scope).
- Multi-level undo/redo.
- Diff view (changed vs. saved on disk).
- Syncing hot-reload changes back into the menu while it is open.
- Supporting more than one open menu at a time (menus are exclusive).

---

## 4. Background: the Rust reflection problem

Python config editors (like the agenthicc PRD-43 inspiration) can iterate a
dataclass at runtime, discovering field names and types via `__annotations__`.
Rust has no equivalent. The `serde` machinery can serialize `XaftConfig` to a
`toml::Value` tree, which gives string keys and values, but:

- It loses type information (we cannot distinguish `u32` from `u8` from `f32`
  in the serialized form without extra metadata).
- It cannot distinguish "API key field — must be redacted" from an ordinary
  string without domain knowledge.
- TOML `HashMap<String, T>` sections (like `agent` and `provider`) produce
  dynamic keys we cannot enumerate statically.

The solution is a hand-built `build_sections` function that directly reads the
known fields of `XaftConfig` and its sub-structs. This is verbose but:

- Completely type-safe.
- Zero runtime overhead.
- Easy to audit for security (redacted fields are explicitly marked).
- Easy to extend (add a new field → add one `ConfigRow` entry).

The tradeoff is maintenance cost: when a new field is added to `XaftConfig`, an
engineer must also add a `ConfigRow` to `build_sections`. Tests verify coverage
of the most important fields.

---

## 5. Core types

All types live in `crates/xaft-tui/src/menu/config_menu.rs`.

### 5.1 `ConfigValue`

```rust
/// The typed value of a single config field, as the menu represents it.
///
/// `Redacted` and `Complex` fields are always read-only; the menu never opens
/// edit mode for them.  `Bool` toggles on Enter without entering edit mode.
/// `Str`, `Int`, and `Float` enter inline edit mode on Enter.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValue {
    /// UTF-8 string field.
    Str(String),
    /// Signed 64-bit integer (covers u8, u16, u32, u64, i64, usize).
    Int(i64),
    /// 64-bit float (covers f32, f64).
    Float(f64),
    /// Boolean field — toggled directly on Enter.
    Bool(bool),
    /// Sensitive field (api_key, raw key material).  Displayed as `***` and
    /// never opened for editing.
    Redacted,
    /// Struct, map, vec, or other compound type.  Displayed as a summary and
    /// never opened for editing.
    Complex,
}

impl ConfigValue {
    /// Return a display string suitable for the menu value column.
    pub fn display(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            Self::Int(n) => n.to_string(),
            Self::Float(f) => format!("{f:.4}"),
            Self::Bool(b) => b.to_string(),
            Self::Redacted => "***".to_string(),
            Self::Complex => "[ … ]".to_string(),
        }
    }

    /// Return `true` when the value can be opened for inline editing.
    pub fn is_editable(&self) -> bool {
        matches!(self, Self::Str(_) | Self::Int(_) | Self::Float(_))
    }

    /// Return `true` when Enter should toggle this value directly.
    pub fn is_toggle(&self) -> bool {
        matches!(self, Self::Bool(_))
    }
}
```

### 5.2 `ConfigRow`

```rust
/// One editable (or read-only) row within a config section.
pub struct ConfigRow {
    /// Dotted TOML key path from the root, e.g. `"agent.default.max_turns"`.
    /// Used as the key when emitting `XaftConfigPatched` and when writing TOML.
    pub path: String,
    /// Human-readable label shown in the key column, e.g. `"max turns"`.
    pub label: String,
    /// Current value in the live snapshot.
    pub value: ConfigValue,
    /// Compiled-in default value for comparison / reset.
    pub default: ConfigValue,
    /// `true` if this field may be edited through the menu.
    /// `false` for `Redacted` and `Complex` variants, plus any field that is
    /// intentionally locked (e.g. `data_dir` while a session is active).
    pub editable: bool,
}

impl ConfigRow {
    /// Return `true` when `value != default` (indicates a change from default).
    pub fn is_changed(&self) -> bool {
        self.value != self.default
    }
}
```

### 5.3 `ConfigSection`

```rust
/// A named, collapsible group of `ConfigRow`s in the menu.
pub struct ConfigSection {
    /// Section name shown in the header, e.g. `"provider"` or `"agent.default"`.
    pub name: String,
    /// Rows belonging to this section.
    pub rows: Vec<ConfigRow>,
    /// Whether the section is currently expanded in the menu.
    pub expanded: bool,
}
```

### 5.4 `MenuState`

```rust
/// The interaction mode of `ConfigurationMenu`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuState {
    /// Normal navigation: cursor moves, Enter acts on the selected row.
    Navigate,
    /// Inline edit mode: keyboard input goes to `edit_buf`; Enter commits,
    /// Esc cancels.
    Edit,
}
```

### 5.5 `ConfigurationMenu`

```rust
use std::sync::Arc;
use xaft_config::XaftConfig;

pub struct ConfigurationMenu {
    /// Shared reference to the current live config (read-only for display).
    /// The menu works on a mutable snapshot clone; this field is only used to
    /// reload the snapshot on open, not written to.
    config_ref: Arc<XaftConfig>,

    /// Mutable working copy.  Changes accumulate here until `s` saves them.
    snapshot: XaftConfig,

    /// Sections built from `snapshot`.  Rebuilt after each committed edit.
    sections: Vec<ConfigSection>,

    /// Cursor position: `(section_idx, row_idx)`.
    /// `row_idx == -1` means the section header itself is focused.
    cursor: (usize, i32),

    /// Interaction state.
    state: MenuState,

    /// Buffer for the current inline edit.  Cleared when edit mode is entered/exited.
    edit_buf: String,

    /// Status / error message shown in the bottom bar.
    status_msg: String,

    /// First visible logical row index (for scrolling).
    scroll_offset: usize,

    /// Number of rows visible in the allocated render area (set on `render()`).
    max_visible: usize,

    /// The project working directory — used to resolve where `.xaft.toml` lives.
    working_dir: std::path::PathBuf,

    /// Signal bus reference — used to emit `XaftConfigPatched` on save.
    signals: Arc<agtrs_runtime::signals::SignalBus>,
}
```

---

## 6. `build_sections` — manual field enumeration

`build_sections` is the authoritative mapping of `XaftConfig` fields to
`ConfigRow`s. It is called once when the menu opens, and again after every
committed edit (to rebuild `sections` from the updated snapshot).

```rust
/// Build the full ordered list of sections from a config snapshot.
///
/// Sections are always emitted in the fixed order:
///   core → agent.default → agent.* (other presets) → provider.* →
///   guardrail → compaction → memory → tui → mention → workflow → model_tiers
///
/// Fields of type `HashMap<String, T>` or `Vec<T>` that are too complex to
/// edit inline are given `ConfigValue::Complex` and `editable: false`.
pub fn build_sections(config: &XaftConfig) -> Vec<ConfigSection> {
    let defaults = XaftConfig::default();
    let mut sections = Vec::new();

    // ── core ──────────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "core".into(),
        expanded: true,
        rows: vec![
            ConfigRow {
                path: "core.log_level".into(),
                label: "log level".into(),
                value: ConfigValue::Str(config.core.log_level.to_string()),
                default: ConfigValue::Str(defaults.core.log_level.to_string()),
                editable: true,
            },
            ConfigRow {
                path: "core.data_dir".into(),
                label: "data dir".into(),
                value: ConfigValue::Str(config.core.data_dir.display().to_string()),
                default: ConfigValue::Str(defaults.core.data_dir.display().to_string()),
                editable: true,
            },
            ConfigRow {
                path: "core.telemetry".into(),
                label: "telemetry".into(),
                value: ConfigValue::Bool(config.core.telemetry),
                default: ConfigValue::Bool(defaults.core.telemetry),
                editable: true,
            },
            ConfigRow {
                path: "core.agents_md_enabled".into(),
                label: "agents.md auto-load".into(),
                value: ConfigValue::Bool(config.core.agents_md_enabled),
                default: ConfigValue::Bool(defaults.core.agents_md_enabled),
                editable: true,
            },
            ConfigRow {
                path: "core.agents_md_max_bytes".into(),
                label: "agents.md max bytes".into(),
                value: ConfigValue::Int(config.core.agents_md_max_bytes as i64),
                default: ConfigValue::Int(defaults.core.agents_md_max_bytes as i64),
                editable: true,
            },
        ],
    });

    // ── agent.default ─────────────────────────────────────────────────────────
    // The "default" preset is always present; other presets are added below.
    if let Some(preset) = config.agent.get("default") {
        let def_preset = defaults.agent.get("default").cloned().unwrap_or_default();
        sections.push(ConfigSection {
            name: "agent.default".into(),
            expanded: true,
            rows: build_agent_preset_rows("agent.default", preset, &def_preset),
        });
    }

    // ── other agent presets ───────────────────────────────────────────────────
    let mut other_presets: Vec<&str> = config
        .agent
        .keys()
        .filter(|k| k.as_str() != "default")
        .map(|k| k.as_str())
        .collect();
    other_presets.sort_unstable();
    for preset_name in other_presets {
        if let Some(preset) = config.agent.get(preset_name) {
            let path_prefix = format!("agent.{preset_name}");
            let def_preset = defaults
                .agent
                .get(preset_name)
                .cloned()
                .unwrap_or_default();
            sections.push(ConfigSection {
                name: path_prefix.clone(),
                expanded: false, // non-default presets collapsed by default
                rows: build_agent_preset_rows(&path_prefix, preset, &def_preset),
            });
        }
    }

    // ── provider sections ─────────────────────────────────────────────────────
    let mut provider_names: Vec<&str> = config.provider.keys().map(|k| k.as_str()).collect();
    provider_names.sort_unstable();
    for prov_name in provider_names {
        if let Some(prov) = config.provider.get(prov_name) {
            let path_prefix = format!("provider.{prov_name}");
            let def_prov = defaults.provider.get(prov_name).cloned().unwrap_or_default();
            sections.push(ConfigSection {
                name: path_prefix.clone(),
                expanded: false,
                rows: build_provider_rows(&path_prefix, prov, &def_prov),
            });
        }
    }

    // ── guardrail ─────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "guardrail".into(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "guardrail.file_destruction".into(),
                label: "block file destruction".into(),
                value: ConfigValue::Bool(config.guardrail.file_destruction),
                default: ConfigValue::Bool(defaults.guardrail.file_destruction),
                editable: true,
            },
            ConfigRow {
                path: "guardrail.secret_leakage".into(),
                label: "detect secret leakage".into(),
                value: ConfigValue::Bool(config.guardrail.secret_leakage),
                default: ConfigValue::Bool(defaults.guardrail.secret_leakage),
                editable: true,
            },
            ConfigRow {
                path: "guardrail.cost_limit".into(),
                label: "enforce cost limit".into(),
                value: ConfigValue::Bool(config.guardrail.cost_limit),
                default: ConfigValue::Bool(defaults.guardrail.cost_limit),
                editable: true,
            },
            ConfigRow {
                path: "guardrail.command_approval".into(),
                label: "require command approval".into(),
                value: ConfigValue::Bool(config.guardrail.command_approval),
                default: ConfigValue::Bool(defaults.guardrail.command_approval),
                editable: true,
            },
            ConfigRow {
                path: "guardrail.cost_limit_config.max_spend".into(),
                label: "max spend (USD)".into(),
                value: ConfigValue::Float(config.guardrail.cost_limit_config.max_spend),
                default: ConfigValue::Float(defaults.guardrail.cost_limit_config.max_spend),
                editable: true,
            },
            ConfigRow {
                path: "guardrail.cost_limit_config.max_tokens_per_request".into(),
                label: "max tokens/request".into(),
                value: ConfigValue::Int(
                    config.guardrail.cost_limit_config.max_tokens_per_request as i64,
                ),
                default: ConfigValue::Int(
                    defaults.guardrail.cost_limit_config.max_tokens_per_request as i64,
                ),
                editable: true,
            },
            ConfigRow {
                path: "guardrail.cost_limit_config.warn_at_percent".into(),
                label: "warn at % of limit".into(),
                value: ConfigValue::Int(
                    config.guardrail.cost_limit_config.warn_at_percent as i64,
                ),
                default: ConfigValue::Int(
                    defaults.guardrail.cost_limit_config.warn_at_percent as i64,
                ),
                editable: true,
            },
        ],
    });

    // ── compaction ────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "compaction".into(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "compaction.enabled".into(),
                label: "auto compaction".into(),
                value: ConfigValue::Bool(config.compaction.enabled),
                default: ConfigValue::Bool(defaults.compaction.enabled),
                editable: true,
            },
            ConfigRow {
                path: "compaction.threshold_pct".into(),
                label: "threshold %".into(),
                value: ConfigValue::Int(config.compaction.threshold_pct as i64),
                default: ConfigValue::Int(defaults.compaction.threshold_pct as i64),
                editable: true,
            },
            ConfigRow {
                path: "compaction.keep_recent_turns".into(),
                label: "keep recent turns".into(),
                value: ConfigValue::Int(config.compaction.keep_recent_turns as i64),
                default: ConfigValue::Int(defaults.compaction.keep_recent_turns as i64),
                editable: true,
            },
            ConfigRow {
                path: "compaction.summary_max_tokens".into(),
                label: "summary max tokens".into(),
                value: ConfigValue::Int(config.compaction.summary_max_tokens as i64),
                default: ConfigValue::Int(defaults.compaction.summary_max_tokens as i64),
                editable: true,
            },
        ],
    });

    // ── memory ────────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "memory".into(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "memory.enabled".into(),
                label: "enabled".into(),
                value: ConfigValue::Bool(config.memory.enabled),
                default: ConfigValue::Bool(defaults.memory.enabled),
                editable: true,
            },
            ConfigRow {
                path: "memory.auto_remember".into(),
                label: "auto remember".into(),
                value: ConfigValue::Bool(config.memory.auto_remember),
                default: ConfigValue::Bool(defaults.memory.auto_remember),
                editable: true,
            },
            ConfigRow {
                path: "memory.auto_summarize".into(),
                label: "auto summarize".into(),
                value: ConfigValue::Bool(config.memory.auto_summarize),
                default: ConfigValue::Bool(defaults.memory.auto_summarize),
                editable: true,
            },
            ConfigRow {
                path: "memory.max_search_results".into(),
                label: "max search results".into(),
                value: ConfigValue::Int(config.memory.max_search_results as i64),
                default: ConfigValue::Int(defaults.memory.max_search_results as i64),
                editable: true,
            },
        ],
    });

    // ── tui ───────────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "tui".into(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "tui.theme".into(),
                label: "theme".into(),
                value: ConfigValue::Str(format!("{:?}", config.tui.theme).to_lowercase()),
                default: ConfigValue::Str(format!("{:?}", defaults.tui.theme).to_lowercase()),
                editable: true,
            },
            ConfigRow {
                path: "tui.mouse".into(),
                label: "mouse support".into(),
                value: ConfigValue::Bool(config.tui.mouse),
                default: ConfigValue::Bool(defaults.tui.mouse),
                editable: true,
            },
            ConfigRow {
                path: "tui.timestamps".into(),
                label: "timestamps".into(),
                value: ConfigValue::Bool(config.tui.timestamps),
                default: ConfigValue::Bool(defaults.tui.timestamps),
                editable: true,
            },
            ConfigRow {
                path: "tui.render_markdown".into(),
                label: "render markdown".into(),
                value: ConfigValue::Bool(config.tui.render_markdown),
                default: ConfigValue::Bool(defaults.tui.render_markdown),
                editable: true,
            },
            ConfigRow {
                path: "tui.max_background_tasks".into(),
                label: "max background tasks".into(),
                value: ConfigValue::Int(config.tui.max_background_tasks as i64),
                default: ConfigValue::Int(defaults.tui.max_background_tasks as i64),
                editable: true,
            },
            ConfigRow {
                path: "tui.show_exit_summary".into(),
                label: "show exit summary".into(),
                value: ConfigValue::Bool(config.tui.show_exit_summary),
                default: ConfigValue::Bool(defaults.tui.show_exit_summary),
                editable: true,
            },
            ConfigRow {
                path: "tui.preserve_output_on_exit".into(),
                label: "preserve output on exit".into(),
                value: ConfigValue::Bool(config.tui.preserve_output_on_exit),
                default: ConfigValue::Bool(defaults.tui.preserve_output_on_exit),
                editable: true,
            },
        ],
    });

    // ── mention ───────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "mention".into(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "mention.max_inline_lines".into(),
                label: "max inline lines".into(),
                value: ConfigValue::Int(config.mention.max_inline_lines as i64),
                default: ConfigValue::Int(defaults.mention.max_inline_lines as i64),
                editable: true,
            },
            ConfigRow {
                path: "mention.max_inline_bytes".into(),
                label: "max inline bytes".into(),
                value: ConfigValue::Int(config.mention.max_inline_bytes as i64),
                default: ConfigValue::Int(defaults.mention.max_inline_bytes as i64),
                editable: true,
            },
            ConfigRow {
                path: "mention.dedupe".into(),
                label: "deduplicate refs".into(),
                value: ConfigValue::Bool(config.mention.dedupe),
                default: ConfigValue::Bool(defaults.mention.dedupe),
                editable: true,
            },
        ],
    });

    // ── workflow ──────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "workflow".into(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "workflow.mode".into(),
                label: "mode".into(),
                value: ConfigValue::Str(format!("{:?}", config.workflow.mode).to_lowercase()),
                default: ConfigValue::Str(
                    format!("{:?}", defaults.workflow.mode).to_lowercase(),
                ),
                editable: true,
            },
            ConfigRow {
                path: "workflow.meta_max_spawned".into(),
                label: "meta max spawned".into(),
                value: ConfigValue::Int(config.workflow.meta_max_spawned as i64),
                default: ConfigValue::Int(defaults.workflow.meta_max_spawned as i64),
                editable: true,
            },
            ConfigRow {
                path: "workflow.meta_max_parallel".into(),
                label: "meta max parallel".into(),
                value: ConfigValue::Int(config.workflow.meta_max_parallel as i64),
                default: ConfigValue::Int(defaults.workflow.meta_max_parallel as i64),
                editable: true,
            },
            ConfigRow {
                path: "workflow.meta_allow_nesting".into(),
                label: "meta allow nesting".into(),
                value: ConfigValue::Bool(config.workflow.meta_allow_nesting),
                default: ConfigValue::Bool(defaults.workflow.meta_allow_nesting),
                editable: true,
            },
        ],
    });

    // ── model_tiers ───────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "model_tiers".into(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "model_tiers.flagship_model".into(),
                label: "flagship model".into(),
                value: ConfigValue::Str(
                    config
                        .model_tiers
                        .flagship_model
                        .clone()
                        .unwrap_or_default(),
                ),
                default: ConfigValue::Str(String::new()),
                editable: true,
            },
            ConfigRow {
                path: "model_tiers.standard_model".into(),
                label: "standard model".into(),
                value: ConfigValue::Str(
                    config
                        .model_tiers
                        .standard_model
                        .clone()
                        .unwrap_or_default(),
                ),
                default: ConfigValue::Str(String::new()),
                editable: true,
            },
            ConfigRow {
                path: "model_tiers.fast_model".into(),
                label: "fast model".into(),
                value: ConfigValue::Str(
                    config.model_tiers.fast_model.clone().unwrap_or_default(),
                ),
                default: ConfigValue::Str(String::new()),
                editable: true,
            },
        ],
    });

    sections
}

// ── Helpers for repeated sub-struct patterns ──────────────────────────────────

fn build_agent_preset_rows(
    prefix: &str,
    preset: &xaft_config::AgentPreset,
    defaults: &xaft_config::AgentPreset,
) -> Vec<ConfigRow> {
    vec![
        ConfigRow {
            path: format!("{prefix}.model"),
            label: "model".into(),
            value: ConfigValue::Str(preset.model.clone()),
            default: ConfigValue::Str(defaults.model.clone()),
            editable: true,
        },
        ConfigRow {
            path: format!("{prefix}.provider"),
            label: "provider".into(),
            value: ConfigValue::Str(preset.provider.clone()),
            default: ConfigValue::Str(defaults.provider.clone()),
            editable: true,
        },
        ConfigRow {
            path: format!("{prefix}.max_turns"),
            label: "max turns".into(),
            value: ConfigValue::Int(preset.max_turns as i64),
            default: ConfigValue::Int(defaults.max_turns as i64),
            editable: true,
        },
        ConfigRow {
            path: format!("{prefix}.temperature"),
            label: "temperature".into(),
            value: ConfigValue::Float(preset.temperature as f64),
            default: ConfigValue::Float(defaults.temperature as f64),
            editable: true,
        },
        ConfigRow {
            path: format!("{prefix}.top_p"),
            label: "top_p".into(),
            value: ConfigValue::Float(preset.top_p as f64),
            default: ConfigValue::Float(defaults.top_p as f64),
            editable: true,
        },
        ConfigRow {
            path: format!("{prefix}.allow_dynamic_tools"),
            label: "dynamic tools".into(),
            value: ConfigValue::Bool(preset.allow_dynamic_tools),
            default: ConfigValue::Bool(defaults.allow_dynamic_tools),
            editable: true,
        },
        ConfigRow {
            path: format!("{prefix}.dynamic_tool_approval"),
            label: "dynamic tool approval".into(),
            value: ConfigValue::Bool(preset.dynamic_tool_approval),
            default: ConfigValue::Bool(defaults.dynamic_tool_approval),
            editable: true,
        },
        ConfigRow {
            path: format!("{prefix}.allowed_tools"),
            label: "allowed tools".into(),
            value: ConfigValue::Complex,
            default: ConfigValue::Complex,
            editable: false,
        },
        ConfigRow {
            path: format!("{prefix}.denied_tools"),
            label: "denied tools".into(),
            value: ConfigValue::Complex,
            default: ConfigValue::Complex,
            editable: false,
        },
    ]
}

fn build_provider_rows(
    prefix: &str,
    prov: &xaft_config::ProviderConfig,
    defaults: &xaft_config::ProviderConfig,
) -> Vec<ConfigRow> {
    vec![
        ConfigRow {
            path: format!("{prefix}.type"),
            label: "type".into(),
            value: ConfigValue::Str(format!("{:?}", prov.provider_type).to_lowercase()),
            default: ConfigValue::Str(format!("{:?}", defaults.provider_type).to_lowercase()),
            editable: true,
        },
        // api_key is always Redacted — never editable.
        ConfigRow {
            path: format!("{prefix}.api_key"),
            label: "api key".into(),
            value: ConfigValue::Redacted,
            default: ConfigValue::Redacted,
            editable: false,
        },
        ConfigRow {
            path: format!("{prefix}.api_key_env"),
            label: "api key env var".into(),
            value: ConfigValue::Str(
                prov.api_key_env.clone().unwrap_or_default(),
            ),
            default: ConfigValue::Str(
                defaults.api_key_env.clone().unwrap_or_default(),
            ),
            editable: true,
        },
        ConfigRow {
            path: format!("{prefix}.base_url"),
            label: "base URL".into(),
            value: ConfigValue::Str(prov.base_url.clone()),
            default: ConfigValue::Str(defaults.base_url.clone()),
            editable: true,
        },
        ConfigRow {
            path: format!("{prefix}.max_retries"),
            label: "max retries".into(),
            value: ConfigValue::Int(prov.max_retries as i64),
            default: ConfigValue::Int(defaults.max_retries as i64),
            editable: true,
        },
        ConfigRow {
            path: format!("{prefix}.timeout_secs"),
            label: "timeout (sec)".into(),
            value: ConfigValue::Int(prov.timeout_secs as i64),
            default: ConfigValue::Int(defaults.timeout_secs as i64),
            editable: true,
        },
    ]
}
```

---

## 7. `MenuWidget` implementation

`ConfigurationMenu` implements the `MenuWidget` trait from PRD 61.

```rust
// crates/xaft-tui/src/menu/config_menu.rs

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crate::menu::{MenuResult, MenuWidget};

impl MenuWidget for ConfigurationMenu {
    // ── Identity ──────────────────────────────────────────────────────────────

    fn title(&self) -> &str {
        "Configuration"
    }

    /// Return `Some(&self.edit_buf)` when in Edit state; `None` in Navigate.
    /// The `MenuDriver` uses this to display the live edit buffer in the header.
    fn edit_field_value(&self) -> Option<&str> {
        match self.state {
            MenuState::Edit => Some(&self.edit_buf),
            MenuState::Navigate => None,
        }
    }

    // ── Input ─────────────────────────────────────────────────────────────────

    fn handle_key(&mut self, key: KeyEvent) -> MenuResult {
        match self.state {
            MenuState::Navigate => self.handle_key_navigate(key),
            MenuState::Edit => self.handle_key_edit(key),
        }
    }

    // ── Render ────────────────────────────────────────────────────────────────

    fn render(&mut self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        self.max_visible = area.height.saturating_sub(3) as usize; // header + footer
        self.render_inner(area, buf);
    }
}
```

### 7.1 Navigation key handler

```rust
impl ConfigurationMenu {
    fn handle_key_navigate(&mut self, key: KeyEvent) -> MenuResult {
        match key.code {
            // Close menu.
            KeyCode::Esc => MenuResult::Close,

            // Move cursor down.
            KeyCode::Down => {
                self.move_cursor(1);
                MenuResult::Consumed
            }

            // Move cursor up.
            KeyCode::Up => {
                self.move_cursor(-1);
                MenuResult::Consumed
            }

            // Enter on a section header: toggle expand/collapse.
            // Enter on a Bool row: toggle the value.
            // Enter on an editable row: enter Edit mode, pre-fill edit_buf.
            // Enter on Redacted/Complex: set status "read-only field".
            KeyCode::Enter => self.activate_cursor(),

            // Tab / Right: expand the focused section if it is a header.
            KeyCode::Tab | KeyCode::Right => {
                self.toggle_section_at_cursor(true);
                MenuResult::Consumed
            }

            // Left: collapse the focused section if it is a header.
            KeyCode::Left => {
                self.toggle_section_at_cursor(false);
                MenuResult::Consumed
            }

            // Save snapshot to .xaft.toml.
            KeyCode::Char('s') if key.modifiers == KeyModifiers::NONE => {
                self.save()
            }

            // Ignore everything else in navigate mode.
            _ => MenuResult::Ignored,
        }
    }
```

### 7.2 Edit key handler

```rust
    fn handle_key_edit(&mut self, key: KeyEvent) -> MenuResult {
        match key.code {
            // Commit the edit.
            KeyCode::Enter => self.commit_edit(),

            // Cancel the edit — restore original value, return to Navigate.
            KeyCode::Esc => {
                self.state = MenuState::Navigate;
                self.edit_buf.clear();
                self.status_msg = "edit cancelled".into();
                MenuResult::Consumed
            }

            // Backspace in the edit buffer.
            KeyCode::Backspace => {
                self.edit_buf.pop();
                MenuResult::Consumed
            }

            // Delete clears the buffer entirely.
            KeyCode::Delete => {
                self.edit_buf.clear();
                MenuResult::Consumed
            }

            // Printable characters appended to the edit buffer.
            KeyCode::Char(c) => {
                self.edit_buf.push(c);
                MenuResult::Consumed
            }

            // Ignore all others (arrows, function keys, etc.).
            _ => MenuResult::Ignored,
        }
    }
```

### 7.3 Cursor movement

```rust
    /// Move the cursor by `delta` logical rows, skipping collapsed section
    /// rows, and adjusting `scroll_offset` to keep the cursor visible.
    fn move_cursor(&mut self, delta: i32) {
        // Enumerate all visible logical positions:
        //   - section header: (section_idx, -1)
        //   - row within expanded section: (section_idx, row_idx as i32)
        // Move in that flat list by delta, clamping at boundaries.
        // Update self.cursor and self.scroll_offset.
        // ...
    }
```

### 7.4 Activate cursor

```rust
    /// Act on the currently focused position based on what it represents.
    fn activate_cursor(&mut self) -> MenuResult {
        let (si, ri) = self.cursor;
        if ri == -1 {
            // Header: toggle section expansion.
            if let Some(section) = self.sections.get_mut(si) {
                section.expanded = !section.expanded;
            }
            return MenuResult::Consumed;
        }

        let row_idx = ri as usize;
        let row = match self.sections.get(si).and_then(|s| s.rows.get(row_idx)) {
            Some(r) => r,
            None => return MenuResult::Consumed,
        };

        match &row.value {
            // Bool: toggle in-place, emit signal.
            ConfigValue::Bool(b) => {
                let new_val = !b;
                let path = row.path.clone();
                let old_str = b.to_string();
                let new_str = new_val.to_string();
                // Apply to snapshot, rebuild sections, emit signal.
                self.apply_bool(&path, new_val);
                self.emit_patched(&path, &old_str, &new_str);
                self.status_msg = format!("{path} = {new_val}");
                MenuResult::Consumed
            }

            // Str / Int / Float: enter Edit mode.
            ConfigValue::Str(s) => {
                self.edit_buf = s.clone();
                self.state = MenuState::Edit;
                MenuResult::Consumed
            }
            ConfigValue::Int(n) => {
                self.edit_buf = n.to_string();
                self.state = MenuState::Edit;
                MenuResult::Consumed
            }
            ConfigValue::Float(f) => {
                self.edit_buf = f.to_string();
                self.state = MenuState::Edit;
                MenuResult::Consumed
            }

            // Redacted / Complex: read-only.
            ConfigValue::Redacted => {
                self.status_msg = "this field cannot be edited here".into();
                MenuResult::Consumed
            }
            ConfigValue::Complex => {
                self.status_msg = "use /config set <path> <value> to edit complex fields".into();
                MenuResult::Consumed
            }
        }
    }
```

### 7.5 Commit edit with type validation

```rust
    /// Commit the current `edit_buf` value to the snapshot, or set
    /// `status_msg` if validation fails and keep Edit mode active.
    fn commit_edit(&mut self) -> MenuResult {
        let (si, ri) = self.cursor;
        let row = match self.sections.get(si).and_then(|s| s.rows.get(ri as usize)) {
            Some(r) => r,
            None => return MenuResult::Consumed,
        };
        let path = row.path.clone();
        let buf = self.edit_buf.trim().to_string();

        let result: Result<ConfigValue, String> = match &row.value {
            ConfigValue::Str(_) => Ok(ConfigValue::Str(buf.clone())),
            ConfigValue::Int(_) => buf
                .parse::<i64>()
                .map(ConfigValue::Int)
                .map_err(|_| format!("expected integer, got {:?}", buf)),
            ConfigValue::Float(_) => buf
                .parse::<f64>()
                .map(ConfigValue::Float)
                .map_err(|_| format!("expected decimal number, got {:?}", buf)),
            _ => Err("not an editable value type".into()),
        };

        match result {
            Ok(new_val) => {
                let old_str = row.value.display();
                let new_str = new_val.display();
                self.apply_value(&path, new_val);
                self.emit_patched(&path, &old_str, &new_str);
                self.status_msg = format!("{path} updated");
                self.edit_buf.clear();
                self.state = MenuState::Navigate;
                MenuResult::Consumed
            }
            Err(msg) => {
                self.status_msg = format!("invalid: {msg}");
                // Stay in Edit mode — do not clear buf.
                MenuResult::Consumed
            }
        }
    }
```

---

## 8. Live config mutation and the snapshot strategy

### 8.1 Ownership model

`XaftConfig` is not wrapped in an `Arc<RwLock<_>>` at the current API surface:
`TuiApp` holds a plain owned `XaftConfig` that is cloned into each `RunRequest`.
The `ConfigurationMenu` therefore cannot hold a live shared reference that the
running agent also reads.

The snapshot strategy resolves this:

1. When the menu opens, `ConfigurationMenu::new` clones the current `XaftConfig`
   into `snapshot`. All edits mutate `snapshot` directly.
2. Every committed change (toggle or edit commit) also calls `emit_patched`,
   which fires `XaftConfigPatched` on the `SignalBus`. The running agent
   subscribes to this signal and applies the patch via its own config
   reference on the next iteration of its event loop.
3. On `s` save, the entire `snapshot` is serialized to TOML and written to
   the project `.xaft.toml`. Future sessions inherit the saved values.

```rust
impl ConfigurationMenu {
    pub fn new(
        config: Arc<XaftConfig>,
        working_dir: std::path::PathBuf,
        signals: Arc<agtrs_runtime::signals::SignalBus>,
    ) -> Self {
        let snapshot = (*config).clone();
        let sections = build_sections(&snapshot);
        Self {
            config_ref: config,
            snapshot,
            sections,
            cursor: (0, -1),
            state: MenuState::Navigate,
            edit_buf: String::new(),
            status_msg: String::new(),
            scroll_offset: 0,
            max_visible: 20,
            working_dir,
            signals,
        }
    }
}
```

### 8.2 Applying values to the snapshot

`apply_value` is a large `match path { ... }` that updates the correct field on
`self.snapshot` and then calls `self.sections = build_sections(&self.snapshot)`
to rebuild the rendered view. This is the intentional tradeoff of the hand-built
approach: type safety at the cost of exhaustive matching.

A helper `apply_bool`, `apply_int`, `apply_float`, and `apply_str` are defined
for each primitive variant to reduce duplication. Example:

```rust
fn apply_bool(&mut self, path: &str, val: bool) {
    match path {
        "core.telemetry" => self.snapshot.core.telemetry = val,
        "core.agents_md_enabled" => self.snapshot.core.agents_md_enabled = val,
        "guardrail.file_destruction" => self.snapshot.guardrail.file_destruction = val,
        "guardrail.cost_limit" => self.snapshot.guardrail.cost_limit = val,
        "compaction.enabled" => self.snapshot.compaction.enabled = val,
        "memory.enabled" => self.snapshot.memory.enabled = val,
        "tui.mouse" => self.snapshot.tui.mouse = val,
        "tui.timestamps" => self.snapshot.tui.timestamps = val,
        "tui.render_markdown" => self.snapshot.tui.render_markdown = val,
        // ... all bool fields
        _ => {
            self.status_msg = format!("unknown path: {path}");
            return;
        }
    }
    self.sections = build_sections(&self.snapshot);
}
```

Agent-preset paths with dynamic segment (e.g. `"agent.default.allow_dynamic_tools"`)
require splitting on `.` and indexing into `self.snapshot.agent`:

```rust
fn apply_preset_bool(&mut self, preset_name: &str, field: &str, val: bool) {
    if let Some(preset) = self.snapshot.agent.get_mut(preset_name) {
        match field {
            "allow_dynamic_tools" => preset.allow_dynamic_tools = val,
            "dynamic_tool_approval" => preset.dynamic_tool_approval = val,
            _ => {}
        }
    }
    self.sections = build_sections(&self.snapshot);
}
```

The same pattern applies to provider sections.

### 8.3 Save to disk

```rust
fn save(&mut self) -> MenuResult {
    let toml_str = match toml::to_string_pretty(&self.snapshot) {
        Ok(s) => s,
        Err(e) => {
            self.status_msg = format!("serialize error: {e}");
            return MenuResult::Consumed;
        }
    };

    let path = self.working_dir.join(".xaft.toml");
    if let Err(e) = std::fs::write(&path, &toml_str) {
        self.status_msg = format!("write error: {e}");
        return MenuResult::Consumed;
    }

    self.status_msg = format!(
        "saved to {}",
        path.display()
    );
    MenuResult::Consumed
}
```

Note: `save()` does NOT emit a `XaftConfigPatched` signal. Each individual field
change already emitted its own signal when the user committed the edit. `save` is
purely a disk-persistence operation; the in-memory agent state was already updated
incrementally. Calling `save` after closing the menu without editing anything is
a no-op beyond writing identical TOML.

---

## 9. `emit_patched` — signal emission

```rust
fn emit_patched(&self, path: &str, old_value: &str, new_value: &str) {
    use xaft_agent::signals::XaftConfigPatched;
    let signal = XaftConfigPatched {
        key: path.to_string(),
        old_value: old_value.to_string(),
        new_value: new_value.to_string(),
        layer: "session".to_string(),
    };
    // Emit non-blocking; the SignalBus uses tokio broadcast channels.
    // If no tokio runtime is active (unit test context), this silently drops.
    let bus = Arc::clone(&self.signals);
    let _ = tokio::task::spawn(async move {
        bus.emit(signal).await;
    });
}
```

The `XaftConfigPatched` signal is already defined in
`crates/xaft-agent/src/signals.rs` (see §11 for the subscriber side).

---

## 10. Render format

The `render_inner` method uses Ratatui primitives (no third-party widget crates).
All rendering is contained within the `area` rect passed by the `MenuDriver`.

```
┌─ Configuration ─────────────────────────────────────────────────────────────┐
│ ❯ [editing: agent.default.max_turns = 25_]  ← Edit state header             │
│ ──────────────────────────────────────────────────────────────────────────── │
│   ▼ core                                                                     │
│   │   log level             info                                             │
│ ► │   telemetry           ● true        ← focused, changed from default      │
│   │   agents.md auto-load   true                                             │
│   ▼ agent.default                                                            │
│   │   model                 claude-3-5-sonnet-20241022                       │
│ ► │   max turns           ● 25          ← focused row, changed               │
│   │   temperature           0.0000                                           │
│   ▶ guardrail (collapsed)                                                    │
│   ▶ compaction (collapsed)                                                   │
│ ──────────────────────────────────────────────────────────────────────────── │
│  ↑↓ navigate   Enter edit/toggle   Tab expand   Esc close   s save           │
└─────────────────────────────────────────────────────────────────────────────┘
```

Visual conventions:

| Element | Meaning |
|---------|---------|
| `▼ section` | Section expanded, cursor not on header |
| `▶ section (collapsed)` | Section collapsed |
| `►` prefix on a row | Row is currently focused |
| `●` after value | Value differs from compiled-in default |
| `***` value | Redacted field |
| `[ … ]` value | Complex field (Vec/HashMap) |
| Edit state header | `❯ [editing: <path> = <buf>_]` shown only in Edit state |

### Scrolling

The `scroll_offset` field tracks the first visible logical row. The logical row
list is the flat sequence of (section header OR section row) items produced by
iterating `sections` in order, skipping rows of collapsed sections. If the
cursor would move above `scroll_offset` or below `scroll_offset + max_visible`,
`scroll_offset` is adjusted by `move_cursor`.

### Color palette (uses existing `Theme`)

| Item | Theme color |
|------|-------------|
| Section header | `theme.section_header` (bold) |
| Focused row background | `theme.selection_bg` |
| Changed value marker `●` | `theme.warning` (yellow) |
| Default value | `theme.dim` |
| Error in status bar | `theme.error` (red) |
| Success in status bar | `theme.success` (green) |
| Redacted `***` | `theme.dim` (italic) |

---

## 11. `XaftConfigPatched` subscriber in the TUI

The `EventBridge` in `crates/xaft-tui/src/bridge.rs` already attaches signal
subscriptions to the `SignalBus`. PRD 63 adds a subscription for
`XaftConfigPatched`:

```rust
// in EventBridge::attach()
{
    let tx = self.tx.clone();
    signals
        .on::<XaftConfigPatched>(move |e| {
            let _ = tx.send(TuiEvent::ConfigPatched {
                key: e.key.clone(),
                new_value: e.new_value.clone(),
            });
        })
        .await;
}
```

A new `TuiEvent` variant is added to `bridge.rs`:

```rust
pub enum TuiEvent {
    // … existing variants …
    /// A config key was patched (from the ConfigurationMenu or `/config set`).
    ConfigPatched { key: String, new_value: String },
}
```

`AppState::handle_event` handles `TuiEvent::ConfigPatched` by pushing a
`RenderMutation::CommitLine` with a dim system line:

```
  ● config: agent.default.max_turns = 30  (session)
```

This gives the user visible confirmation in the transcript without requiring
them to reopen the menu.

---

## 12. File layout

```
crates/xaft-tui/src/menu/
  mod.rs              — MenuWidget trait, MenuResult enum, MenuDriver struct
                        (PRD 61 — already exists)
  dropdown.rs         — DropdownWidget (PRD 61 — already exists)
  config_menu.rs      — ConfigurationMenu, ConfigRow, ConfigSection,
                        ConfigValue, MenuState, build_sections,
                        build_agent_preset_rows, build_provider_rows   ← NEW
```

No new crate-level dependencies are required. `toml` is already a dependency of
`xaft-config` and is re-exported; `std::fs::write` is used for disk writes.

Changes to existing files:

| File | Change |
|------|--------|
| `crates/xaft-tui/src/menu/mod.rs` | Export `config_menu` module |
| `crates/xaft-tui/src/slash/commands/config.rs` | When called with no args, return `CommandResult::OpenMenu(Box::new(ConfigurationMenu::new(...)))` instead of `CommandResult::ConfigDisplay(...)` |
| `crates/xaft-tui/src/bridge.rs` | Add `TuiEvent::ConfigPatched`; subscribe `XaftConfigPatched` in `EventBridge::attach` |
| `crates/xaft-tui/src/state.rs` | Handle `TuiEvent::ConfigPatched` in `handle_event` |
| `crates/xaft-tui/src/lib.rs` | Re-export `menu::config_menu::{ConfigurationMenu, ConfigValue, ConfigRow, ConfigSection}` |

---

## 13. Integration with `/config` command

After PRD 62 lands, the `/config` handler in
`crates/xaft-tui/src/slash/commands/config.rs` is updated as follows:

```rust
pub fn handle(&self, args: &str, ctx: &CommandContext) -> CommandResult {
    let trimmed = args.trim();

    // `/config set <key> <value>` — unchanged from PRD 55.
    if let Some(rest) = trimmed.strip_prefix("set ") {
        return self.handle_set(rest, ctx);
    }

    // `/config` (no args) — open the interactive menu.
    if trimmed.is_empty() {
        let menu = ConfigurationMenu::new(
            Arc::new(ctx.config.clone()),
            ctx.working_dir.clone(),
            Arc::clone(&ctx.signals),
        );
        return CommandResult::OpenMenu(Box::new(menu));
    }

    // `/config <section>` — read-only display (PRD 55 behaviour, unchanged).
    self.handle_display(trimmed, ctx)
}
```

`CommandContext` must expose `signals: Arc<SignalBus>` and `working_dir:
PathBuf`. These fields are added to `CommandContext` in the PRD 62
`CommandMenuRegistry` implementation.

---

## 14. Test scenarios

All tests live in `crates/xaft-tui/src/menu/config_menu.rs` under
`#[cfg(test)] mod tests`, except integration tests which live in
`crates/xaft-tui/tests/config_menu_integration.rs`.

### 14.1 `build_sections` coverage

```rust
#[test]
fn build_sections_includes_core() {
    let cfg = XaftConfig::default();
    let sections = build_sections(&cfg);
    let core = sections.iter().find(|s| s.name == "core").unwrap();
    assert!(!core.rows.is_empty());
    assert!(core.rows.iter().any(|r| r.path == "core.log_level"));
}

#[test]
fn build_sections_includes_agent_default() {
    let cfg = XaftConfig::default();
    let sections = build_sections(&cfg);
    assert!(sections.iter().any(|s| s.name == "agent.default"));
}

#[test]
fn build_sections_api_key_is_redacted() {
    let cfg = XaftConfig::default();
    let sections = build_sections(&cfg);
    for section in &sections {
        for row in &section.rows {
            if row.path.ends_with(".api_key") {
                assert_eq!(row.value, ConfigValue::Redacted,
                    "field {} must be Redacted", row.path);
                assert!(!row.editable,
                    "field {} must not be editable", row.path);
            }
        }
    }
}

#[test]
fn build_sections_stable_ordering() {
    let cfg = XaftConfig::default();
    let s1 = build_sections(&cfg);
    let s2 = build_sections(&cfg);
    let names1: Vec<&str> = s1.iter().map(|s| s.name.as_str()).collect();
    let names2: Vec<&str> = s2.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names1, names2, "section order must be deterministic");
    // First section is always "core".
    assert_eq!(s1[0].name, "core");
}
```

### 14.2 Navigation

```rust
#[test]
fn cursor_starts_at_first_section_header() {
    let menu = make_test_menu();
    assert_eq!(menu.cursor, (0, -1));
}

#[test]
fn down_key_moves_cursor_into_first_row_of_expanded_section() {
    let mut menu = make_test_menu();
    // core section is expanded by default; first row index is 0.
    let _ = menu.handle_key(key(KeyCode::Down));
    assert_eq!(menu.cursor, (0, 0));
}

#[test]
fn up_from_first_position_stays_at_first() {
    let mut menu = make_test_menu();
    // Cursor is at (0, -1), Up should not wrap around.
    let _ = menu.handle_key(key(KeyCode::Up));
    assert_eq!(menu.cursor, (0, -1));
}

#[test]
fn tab_expands_collapsed_section() {
    let mut menu = make_test_menu();
    // Navigate to a collapsed section header.
    navigate_to_section_header(&mut menu, "guardrail");
    assert!(!section_expanded(&menu, "guardrail"));
    let _ = menu.handle_key(key(KeyCode::Tab));
    assert!(section_expanded(&menu, "guardrail"));
}
```

### 14.3 Bool toggle

```rust
#[test]
fn enter_on_bool_row_toggles_value() {
    let mut menu = make_test_menu();
    let orig = bool_value_at(&menu, "core.telemetry");
    navigate_to_row(&mut menu, "core.telemetry");
    let _ = menu.handle_key(key(KeyCode::Enter));
    let new = bool_value_at(&menu, "core.telemetry");
    assert_ne!(orig, new, "bool must have toggled");
}

#[test]
fn bool_toggle_does_not_enter_edit_mode() {
    let mut menu = make_test_menu();
    navigate_to_row(&mut menu, "core.telemetry");
    let _ = menu.handle_key(key(KeyCode::Enter));
    assert_eq!(menu.state, MenuState::Navigate);
}
```

### 14.4 String editing

```rust
#[test]
fn enter_on_str_row_enters_edit_mode() {
    let mut menu = make_test_menu();
    navigate_to_row(&mut menu, "core.log_level");
    let _ = menu.handle_key(key(KeyCode::Enter));
    assert_eq!(menu.state, MenuState::Edit);
    assert_eq!(&menu.edit_buf, "info"); // pre-filled with current value
}

#[test]
fn char_input_in_edit_mode_appends_to_buf() {
    let mut menu = make_test_menu();
    navigate_to_row(&mut menu, "core.log_level");
    let _ = menu.handle_key(key(KeyCode::Enter)); // enter edit
    let _ = menu.handle_key(char_key('x'));
    assert!(menu.edit_buf.ends_with('x'));
}
```

### 14.5 Edit commit

```rust
#[test]
fn enter_in_edit_mode_commits_string_value() {
    let mut menu = make_test_menu();
    navigate_to_row(&mut menu, "core.log_level");
    let _ = menu.handle_key(key(KeyCode::Enter));
    clear_buf_and_type(&mut menu, "warn");
    let _ = menu.handle_key(key(KeyCode::Enter)); // commit
    assert_eq!(menu.state, MenuState::Navigate);
    assert_eq!(
        str_value_at(&menu, "core.log_level"),
        "warn"
    );
}
```

### 14.6 Edit cancel

```rust
#[test]
fn esc_in_edit_mode_cancels_and_leaves_value_unchanged() {
    let mut menu = make_test_menu();
    let orig = str_value_at(&menu, "core.log_level");
    navigate_to_row(&mut menu, "core.log_level");
    let _ = menu.handle_key(key(KeyCode::Enter));
    clear_buf_and_type(&mut menu, "trace");
    let _ = menu.handle_key(key(KeyCode::Esc)); // cancel
    assert_eq!(menu.state, MenuState::Navigate);
    assert_eq!(str_value_at(&menu, "core.log_level"), orig);
}
```

### 14.7 Invalid integer

```rust
#[test]
fn invalid_int_sets_status_msg_and_stays_in_edit_mode() {
    let mut menu = make_test_menu();
    navigate_to_row(&mut menu, "agent.default.max_turns");
    let _ = menu.handle_key(key(KeyCode::Enter));
    clear_buf_and_type(&mut menu, "abc");
    let _ = menu.handle_key(key(KeyCode::Enter));
    assert_eq!(menu.state, MenuState::Edit, "must stay in Edit on bad int");
    assert!(
        menu.status_msg.to_lowercase().contains("integer"),
        "status must mention 'integer', got: {}",
        menu.status_msg
    );
}

#[test]
fn valid_int_commits_correctly() {
    let mut menu = make_test_menu();
    navigate_to_row(&mut menu, "agent.default.max_turns");
    let _ = menu.handle_key(key(KeyCode::Enter));
    clear_buf_and_type(&mut menu, "42");
    let _ = menu.handle_key(key(KeyCode::Enter));
    assert_eq!(menu.state, MenuState::Navigate);
    assert_eq!(int_value_at(&menu, "agent.default.max_turns"), 42);
}
```

### 14.8 Esc in Navigate mode closes menu

```rust
#[test]
fn esc_in_navigate_returns_close() {
    let mut menu = make_test_menu();
    let result = menu.handle_key(key(KeyCode::Esc));
    assert_eq!(result, MenuResult::Close);
}
```

### 14.9 Redacted field is read-only

```rust
#[test]
fn enter_on_redacted_field_sets_status_and_stays_navigate() {
    let mut menu = make_test_menu_with_provider();
    navigate_to_row(&mut menu, "provider.anthropic.api_key");
    let _ = menu.handle_key(key(KeyCode::Enter));
    assert_eq!(menu.state, MenuState::Navigate);
    assert!(
        !menu.status_msg.is_empty(),
        "status_msg must be set for redacted field"
    );
}
```

### 14.10 Save writes correct path

```rust
#[test]
fn save_writes_to_working_dir_dot_xaft_toml() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let mut menu = make_test_menu_in_dir(dir.path());
    let result = menu.handle_key(char_key('s'));
    assert_eq!(result, MenuResult::Consumed);
    let saved = dir.path().join(".xaft.toml");
    assert!(saved.exists(), ".xaft.toml must be created");
    let content = std::fs::read_to_string(&saved).unwrap();
    // Check for at least one known field to verify TOML serialization.
    assert!(content.contains("[core]") || content.contains("log_level"));
}
```

### 14.11 Complex field is read-only

```rust
#[test]
fn enter_on_complex_field_sets_status_and_stays_navigate() {
    let mut menu = make_test_menu();
    navigate_to_row(&mut menu, "agent.default.allowed_tools");
    let _ = menu.handle_key(key(KeyCode::Enter));
    assert_eq!(menu.state, MenuState::Navigate);
    assert!(!menu.status_msg.is_empty());
}
```

### 14.12 Changed marker

```rust
#[test]
fn is_changed_returns_true_after_edit() {
    let mut menu = make_test_menu();
    let orig_changed = row_is_changed(&menu, "agent.default.max_turns");
    assert!(!orig_changed, "fresh menu should show no changes");
    navigate_to_row(&mut menu, "agent.default.max_turns");
    let _ = menu.handle_key(key(KeyCode::Enter));
    clear_buf_and_type(&mut menu, "99");
    let _ = menu.handle_key(key(KeyCode::Enter));
    assert!(row_is_changed(&menu, "agent.default.max_turns"),
        "row must be marked changed after edit");
}
```

---

## 15. Acceptance criteria

| # | Criterion |
|---|-----------|
| AC1 | `/config` (no args) opens `ConfigurationMenu` overlay; does NOT append config lines to the transcript. |
| AC2 | `/config tui` still shows the PRD 55 read-only display (section filter path unchanged). |
| AC3 | Menu renders section headers with `▼`/`▶` expansion indicators. |
| AC4 | `Down`/`Up` keys navigate between rows within expanded sections and between section headers. |
| AC5 | `Enter` on a `Bool` row toggles the value without entering Edit mode. |
| AC6 | `Enter` on a `Str`/`Int`/`Float` row enters Edit mode with the current value pre-filled in `edit_buf`. |
| AC7 | Typing in Edit mode appends characters to `edit_buf`; `Backspace` removes the last character. |
| AC8 | `Enter` in Edit mode commits a valid value; `edit_buf` is cleared and state returns to Navigate. |
| AC9 | `Enter` in Edit mode with a non-integer value for an `Int` field shows a validation error and keeps Edit mode active. |
| AC10 | `Esc` in Edit mode cancels the edit; the row value is unchanged. |
| AC11 | `Esc` in Navigate mode returns `MenuResult::Close`. |
| AC12 | Rows with `ConfigValue::Redacted` display `***`; Enter does not open Edit mode. |
| AC13 | `s` keypress serializes `snapshot` to `.xaft.toml` in `working_dir` and sets a success status message. |
| AC14 | Each committed value change emits `XaftConfigPatched` on the `SignalBus`. |
| AC15 | `TuiEvent::ConfigPatched` received in `AppState` appends a dim config-change line to the transcript. |
| AC16 | `build_sections` marks no `api_key` fields as editable. |
| AC17 | All 12 unit test scenarios pass under `cargo test -p xaft-tui`. |
| AC18 | `cargo clippy --workspace` reports no errors in `config_menu.rs`. |

---

## 16. Cross-references

| Symbol | Location | Relevance |
|--------|----------|-----------|
| `XaftConfig` | `crates/xaft-config/src/types.rs` | Root config struct being edited |
| `XaftConfig::default()` | `crates/xaft-config/src/defaults.rs` | Source of default values for `ConfigRow::default` |
| `ConfigPatch` | `crates/xaft-config/src/types.rs:103` | Session-layer patch type (used by `/config set`) |
| `XaftConfigPatched` | `crates/xaft-agent/src/signals.rs:311` | Signal emitted per-field change and subscribed by the TUI |
| `MenuWidget` | `crates/xaft-tui/src/menu/mod.rs` | Trait implemented by `ConfigurationMenu` (PRD 61) |
| `MenuResult` | `crates/xaft-tui/src/menu/mod.rs` | Return type of `handle_key` (PRD 61) |
| `CommandResult::OpenMenu` | `crates/xaft-tui/src/slash/mod.rs` | Variant that triggers menu opening (PRD 62) |
| `CommandContext` | `crates/xaft-tui/src/slash/mod.rs` | Context passed to slash command handlers |
| `EventBridge` | `crates/xaft-tui/src/bridge.rs` | Subscribes signals and forwards as `TuiEvent` |
| `AppState::handle_event` | `crates/xaft-tui/src/state.rs` | Handles `TuiEvent::ConfigPatched` |
| `Theme` | `crates/xaft-tui/src/theme.rs` | Color palette used during render |
