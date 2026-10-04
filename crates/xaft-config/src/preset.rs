//! Agent preset resolution.

use crate::error::ConfigError;
use crate::types::{ResolvedAgentPreset, XaftConfig};

/// Resolves named agent presets to fully-typed structs ready for runtime use.
pub struct AgentPresetResolver;

impl AgentPresetResolver {
    /// Resolve the active agent preset.
    ///
    /// `requested` selects the preset by name; `None` falls back to `"default"`.
    pub fn resolve(
        config: &XaftConfig,
        requested: Option<&str>,
    ) -> Result<ResolvedAgentPreset, ConfigError> {
        let preset_name = requested.unwrap_or("default");

        let preset = config
            .agent
            .get(preset_name)
            .ok_or_else(|| ConfigError::UnknownPreset {
                name: preset_name.to_string(),
            })?
            .clone();

        // Validate that the referenced provider exists
        let provider_cfg =
            config
                .provider
                .get(&preset.provider)
                .ok_or_else(|| ConfigError::UnknownProvider {
                    name: preset.provider.clone(),
                })?;

        // Resolve model alias if present. A preset that names no model falls back to
        // the provider profile's default, then to the empty string.
        let requested_model = if preset.model.is_empty() {
            provider_cfg.default_model().unwrap_or("").to_string()
        } else {
            preset.model.clone()
        };
        let model = provider_cfg.resolve_model(&requested_model);

        Ok(ResolvedAgentPreset {
            name: preset_name.to_string(),
            description: preset.description,
            model,
            provider: preset.provider,
            system_prompt: preset.system_prompt,
            max_turns: preset.max_turns,
            // Profile-level sampling defaults apply only where the preset left
            // the value at its "unset" sentinel, so a preset can always
            // override its endpoint. `0.0` is that sentinel for both: it is
            // the preset default, and the ordinary sampled ranges
            // (temp > 0, top_p in (0, 1]) are always explicit.
            temperature: match preset.temperature {
                0.0 => provider_cfg.temperature.unwrap_or(0.0),
                t => t,
            },
            top_p: match preset.top_p {
                0.0 => provider_cfg.top_p.unwrap_or(1.0),
                p => p,
            },
            max_completion_tokens: provider_cfg.max_completion_tokens,
            stop_sequences: preset.stop_sequences,
            allowed_tools: preset.allowed_tools,
            denied_tools: preset.denied_tools,
            allowed_skills: preset.allowed_skills,
            denied_skills: preset.denied_skills,
        })
    }

