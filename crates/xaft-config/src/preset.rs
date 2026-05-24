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
        let provider_cfg = config
            .provider
            .get(&preset.provider)
            .ok_or_else(|| ConfigError::UnknownProvider {
                name: preset.provider.clone(),
            })?;

        // Resolve model alias if present
        let model = provider_cfg.resolve_model(&preset.model);

        Ok(ResolvedAgentPreset {
            name: preset_name.to_string(),
            model,
            provider: preset.provider,
            system_prompt: preset.system_prompt,
            max_turns: preset.max_turns,
            temperature: preset.temperature,
            top_p: preset.top_p,
            stop_sequences: preset.stop_sequences,
            allowed_tools: preset.allowed_tools,
            denied_tools: preset.denied_tools,
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
        config
            .agent
            .get_mut("default")
            .unwrap()
            .provider = "nonexistent".to_string();
        let err = AgentPresetResolver::resolve(&config, None).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownProvider { .. }));
    }

    #[test]
    fn model_alias_resolved() {
        let mut config = XaftConfig::default();
        config
            .provider
            .get_mut("anthropic")
            .unwrap()
            .models
            .insert("sonnet".to_string(), "claude-3-5-sonnet-20241022".to_string());
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

    #[test]
    fn available_presets_includes_builtins() {
        let config = XaftConfig::default();
        let names = AgentPresetResolver::available_presets(&config);
        assert!(names.contains(&"default".to_string()));
        assert!(names.contains(&"code-review".to_string()));
    }
}
