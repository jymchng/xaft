//! Configuration validation.

use crate::error::ConfigError;
use crate::keybinding::KeybindingParser;
use crate::types::XaftConfig;

/// Validate a fully-loaded `XaftConfig`.
///
/// Called after all merge layers have been applied. Returns a descriptive
/// `ConfigError::Validation` on the first failure.
pub fn validate(config: &XaftConfig) -> Result<(), ConfigError> {
    validate_agents(config)?;
    validate_providers(config)?;
    validate_guardrails(config)?;
    validate_tools(config)?;
    validate_security(config)?;
    validate_tool_settings(config)?;
    validate_behaviour(config)?;
    validate_keybindings(config)?;
    validate_tui_layout(config)?;
    Ok(())
}

fn validate_agents(config: &XaftConfig) -> Result<(), ConfigError> {
    for (name, agent) in &config.agent {
        let section = format!("agent.{name}");

        // Must reference an existing provider
        if !config.provider.contains_key(&agent.provider) {
            return Err(ConfigError::validation(
                &section,
                format!("provider '{}' not found in [provider]", agent.provider),
            ));
        }

        // Temperature must be in [0.0, 2.0]
        if !(0.0..=2.0).contains(&agent.temperature) {
            return Err(ConfigError::validation(
                &section,
                format!(
                    "temperature {} is outside the valid range [0.0, 2.0]",
                    agent.temperature
                ),
            ));
        }

        // top_p must be in (0.0, 1.0]
        if agent.top_p <= 0.0 || agent.top_p > 1.0 {
            return Err(ConfigError::validation(
                &section,
                format!(
                    "top_p {} is outside the valid range (0.0, 1.0]",
                    agent.top_p
                ),
            ));
        }

        // max_turns must be positive
        if agent.max_turns == 0 {
            return Err(ConfigError::validation(&section, "max_turns must be > 0"));
        }
    }
    Ok(())
}

fn validate_providers(config: &XaftConfig) -> Result<(), ConfigError> {
    for (name, provider) in &config.provider {
        let section = format!("provider.{name}");

        // max_retries must be ≥ 0 (0 = no retry, already enforced by u32)

        // timeout_secs must be positive
        if provider.timeout_secs == 0 {
            return Err(ConfigError::validation(
                &section,
                "timeout_secs must be > 0",
            ));
        }

        // base_url must not be empty
        if provider.base_url.is_empty() {
            return Err(ConfigError::validation(
                &section,
                "base_url must not be empty",
            ));
        }

        // timeout_s, when present, must be a usable deadline
        if provider.timeout_s.is_some_and(|t| t <= 0.0) {
            return Err(ConfigError::validation(
                &section,
                "timeout_s must be greater than zero",
            ));
        }

        // The request-option table has to describe a single request: no key
        // may be stated twice, and none may restate a value xaft fills in.
        if let Some(request_options) = &provider.request_options {
            request_options.validate(&section)?;
        }
    }
    Ok(())
}

fn validate_guardrails(config: &XaftConfig) -> Result<(), ConfigError> {
    if config.guardrail.cost_limit_enabled() {
        let cl = &config.guardrail.cost_limit_config;

        if cl.max_spend <= 0.0 {
            return Err(ConfigError::validation(
                "guardrail.cost_limit",
                "max_spend must be > 0.0",
            ));
        }

        if cl.warn_at_percent > 100 {
            return Err(ConfigError::validation(
                "guardrail.cost_limit",
                "warn_at_percent must be in [0, 100]",
            ));
        }
    }
    Ok(())
}

fn validate_tools(config: &XaftConfig) -> Result<(), ConfigError> {
    // Two stanzas for one server name is a config error, not a merge. Which one
    // wins would depend on array order, so a server's transport and credentials
    // would depend on how the file happens to be sorted.
    let duplicates = config.tools.duplicate_server_names();
    if !duplicates.is_empty() {
        return Err(ConfigError::validation(
            "tools.mcp_servers",
            format!("duplicate MCP server name(s): {}", duplicates.join(", ")),
        ));
    }

    // Validate human-friendly sizes if present
    if let Some(fr) = config.tool.get("file-read") {
        if let Some(size_str) = fr.extra.get("max_file_size").and_then(|v| v.as_str()) {
            crate::size::parse_size(size_str)?;
        }
    }
    Ok(())
}