    /// List all available preset names.
    pub fn available_presets(config: &XaftConfig) -> Vec<String> {
        let mut names: Vec<_> = config.agent.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::XaftConfig;

    #[test]
    fn resolve_default_preset() {
        let config = XaftConfig::default();
        let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
        assert_eq!(resolved.name, "default");
        assert!(!resolved.model.is_empty());
    }

    #[test]
    fn resolve_named_preset() {
        let config = XaftConfig::default();
        let resolved = AgentPresetResolver::resolve(&config, Some("code-review")).unwrap();
        assert_eq!(resolved.name, "code-review");
    }

    #[test]
    fn unknown_preset_returns_error() {
        let config = XaftConfig::default();
        let err = AgentPresetResolver::resolve(&config, Some("does-not-exist")).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownPreset { .. }));
    }

    #[test]
    fn unknown_provider_returns_error() {
        let mut config = XaftConfig::default();
        config.agent.get_mut("default").unwrap().provider = "nonexistent".to_string();
        let err = AgentPresetResolver::resolve(&config, None).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownProvider { .. }));
    }

    #[test]
    fn model_alias_resolved() {
        let mut config = XaftConfig::default();
        config.provider.get_mut("anthropic").unwrap().models.insert(
            "sonnet".to_string(),
            "claude-3-5-sonnet-20241022".to_string(),
        );
        config.agent.get_mut("default").unwrap().model = "sonnet".to_string();

        let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
        assert_eq!(resolved.model, "claude-3-5-sonnet-20241022");
    }

    #[test]
    fn tool_allows_wildcard() {
        let config = XaftConfig::default();
        let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
        assert!(resolved.allows_tool("com.xaft.file-read"));
        assert!(resolved.allows_tool("anything-at-all"));
    }

    #[test]
    fn tool_denied_list_blocks() {
        let mut config = XaftConfig::default();
        let preset = config.agent.get_mut("default").unwrap();
        preset.denied_tools = vec!["shell".to_string()];

        let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
        assert!(!resolved.allows_tool("shell"));
        assert!(resolved.allows_tool("file-read"));
    }

    // ── Provider-profile request defaults ────────────────────────────────

    #[test]
    fn a_profile_default_model_is_used_when_the_preset_names_none() {
        let mut config = XaftConfig::default();
        config
            .provider
            .get_mut("anthropic")
            .expect("anthropic provider")
            .model = "claude-profile-default".to_string();
        config.agent.get_mut("default").unwrap().model = String::new();

        let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
        assert_eq!(resolved.model, "claude-profile-default");
    }

    #[test]
    fn an_explicit_preset_model_beats_the_profile_default() {
        let mut config = XaftConfig::default();
        config
            .provider
            .get_mut("anthropic")
            .expect("anthropic provider")
            .model = "claude-profile-default".to_string();
        config.agent.get_mut("default").unwrap().model = "claude-explicit".to_string();

        let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
        assert_eq!(
            resolved.model, "claude-explicit",
            "a preset must always override its endpoint's default"
        );
    }

    #[test]
    fn a_profile_default_model_still_goes_through_alias_expansion() {
        let mut config = XaftConfig::default();
        let provider = config.provider.get_mut("anthropic").expect("anthropic");
        provider.model = "fast".to_string();
        provider
            .models
            .insert("fast".to_string(), "claude-3-5-haiku".to_string());
        // The preset must not name a model, or its own value wins outright.
        config.agent.get_mut("default").unwrap().model = String::new();

        let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
        assert_eq!(
            resolved.model, "claude-3-5-haiku",
            "the profile default must be alias-expanded like any other model"
        );
    }

    #[test]
    fn sampling_defaults_are_inherited_then_overridden() {
        let mut config = XaftConfig::default();
        {
            let provider = config.provider.get_mut("anthropic").expect("anthropic");
            provider.temperature = Some(0.7);
            provider.top_p = Some(0.9);
            provider.max_completion_tokens = Some(4096);
        }
        // The "default" preset ships temperature 0.0 / top_p 1.0, the unset
        // sentinels, so it must inherit.
        config.agent.get_mut("default").unwrap().temperature = 0.0;
        config.agent.get_mut("default").unwrap().top_p = 0.0;

        let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
        assert!((resolved.temperature - 0.7).abs() < 1e-6);
        assert!((resolved.top_p - 0.9).abs() < 1e-6);
        assert_eq!(resolved.max_completion_tokens, Some(4096));

        // An explicit preset value wins over the profile.
        config.agent.get_mut("default").unwrap().temperature = 0.1;
        config.agent.get_mut("default").unwrap().top_p = 0.2;
        let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
        assert!((resolved.temperature - 0.1).abs() < 1e-6);
        assert!((resolved.top_p - 0.2).abs() < 1e-6);
    }

    #[test]
    fn unset_sampling_falls_back_to_the_library_defaults() {
        let config = XaftConfig::default();
        let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
        assert!(
            (resolved.temperature - 0.0).abs() < 1e-6,
            "no preset or profile temperature means 0.0"
        );
        assert!(
            (resolved.top_p - 1.0).abs() < 1e-6,
            "no preset or profile top_p means 1.0"
        );
        assert_eq!(
            resolved.max_completion_tokens, None,
            "an absent cap means unbounded"
        );
    }

    #[test]
    fn available_presets_includes_builtins() {
        let config = XaftConfig::default();
        let names = AgentPresetResolver::available_presets(&config);
        assert!(names.contains(&"default".to_string()));
        assert!(names.contains(&"code-review".to_string()));
    }
}
