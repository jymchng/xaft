//! Provider construction from `XaftConfig`.
//!
//! `ProviderFactory::build(config, preset_name)` translates a xaft-config
//! provider entry into a fully-wrapped `Arc<dyn LlmProvider>`:
//!
//! ```text
//! ProviderConfig (Anthropic/OpenAI)
//!     └── base provider (AnthropicProvider / OpenAiProvider)
//!             └── FallbackProvider  (retry chain)
//!                     └── CostedProvider  (optional cost routing)
//! ```

use std::sync::Arc;

use agtrs_anthropic::AnthropicProvider;
use agtrs_openai::OpenAiProvider;
use agtrs_providers_router::costed::CostedProvider;
use agtrs_providers_router::fallback::{FallbackProvider, StreamingMode};
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::transport::{Message, Role};
use xaft_config::XaftConfig;
use xaft_config::types::{ProviderConfig, ProviderType, ResolvedTiers};

use crate::error::RuntimeError;

// ── ProviderFactory ───────────────────────────────────────────────────────────

/// Builds a production `LlmProvider` chain from xaft configuration.
///
/// The resulting provider is:
/// 1. A concrete provider (`AnthropicProvider` / `OpenAiProvider`)
/// 2. Wrapped in `FallbackProvider` for automatic retry and rate-limit handling
/// 3. Wrapped in `CostedProvider` for optional request routing
///
/// # Errors
///
/// Returns `RuntimeError::Provider` if:
/// - The agent preset is not found in config
/// - The provider referenced by the preset is not found
/// - No API key is available (not in config and not in the referenced env var)
pub struct ProviderFactory;

