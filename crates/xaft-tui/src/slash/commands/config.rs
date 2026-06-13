//! /config command handler — interactive config menu **or** read-only display.
//!
//! - `/config`          → open `ConfigurationMenu` (PRD-63 interactive editor)
//! - `/config <filter>` → static, section-grouped display (read-only)

use std::sync::Arc;

use toml::Value as Tv;
use xaft_config::XaftConfig;

use crate::menu::config_menu::ConfigurationMenu;
use crate::slash::registry::SlashHandler;
use crate::slash::{
    CommandContext, CommandResult, ConfigLayer, ConfigRow, ConfigSection, ConfigValueKind,
};

// Canonical display order for top-level sections.
const SECTION_ORDER: &[&str] = &[
    "core",
    "agent",
    "provider",
    "tool",
    "guardrail",
    "tui",
    "compaction",
    "memory",
    "mention",
    "mcp",
    "model_tiers",
    "plugins",
];

pub struct ConfigHandler {
    config: Arc<XaftConfig>,
}

impl ConfigHandler {
    pub fn new(config: Arc<XaftConfig>) -> Self {
        Self { config }
    }
}

impl SlashHandler for ConfigHandler {
    fn description(&self) -> &'static str {
        "Open the interactive config editor (or /config <section> for read-only display)"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[section]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let args = ctx.args.trim();
        // No filter → open the interactive config menu (PRD-63).
        if args.is_empty() {
            let working_dir = ctx.working_dir.clone();
            let menu = ConfigurationMenu::new(&self.config, working_dir);
            return CommandResult::OpenMenu(Box::new(menu));
        }
        // With a section filter → keep the static read-only display.
        self.run_config(args)
    }
}

impl ConfigHandler {
    fn run_config(&self, args: &str) -> CommandResult {
        let filter = args.trim().to_lowercase();

        let toml_str = match toml::to_string_pretty(&*self.config) {
            Ok(s) => s,
            Err(e) => return CommandResult::Error(format!("Failed to serialize config: {e}")),
        };
        let root: toml::Value = match toml::from_str(&toml_str) {
            Ok(v) => v,
            Err(e) => return CommandResult::Error(format!("Failed to parse config: {e}")),
        };
        let default_root: toml::Value = {
            let default_cfg = XaftConfig::default();
            let s = toml::to_string_pretty(&default_cfg).unwrap_or_default();
            toml::from_str(&s).unwrap_or(toml::Value::Table(Default::default()))
        };

        let Tv::Table(ref top) = root else {
            return CommandResult::Error("Config root is not a TOML table.".into());
        };

        // Build sections in canonical order first, then unknowns alphabetically.
        let mut ordered_names: Vec<String> = SECTION_ORDER
            .iter()
            .filter(|&&name| top.contains_key(name))
            .map(|&s| s.to_string())
            .collect();
        let mut extra: Vec<String> = top
            .keys()
            .filter(|k| !SECTION_ORDER.contains(&k.as_str()))
            .cloned()
            .collect();
        extra.sort();
        ordered_names.extend(extra);

        let mut sections: Vec<ConfigSection> = Vec::new();

        for section_name in &ordered_names {
            if !filter.is_empty() && !section_name.to_lowercase().contains(&filter) {
                continue;
            }
            let section_val = match top.get(section_name) {
                Some(v) => v,
                None => continue,
            };
            let rows = collect_rows(section_name, section_val, &default_root);
            if !rows.is_empty() {
                sections.push(ConfigSection {
                    name: section_name.clone(),
                    rows,
                });
            }
        }

        if sections.is_empty() {
            if filter.is_empty() {
                return CommandResult::Error("Config is empty.".into());
            }
            return CommandResult::Error(format!(
                "No config section matches '{args}'.  Try: /config core | tui | agent | ..."
            ));
        }

        CommandResult::ConfigDisplay(sections)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Flatten a TOML section value into `ConfigRow`s.
///
/// Nested tables recurse with a dotted section name (e.g. `agent.default`).
fn collect_rows(section: &str, val: &Tv, defaults: &Tv) -> Vec<ConfigRow> {
    let mut out = Vec::new();
    collect_rows_inner(section, val, defaults, &mut out);
    out
}

fn collect_rows_inner(section: &str, val: &Tv, defaults: &Tv, out: &mut Vec<ConfigRow>) {
    let Tv::Table(map) = val else { return };
    for (k, v) in map {
        match v {
            Tv::Table(_) => {
                let sub = format!("{section}.{k}");
                collect_rows_inner(&sub, v, defaults, out);
            }
            _ => {
                let default_val = get_nested(defaults, section, k);
                let is_overridden = default_val != Some(v);
                let source_layer = if is_overridden {
                    ConfigLayer::Project
                } else {
                    ConfigLayer::Default
                };
                let (display_value, value_kind) = format_value(v);
                out.push(ConfigRow {
                    key: k.clone(),
                    display_value,
                    value_kind,
                    source_layer,
                    is_overridden,
                });
            }
        }
    }
}

fn get_nested<'a>(root: &'a Tv, section: &str, key: &str) -> Option<&'a Tv> {
    let mut cur = root;
    for part in section.split('.') {
        cur = cur.get(part)?;
    }
    cur.get(key)
}