fn validate_tool_settings(config: &XaftConfig) -> Result<(), ConfigError> {
    let tools = &config.tools;

    // A question that times out immediately would deny the agent any chance to
    // answer, so zero and negatives are refused rather than honoured.
    if tools.question_timeout_s <= 0.0 {
        return Err(ConfigError::validation(
            "tools.question_timeout_s",
            "must be greater than zero",
        ));
    }

    // http_timeout_s uses 0.0 for "no timeout", so only a negative value is
    // meaningless here -- and a negative Duration is a panic, not a no-op.
    if tools.http_timeout_s < 0.0 {
        return Err(ConfigError::validation(
            "tools.http_timeout_s",
            "must not be negative (use 0.0 to disable the timeout)",
        ));
    }

    // A live-call budget of zero would hide every tool call from the user while
    // still running them, which is worse than either extreme.
    if tools.max_live_tool_calls == 0 {
        return Err(ConfigError::validation(
            "tools.max_live_tool_calls",
            "must be greater than zero",
        ));
    }
    Ok(())
}

fn validate_behaviour(config: &XaftConfig) -> Result<(), ConfigError> {
    // An empty profile name would match nothing and fall back silently; a
    // named profile must either exist or be reported.
    if let Some(profile) = config.execution.profile.as_deref() {
        if profile.trim().is_empty() {
            return Err(ConfigError::validation(
                "execution.profile",
                "must name a profile, not be empty",
            ));
        }
        if !config.provider.contains_key(profile) {
            return Err(ConfigError::validation(
                "execution.profile",
                format!("provider '{profile}' not found in [provider]"),
            ));
        }
    }
    Ok(())
}

fn validate_security(config: &XaftConfig) -> Result<(), ConfigError> {
    config.security.policy().validate("security")?;

    // A pattern named by both lists is denied, which is safe but almost
    // certainly a config mistake -- the allow rule is dead and the user probably
    // meant something else by it. Rejecting it beats silently ignoring half of
    // what they wrote.
    for pattern in &config.tools.allowed {
        if config.tools.denied.iter().any(|d| d == pattern) {
            return Err(ConfigError::validation(
                "tools",
                format!(
                    "'{pattern}' is both allowed and denied; denied wins, so the allow rule is dead"
                ),
            ));
        }
    }
    Ok(())
}

fn validate_keybindings(config: &XaftConfig) -> Result<(), ConfigError> {
    for (key_str, _) in &config.tui.keybindings.bindings {
        KeybindingParser::parse(key_str).map_err(|e| {
            ConfigError::validation("tui.keybindings", format!("invalid key '{key_str}': {e}"))
        })?;
    }
    Ok(())
}

fn validate_tui_layout(config: &XaftConfig) -> Result<(), ConfigError> {
    let layout = &config.tui.layout;
    let total = layout.conversation_width as u32 + layout.sidebar_width as u32;
    if total != 100 {
        return Err(ConfigError::validation(
            "tui.layout",
            format!(
                "conversation_width ({}) + sidebar_width ({}) must equal 100, got {}",
                layout.conversation_width, layout.sidebar_width, total
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::XaftConfig;

    fn valid_config() -> XaftConfig {
        XaftConfig::default()
    }

    #[test]
    fn default_config_is_valid() {
        validate(&valid_config()).expect("default config should be valid");
    }

    #[test]
    fn invalid_temperature_rejected() {
        let mut cfg = valid_config();
        cfg.agent.get_mut("default").unwrap().temperature = 3.0;
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("temperature"));
    }

    #[test]
    fn zero_max_turns_rejected() {
        let mut cfg = valid_config();
        cfg.agent.get_mut("default").unwrap().max_turns = 0;
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("max_turns"));
    }

    #[test]
    fn missing_provider_rejected() {
        let mut cfg = valid_config();
        cfg.agent.get_mut("default").unwrap().provider = "nonexistent".to_string();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn zero_timeout_rejected() {
        let mut cfg = valid_config();
        cfg.provider.get_mut("anthropic").unwrap().timeout_secs = 0;
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("timeout_secs"));
    }

    #[test]
    fn negative_cost_limit_rejected() {
        let mut cfg = valid_config();
        cfg.guardrail.cost_limit = true;
        cfg.guardrail.cost_limit_config.max_spend = -1.0;
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("max_spend"));
    }

    #[test]
    fn invalid_layout_rejected() {
        let mut cfg = valid_config();
        cfg.tui.layout.conversation_width = 50;
        cfg.tui.layout.sidebar_width = 30; // 50+30 = 80 ≠ 100
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("100"));
    }
}