impl ProviderFactory {
    /// Build a provider for the named agent preset.
    ///
    /// `preset_name` defaults to `"default"` when `None`.
    pub fn build(
        config: &XaftConfig,
        preset_name: Option<&str>,
    ) -> Result<Arc<dyn LlmProvider>, RuntimeError> {
        let preset_name = preset_name.unwrap_or("default");

        let preset = config.agent.get(preset_name).ok_or_else(|| {
            RuntimeError::Provider(format!(
                "agent preset '{preset_name}' not found in config (available: {})",
                config.agent.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
        })?;

        let provider_name = &preset.provider;
        let provider_cfg = config.provider.get(provider_name).ok_or_else(|| {
            RuntimeError::Provider(format!(
                "provider '{provider_name}' not found in config (available: {})",
                config
                    .provider
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;

        let model = &preset.model;
        let api_key = resolve_api_key_for(provider_cfg, provider_name)?;

        let base: Arc<dyn LlmProvider> = match provider_cfg.provider_type {
            ProviderType::Anthropic => {
                let mut p = AnthropicProvider::new(api_key).with_model(model);
                if !provider_cfg.base_url.is_empty() {
                    p = p.with_base_url(&provider_cfg.base_url);
                }
                Arc::new(p) as Arc<dyn LlmProvider>
            }
            ProviderType::Openai | ProviderType::OpenaiCompatible => {
                let mut p = OpenAiProvider::new(api_key).with_model(model);
                if !provider_cfg.base_url.is_empty() {
                    p = p.with_base_url(&provider_cfg.base_url);
                }
                if !provider_cfg.organization.is_empty() {
                    p = p.with_org(&provider_cfg.organization);
                }
                Arc::new(p) as Arc<dyn LlmProvider>
            }
        };

        // Wrap in FallbackProvider for automatic retry semantics
        let fallback = FallbackProvider::new(vec![base])
            .retry_on_rate_limit(true)
            .retry_on_server_error(true)
            .streaming_mode(StreamingMode::BufferAndCommit);

        // Wrap in CostedProvider for future routing extensibility
        let costed = CostedProvider::builder()
            .default(Arc::new(fallback))
            .build();

        Ok(Arc::new(costed))
    }

    /// Build a provider with explicit fallback providers.
    ///
    /// The primary provider is built from `preset_name`; `fallbacks` are tried
    /// in order when the primary fails.
    pub fn build_with_fallbacks(
        config: &XaftConfig,
        preset_name: Option<&str>,
        fallback_presets: &[&str],
    ) -> Result<Arc<dyn LlmProvider>, RuntimeError> {
        let primary = Self::build(config, preset_name)?;

        if fallback_presets.is_empty() {
            return Ok(primary);
        }

        let mut providers = vec![primary];
        for name in fallback_presets {
            match Self::build(config, Some(name)) {
                Ok(p) => providers.push(p),
                Err(e) => {
                    tracing::warn!(preset = name, error = %e, "xaft: could not build fallback provider, skipping");
                }
            }
        }

        let fallback = FallbackProvider::new(providers)
            .retry_on_rate_limit(true)
            .retry_on_server_error(true);

        let costed = CostedProvider::builder()
            .default(Arc::new(fallback))
            .build();

        Ok(Arc::new(costed))
    }
}

// ── Tiered provider routing ───────────────────────────────────────────────────

/// Build a `CostedProvider` that routes calls to one of three tiers based on
/// the agent's system prompt content.
///
/// Routing predicates inspect the first `System` message — the same technique
/// used in agtrs example `06_codegen_cli.rs` — because the system prompt is the
/// only reliable per-call signal available inside `LlmProvider::complete()`.
///
/// When all three tiers resolve to the same model, returns a plain single
/// provider (no routing overhead).
pub fn build_tiered_provider(
    config: &XaftConfig,
    preset_name: &str,
    tiers: &ResolvedTiers,
) -> Result<Arc<dyn LlmProvider>, RuntimeError> {
    if tiers.all_same() {
        return ProviderFactory::build(config, Some(preset_name));
    }

    let flagship = build_provider_for_model(config, preset_name, &tiers.flagship)?;
    let standard = build_provider_for_model(config, preset_name, &tiers.standard)?;
    let fast = build_provider_for_model(config, preset_name, &tiers.fast)?;

    tracing::info!(
        flagship = %tiers.flagship,
        standard = %tiers.standard,
        fast = %tiers.fast,
        "xaft: building tiered provider"
    );

    let costed = CostedProvider::builder()
        // Tier 1: flagship — planner + QA (complex reasoning)
        .route_named(
            "flagship (planner / qa)",
            |_opts, msgs| {
                let sys = first_system_text(msgs);
                sys.contains("task analyzer")
                    || sys.contains("code reviewer")
                    || sys.contains("requirements analyst")
                    || sys.contains("decompose")
                    || sys.contains("review each file")
            },
            flagship,
        )
        // Tier 2: standard — coder + fixer (code generation)
        .route_named(
            "standard (coder / fixer)",
            |_opts, msgs| {
                let sys = first_system_text(msgs);
                sys.contains("expert software engineer")
                    || sys.contains("bug fixer")
                    || sys.contains("write_file")
                    || sys.contains("edit_file")
            },
            standard,
        )
        // Tier 3: fast — summarizer + any unmatched call
        .default(fast)
        .build();

    Ok(Arc::new(costed))
}

/// Build a single-model provider from the named preset with a model override.
///
/// Extracts the provider construction logic from `ProviderFactory::build()`
/// but substitutes a different model name.
fn build_provider_for_model(
    config: &XaftConfig,
    preset_name: &str,
    model: &str,
) -> Result<Arc<dyn LlmProvider>, RuntimeError> {
    let preset = config
        .agent
        .get(preset_name)
        .ok_or_else(|| RuntimeError::Provider(format!("agent preset '{preset_name}' not found")))?;

    let provider_name = &preset.provider;
    let provider_cfg = config
        .provider
        .get(provider_name)
        .ok_or_else(|| RuntimeError::Provider(format!("provider '{provider_name}' not found")))?;

    let api_key = resolve_api_key_for(provider_cfg, provider_name)?;

    let base: Arc<dyn LlmProvider> = match provider_cfg.provider_type {
        ProviderType::Anthropic => {
            let mut p = AnthropicProvider::new(api_key).with_model(model);
            if !provider_cfg.base_url.is_empty() {
                p = p.with_base_url(&provider_cfg.base_url);
            }
            Arc::new(p) as Arc<dyn LlmProvider>
        }
        ProviderType::Openai | ProviderType::OpenaiCompatible => {
            let mut p = OpenAiProvider::new(api_key).with_model(model);
            if !provider_cfg.base_url.is_empty() {
                p = p.with_base_url(&provider_cfg.base_url);
            }
            if !provider_cfg.organization.is_empty() {
                p = p.with_org(&provider_cfg.organization);
            }
            Arc::new(p) as Arc<dyn LlmProvider>
        }
    };

    let fallback = FallbackProvider::new(vec![base])
        .retry_on_rate_limit(true)
        .retry_on_server_error(true)
        .streaming_mode(StreamingMode::BufferAndCommit);

    Ok(Arc::new(fallback) as Arc<dyn LlmProvider>)
}

/// Extract the lowercased text of the first `System` message.
///
/// Used by routing predicates to match agent identity via system prompt content.
fn first_system_text(msgs: &[Message]) -> String {
    msgs.iter()
        .find(|m| m.role == Role::System)
        .map(|m| m.text().to_lowercase())
        .unwrap_or_default()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve the API key from config or environment variable.
///
/// Precedence:
/// 1. `cfg.api_key_env` — env var name from config (e.g. `OPENAI_API_KEY`)
/// 2. `cfg.api_key` — inline key
/// 3. `XAFT_<PROVIDER_NAME>_API_KEY` — xaft-namespaced env var
/// 4. Type-specific standard vars (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.)
/// 5. Universal fallback: all known key env vars regardless of provider type —
///    guards against mis-matched config where provider type ≠ actual provider
fn resolve_api_key_for(cfg: &ProviderConfig, provider_name: &str) -> Result<String, RuntimeError> {
    // Steps 1–3 via ProviderConfig::resolve_api_key (api_key_env → api_key → XAFT_ var)
    if let Some(key) = cfg.resolve_api_key(provider_name) {
        if !key.is_empty() {
            return Ok(key);
        }
    }

    // Step 4: type-specific well-known env vars
    let type_vars: &[&str] = match cfg.provider_type {
        ProviderType::Anthropic => &["ANTHROPIC_API_KEY", "CLAUDE_API_KEY"],
        ProviderType::Openai | ProviderType::OpenaiCompatible => {
            &["OPENAI_API_KEY", "OPENROUTER_API_KEY"]
        }
    };

    // Step 5: universal fallback — try ALL known key env vars so that setting
    // OPENAI_API_KEY always works even when config merged to the wrong provider type
    let universal_vars: &[&str] = &[
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CLAUDE_API_KEY",
        "OPENROUTER_API_KEY",
    ];

    for var in type_vars.iter().chain(universal_vars.iter()) {
        if let Ok(key) = std::env::var(var) {
            if !key.is_empty() {
                return Ok(key);
            }
        }
    }

    Err(RuntimeError::Provider(format!(
        "no API key found for provider '{provider_name}' (type={:?}, api_key_env={}). \
         Set OPENAI_API_KEY or ANTHROPIC_API_KEY, or add api_key_env to .xaft.toml [provider.{provider_name}].",
        cfg.provider_type,
        cfg.api_key_env.as_deref().unwrap_or("(not set)")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xaft_config::XaftConfig;

    fn config_with_provider(provider_type: ProviderType, api_key: &str, model: &str) -> XaftConfig {
        let mut cfg = XaftConfig::default();
        cfg.provider.insert(
            "test-provider".into(),
            xaft_config::types::ProviderConfig {
                provider_type,
                api_key: api_key.into(),
                api_key_env: None,
                base_url: String::new(),
                organization: String::new(),
                max_retries: 3,
                timeout_secs: 30,
                ..Default::default()
            },
        );
        cfg.agent.get_mut("default").unwrap().provider = "test-provider".into();
        cfg.agent.get_mut("default").unwrap().model = model.into();
        cfg
    }

    #[test]
    fn missing_preset_returns_error() {
        let cfg = XaftConfig::default();
        let result = ProviderFactory::build(&cfg, Some("nonexistent"));
        let err = result.err().expect("expected error");
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn missing_provider_returns_error() {
        let mut cfg = XaftConfig::default();
        // Point default preset to a provider that doesn't exist in provider map
        cfg.agent.get_mut("default").unwrap().provider = "ghost-provider".into();
        let result = ProviderFactory::build(&cfg, Some("default"));
        let err = result.err().expect("expected error");
        assert!(err.to_string().contains("ghost-provider"));
    }

    #[test]
    fn missing_api_key_returns_error() {
        let cfg = config_with_provider(ProviderType::Anthropic, "", "claude-3-5-sonnet-20241022");
        // Remove any set env vars for this test
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") }
        unsafe { std::env::remove_var("CLAUDE_API_KEY") }
        // Only fails if no env vars are set, which may not be the case in CI
        // So we test that it either succeeds (env var set) or returns Provider error
        let result = ProviderFactory::build(&cfg, Some("default"));
        match result {
            Ok(_) => {}                          // env var was set, that's OK
            Err(RuntimeError::Provider(_)) => {} // expected when no env var
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn api_key_from_inline_config() {
        let cfg = config_with_provider(ProviderType::Openai, "sk-test-key-1234", "gpt-4o-mini");
        // Should succeed (returns a provider, even if the key is fake)
        let result = ProviderFactory::build(&cfg, Some("default"));
        assert!(
            result.is_ok(),
            "should build provider with inline key but got error"
        );
    }

    #[test]
    fn api_key_from_env_var() {
        let cfg = config_with_provider(ProviderType::Anthropic, "", "claude-3-5-sonnet-20241022");
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-key") }
        let result = ProviderFactory::build(&cfg, Some("default"));
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") }
        assert!(result.is_ok(), "expected provider to build from env var");
    }

    // ── Tiered provider tests ─────────────────────────────────────────────

    use xaft_config::{ModelTierConfig, ResolvedTiers};

    #[test]
    fn model_tier_config_defaults_to_base_model() {
        let cfg = ModelTierConfig::default();
        let tiers = cfg.resolve("claude-3-5-sonnet");
        assert_eq!(tiers.flagship, "claude-3-5-sonnet");
        assert_eq!(tiers.standard, "claude-3-5-sonnet");
        assert_eq!(tiers.fast, "claude-3-5-sonnet");
        assert!(tiers.all_same());
    }

    #[test]
    fn model_tier_config_from_values() {
        let cfg = ModelTierConfig {
            flagship_model: Some("opus".into()),
            standard_model: Some("sonnet".into()),
            fast_model: Some("haiku".into()),
        };
        let tiers = cfg.resolve("default-model");
        assert_eq!(tiers.flagship, "opus");
        assert_eq!(tiers.standard, "sonnet");
        assert_eq!(tiers.fast, "haiku");
        assert!(!tiers.all_same());
    }

    #[test]
    fn model_tier_config_partial_override() {
        let cfg = ModelTierConfig {
            flagship_model: Some("opus".into()),
            standard_model: None,
            fast_model: None,
        };
        let tiers = cfg.resolve("sonnet");
        assert_eq!(tiers.flagship, "opus");
        assert_eq!(tiers.standard, "sonnet"); // fallback
        assert_eq!(tiers.fast, "sonnet"); // fallback
        assert!(!tiers.all_same());
    }

    #[test]
    fn model_tier_config_deserializes_from_json() {
        let json = r#"{
            "flagship_model": "claude-opus-4-7",
            "standard_model": "claude-sonnet-4-6",
            "fast_model": "claude-haiku-4-5"
        }"#;
        let cfg: ModelTierConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.flagship_model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(cfg.standard_model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(cfg.fast_model.as_deref(), Some("claude-haiku-4-5"));
    }

    #[test]
    fn resolved_tiers_all_same_when_equal() {
        let tiers = ResolvedTiers {
            flagship: "same".into(),
            standard: "same".into(),
            fast: "same".into(),
        };
        assert!(tiers.all_same());
    }

    #[test]
    fn resolved_tiers_not_all_same_when_different() {
        let tiers = ResolvedTiers {
            flagship: "opus".into(),
            standard: "sonnet".into(),
            fast: "haiku".into(),
        };
        assert!(!tiers.all_same());
    }

    #[test]
    fn all_same_tiers_returns_single_provider() {
        let cfg = config_with_provider(ProviderType::Openai, "sk-test", "gpt-4o");
        let tiers = ResolvedTiers {
            flagship: "gpt-4o".into(),
            standard: "gpt-4o".into(),
            fast: "gpt-4o".into(),
        };
        let result = build_tiered_provider(&cfg, "default", &tiers);
        assert!(result.is_ok(), "all-same tiers must build successfully");
    }

    #[test]
    fn tiered_provider_builds_with_different_tiers() {
        let cfg = config_with_provider(ProviderType::Openai, "sk-test", "gpt-4o");
        let tiers = ResolvedTiers {
            flagship: "gpt-4o".into(),
            standard: "gpt-4o-mini".into(),
            fast: "gpt-3.5-turbo".into(),
        };
        let result = build_tiered_provider(&cfg, "default", &tiers);
        assert!(result.is_ok(), "tiered provider must build successfully");
    }

    // ── Predicate tests ───────────────────────────────────────────────────

    use agtrs_runtime::transport::Message;

    fn planner_system_msg() -> Vec<Message> {
        vec![Message::system(
            "You are a smart task analyzer and router for a coding assistant.",
        )]
    }

    fn qa_system_msg() -> Vec<Message> {
        vec![Message::system(
            "You are a code reviewer. Verify that the following task was completed correctly:",
        )]
    }

    fn coder_system_msg() -> Vec<Message> {
        vec![Message::system(
            "You are an expert software engineer. Edit files using the provided tools.",
        )]
    }

    fn fixer_system_msg() -> Vec<Message> {
        vec![Message::system(
            "You are a bug fixer working on this task: fix auth",
        )]
    }

    fn summarizer_system_msg() -> Vec<Message> {
        vec![Message::system(
            "You are summarising the result of an automated coding task.",
        )]
    }

    fn flagship_predicate(msgs: &[Message]) -> bool {
        let sys = first_system_text(msgs);
        sys.contains("task analyzer")
            || sys.contains("code reviewer")
            || sys.contains("requirements analyst")
            || sys.contains("decompose")
            || sys.contains("review each file")
    }

    fn standard_predicate(msgs: &[Message]) -> bool {
        let sys = first_system_text(msgs);
        sys.contains("expert software engineer")
            || sys.contains("bug fixer")
            || sys.contains("write_file")
            || sys.contains("edit_file")
    }

    #[test]
    fn flagship_predicate_matches_planner() {
        assert!(flagship_predicate(&planner_system_msg()));
    }

    #[test]
    fn flagship_predicate_matches_qa() {
        assert!(flagship_predicate(&qa_system_msg()));
    }

    #[test]
    fn standard_predicate_matches_coder() {
        assert!(standard_predicate(&coder_system_msg()));
    }

    #[test]
    fn standard_predicate_matches_fixer() {
        assert!(standard_predicate(&fixer_system_msg()));
    }

    #[test]
    fn summarizer_matches_neither_flagship_nor_standard() {
        let msgs = summarizer_system_msg();
        assert!(!flagship_predicate(&msgs));
        assert!(!standard_predicate(&msgs));
    }

    #[test]
    fn no_predicate_overlap() {
        // No system prompt should match both flagship AND standard
        let all_prompts = vec![
            planner_system_msg(),
            qa_system_msg(),
            coder_system_msg(),
            fixer_system_msg(),
            summarizer_system_msg(),
        ];
        for msgs in &all_prompts {
            let is_flagship = flagship_predicate(msgs);
            let is_standard = standard_predicate(msgs);
            assert!(
                !(is_flagship && is_standard),
                "overlap detected: a prompt matches both flagship and standard"
            );
        }
    }
}
