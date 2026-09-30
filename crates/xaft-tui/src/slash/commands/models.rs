//! `/models` handler — list the configured LLM providers.
//!
//! Mirrors agenthicc's `/models` (which shares `_cmd_model`): a table of
//! provider, default model, API-key env var, and whether that key is set.
//! agenthicc checks `os.environ` directly; xaft resolves the same way via
//! `ProviderConfig::resolve_api_key`, which prefers `api_key_env` and never
//! returns the key to this handler for printing.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};
use xaft_config::types::ProviderConfig;

pub struct ModelsHandler;

impl SlashHandler for ModelsHandler {
    fn description(&self) -> &'static str {
        "List all configured LLM providers and their default models"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let providers = &ctx.config.provider;
        if providers.is_empty() {
            return CommandResult::Lines(vec!["No providers configured.".into()]);
        }

        // Sorted so the output is stable across runs (HashMap order is not).
        let mut names: Vec<&String> = providers.keys().collect();
        names.sort();

        let mut lines = vec![format!("LLM Providers ({}):", names.len())];
        for name in names {
            let p: &ProviderConfig = &providers[name];
            let model = p
                .models
                .get("default")
                .cloned()
                .unwrap_or_else(|| "—".to_string());
            let env = p
                .api_key_env
                .clone()
                .unwrap_or_else(|| conventional_env(name));
            let status = if p.resolve_api_key(name).is_some() {
                "✓ set"
            } else {
                "✗ not set"
            };
            lines.push(format!("  {name}  model={model}  key_env={env}  {status}"));
        }
        lines.push("  Set a provider: /model <name>".into());
        CommandResult::Lines(lines)
    }
}

/// The fallback env var name, mirroring `ProviderConfig::resolve_api_key`'s
/// third lookup step. Reporting it is what makes "not set" actionable.
fn conventional_env(provider: &str) -> String {
    format!("XAFT_{}_API_KEY", provider.to_uppercase().replace('-', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::AgentStatsMap;
    use crate::slash::SlashCommand;
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};
    use xaft_config::XaftConfig;

    fn ctx(cfg: XaftConfig) -> CommandContext {
        CommandContext {
            args: String::new(),
            command: SlashCommand::Models,
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(cfg),
            session_id: None,
            working_dir: PathBuf::from("/tmp"),
            terminal_cols: 80,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: None,
            session_store: None,
            intent_active: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            input_history: None,
        }
    }

    fn lines(out: CommandResult) -> Vec<String> {
        match out {
            CommandResult::Lines(v) => v,
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn lists_every_configured_provider() {
        let cfg = XaftConfig::default();
        let expected: Vec<String> = cfg.provider.keys().cloned().collect();
        let out = lines(ModelsHandler.execute(ctx(cfg)));
        for name in expected {
            assert!(
                out.iter().any(|l| l.contains(&name)),
                "missing {name}: {out:?}"
            );
        }
    }

    #[test]
    fn reports_the_configured_env_var_and_its_state() {
        let mut cfg = XaftConfig::default();
        let p = cfg.provider.get_mut("anthropic").expect("default provider");
        p.api_key_env = Some("XAFT_TEST_ANTHROPIC_KEY_UNSET".into());
        p.api_key = String::new();

        let out = lines(ModelsHandler.execute(ctx(cfg)));
        let row = out
            .iter()
            .find(|l| l.trim_start().starts_with("anthropic"))
            .expect("no anthropic row");
        assert!(row.contains("XAFT_TEST_ANTHROPIC_KEY_UNSET"), "{row}");
        assert!(row.contains("not set"), "{row}");
    }

    #[test]
    fn never_prints_an_api_key_value() {
        let mut cfg = XaftConfig::default();
        let p = cfg.provider.get_mut("anthropic").expect("default provider");
        p.api_key_env = None;
        p.api_key = "sk-super-secret-value".into();

        let out = lines(ModelsHandler.execute(ctx(cfg)));
        assert!(
            !out.join("\n").contains("sk-super-secret-value"),
            "the key value must never be printed: {out:?}"
        );
    }

    #[test]
    fn empty_provider_map_says_so() {
        let mut cfg = XaftConfig::default();
        cfg.provider.clear();
        let out = lines(ModelsHandler.execute(ctx(cfg)));
        assert_eq!(out, vec!["No providers configured.".to_string()]);
    }
}