fn format_value(v: &Tv) -> (String, ConfigValueKind) {
    match v {
        Tv::String(s) => (format!("{s:?}"), ConfigValueKind::Str),
        Tv::Integer(i) => (i.to_string(), ConfigValueKind::Int),
        Tv::Float(f) => (format!("{f}"), ConfigValueKind::Float),
        Tv::Boolean(b) => (b.to_string(), ConfigValueKind::Bool),
        Tv::Array(_) => ("[ … ]".into(), ConfigValueKind::Array),
        Tv::Table(_) => ("{ … }".into(), ConfigValueKind::Table),
        Tv::Datetime(d) => (d.to_string(), ConfigValueKind::Str),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_handler() -> ConfigHandler {
        ConfigHandler::new(Arc::new(XaftConfig::default()))
    }

    #[test]
    fn run_config_no_filter_returns_multiple_sections() {
        let h = make_handler();
        match h.run_config("") {
            CommandResult::ConfigDisplay(sections) => {
                assert!(!sections.is_empty(), "must return at least one section");
            }
            other => panic!("expected ConfigDisplay, got: {other:?}"),
        }
    }

    #[test]
    fn run_config_tui_filter_returns_only_tui() {
        let h = make_handler();
        match h.run_config("tui") {
            CommandResult::ConfigDisplay(sections) => {
                assert!(sections.iter().all(|s| s.name.contains("tui")));
                assert!(!sections.is_empty());
            }
            other => panic!("expected ConfigDisplay: {other:?}"),
        }
    }

    #[test]
    fn run_config_unknown_filter_returns_error() {
        let h = make_handler();
        assert!(matches!(
            h.run_config("zzznomatch"),
            CommandResult::Error(_)
        ));
    }

    #[test]
    fn default_values_not_marked_overridden() {
        let h = make_handler();
        match h.run_config("core") {
            CommandResult::ConfigDisplay(sections) => {
                for row in sections.iter().flat_map(|s| &s.rows) {
                    // Default config should produce no overrides.
                    assert!(
                        !row.is_overridden,
                        "row {} should not be overridden",
                        row.key
                    );
                }
            }
            other => panic!("expected ConfigDisplay: {other:?}"),
        }
    }

    #[test]
    fn sections_returned_in_canonical_order() {
        let h = make_handler();
        match h.run_config("") {
            CommandResult::ConfigDisplay(sections) => {
                let names: Vec<&str> = sections.iter().map(|s| s.name.as_str()).collect();
                // core must appear before tui
                if let (Some(ci), Some(ti)) = (
                    names.iter().position(|&n| n == "core"),
                    names.iter().position(|&n| n == "tui"),
                ) {
                    assert!(ci < ti, "core must come before tui");
                }
            }
            other => panic!("expected ConfigDisplay: {other:?}"),
        }
    }
}
