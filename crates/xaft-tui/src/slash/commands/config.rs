//! /config command handler — produces a navigable ConfigEntry list.

use std::sync::Arc;

use toml::Value as Tv;
use xaft_config::XaftConfig;

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult, ConfigEntry, ConfigLayer, ConfigValueKind};

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
        "Show the resolved configuration"
    }
    fn args_hint(&self) -> Option<&'static str> {
        Some("[section]")
    }
    fn execute(&self, ctx: CommandContext) -> CommandResult {
        self.run_config(ctx.args.trim())
    }
}

impl ConfigHandler {
    /// Execute with an optional section filter (empty = show all).
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

        let mut entries: Vec<ConfigEntry> = Vec::new();
        if let Tv::Table(ref top) = root {
            for (section_name, section_val) in top {
                if !filter.is_empty() && !section_name.to_lowercase().contains(&filter) {
                    continue;
                }
                flatten_section(section_name, section_val, &default_root, &mut entries);
            }
        }

        if entries.is_empty() {
            if filter.is_empty() {
                return CommandResult::Error("Config is empty.".into());
            }
            return CommandResult::Error(format!("No config section matches '{}'.", args.trim()));
        }
        CommandResult::ConfigEditor(entries)
    }
}

fn flatten_section(section: &str, val: &Tv, defaults: &Tv, out: &mut Vec<ConfigEntry>) {
    match val {
        Tv::Table(map) => {
            for (k, v) in map {
                match v {
                    Tv::Table(_) => {
                        // Nested table — recurse with dotted key.
                        let sub_section = format!("{section}.{k}");
                        flatten_section(&sub_section, v, defaults, out);
                    }
                    _ => {
                        let default_val = get_nested(defaults, section, k);
                        let source_layer = if default_val == Some(v) {
                            ConfigLayer::Default
                        } else {
                            ConfigLayer::Project
                        };
                        let (display_value, raw_value, value_kind) = format_value(v);
                        let editable =
                            !matches!(value_kind, ConfigValueKind::Array | ConfigValueKind::Table);
                        out.push(ConfigEntry {
                            section: section.to_string(),
                            key: k.clone(),
                            display_value,
                            raw_value,
                            value_kind,
                            source_layer,
                            editable,
                        });
                    }
                }
            }
        }
        _ => {
            // Top-level scalar — unusual but handle it.
            let (display_value, raw_value, value_kind) = format_value(val);
            out.push(ConfigEntry {
                section: section.to_string(),
                key: String::new(),
                display_value,
                raw_value,
                value_kind: value_kind.clone(),
                source_layer: ConfigLayer::Default,
                editable: !matches!(value_kind, ConfigValueKind::Array | ConfigValueKind::Table),
            });
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

fn format_value(v: &Tv) -> (String, String, ConfigValueKind) {
    match v {
        Tv::String(s) => (format!("{s:?}"), s.clone(), ConfigValueKind::Str),
        Tv::Integer(i) => (i.to_string(), i.to_string(), ConfigValueKind::Int),
        Tv::Float(f) => (format!("{f:.4}"), f.to_string(), ConfigValueKind::Float),
        Tv::Boolean(b) => (b.to_string(), b.to_string(), ConfigValueKind::Bool),
        Tv::Array(_) => ("[ … ]".into(), String::new(), ConfigValueKind::Array),
        Tv::Table(_) => ("{ … }".into(), String::new(), ConfigValueKind::Table),
        Tv::Datetime(d) => (d.to_string(), d.to_string(), ConfigValueKind::Str),
    }
}
