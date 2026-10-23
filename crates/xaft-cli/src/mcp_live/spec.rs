//! One configured MCP server, resolved for connecting.
//!
//! Port of the connect-time half of agenthicc's `McpServerConfig`
//! (`tools/mcp.py`): `normalized_transport`, `validate`, `resolved_token`,
//! `resolved_headers`, `resolved_command`, plus the manager's `_auth_state`
//! and `_redact`. The *stored* shape is xaft's own
//! [`McpClientConfig`]; this type only answers "what would connecting to it
//! do, and is that allowed".
//!
//! # Mapping from xaft's config to agenthicc's
//!
//! | agenthicc | xaft | note |
//! | --- | --- | --- |
//! | `command` (argv tuple) | `command` + `args` | a `command` with spaces and no `args` is split like agenthicc's legacy string form |
//! | `url` | `url` | `${VAR}` expanded |
//! | `token` | `token` / `token_env` | `token_env` names the variable directly |
//! | `env_headers` | `env_headers` | header → variable name |
//! | `startup_timeout_s` | — | agenthicc's default, 10 s |

use std::time::Duration;

use xaft_config::{McpClientConfig, is_env_var_name};

/// agenthicc's default `startup_timeout_s`.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Transport spellings that mean the same transport (agenthicc's
/// `_TRANSPORT_ALIASES`, plus xaft's `http+sse` for the legacy SSE transport).
const TRANSPORT_ALIASES: &[(&str, &str)] = &[
    ("streamable", "streamable_http"),
    ("http", "streamable_http"),
    ("websocket", "ws"),
    ("http+sse", "sse"),
];

/// Transports a connection can be made over.
pub const SUPPORTED_TRANSPORTS: &[&str] = &["stdio", "streamable_http", "sse", "ws"];

/// A configured server, ready to validate and connect.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    config: McpClientConfig,
}

impl ServerSpec {
    /// Wrap one `[[mcp.client]]` / `[[tools.mcp_servers]]` entry.
    pub fn new(config: McpClientConfig) -> Self {
        Self { config }
    }

    /// The stored configuration.
    pub fn config(&self) -> &McpClientConfig {
        &self.config
    }

    /// The server name.
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Whether the server is enabled.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Whether the server connects without being asked.
    pub fn auto_connect(&self) -> bool {
        self.config.auto_connect
    }

    /// Retries after the first connection attempt.
    pub fn reconnect_attempts(&self) -> u32 {
        self.config.reconnect_attempts
    }

    /// Base delay between attempts; doubles each retry.
    pub fn reconnect_delay(&self) -> Duration {
        let seconds = self.config.reconnect_delay_seconds;
        if seconds.is_finite() && seconds > 0.0 {
            Duration::from_secs_f64(seconds)
        } else {
            Duration::ZERO
        }
    }

    /// The transport, lowercased with aliases resolved.
    pub fn transport(&self) -> String {
        let raw = self.config.normalized_transport();
        TRANSPORT_ALIASES
            .iter()
            .find(|(alias, _)| *alias == raw)
            .map(|(_, canonical)| canonical.to_string())
            .unwrap_or(raw)
    }

    /// The safe-to-print view of the configuration.
    pub fn redacted(&self) -> serde_json::Value {
        self.config.redacted()
    }

    /// The bearer token, with `token_env` or a `${VAR}` reference resolved.
    pub fn resolved_token(&self) -> String {
        if let Some(var) = self.config.token_env.as_deref().filter(|v| !v.is_empty()) {
            if let Ok(value) = std::env::var(var) {
                return value;
            }
        }
        self.config
            .token
            .as_deref()
            .map(expand_env)
            .unwrap_or_default()
    }

    /// The URL with `${VAR}` references expanded.
    pub fn resolved_url(&self) -> String {
        self.config
            .url
            .as_deref()
            .map(expand_env)
            .unwrap_or_default()
    }

    /// The argv for a `stdio` server; never interpreted by a shell.
    pub fn resolved_command(&self) -> Vec<String> {
        let Some(command) = self.config.command.as_deref() else {
            return Vec::new();
        };
        let command = command.trim();
        if command.is_empty() {
            return Vec::new();
        }
        let mut argv = if self.config.args.is_empty() && command.contains(char::is_whitespace) {
            // agenthicc accepts a legacy single-string command and shlex-splits
            // it; the program is still executed directly, never via a shell.
            crate::cli_plugins::shell_split(command).unwrap_or_else(|_| vec![command.to_string()])
        } else {
            vec![command.to_string()]
        };
        argv.extend(self.config.args.iter().cloned());
        argv.into_iter().map(|part| expand_env(&part)).collect()
    }

    /// Request headers: every set `env_headers` variable, then
    /// `Authorization: Bearer <token>` unless a header already claims it.
    pub fn resolved_headers(&self) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = Vec::new();
        for (header, variable) in &self.config.env_headers {
            if let Ok(value) = std::env::var(variable) {
                headers.push((header.clone(), value));
            }
        }
        let token = self.resolved_token();
        if !token.is_empty()
            && !headers
                .iter()
                .any(|(h, _)| h.eq_ignore_ascii_case("authorization"))
        {
            headers.push(("Authorization".into(), format!("Bearer {token}")));
        }
        headers
    }

    /// Validate what can be checked before connecting.
    ///
    /// Port of `McpServerConfig.validate`, with its messages.
    pub fn validate(&self) -> Result<(), String> {
        let name = self.name();
        if name.trim().is_empty() || name.contains(':') || name.chars().any(char::is_whitespace) {
            return Err("MCP server name must be non-empty and contain no spaces or ':'".into());
        }
        let transport = self.transport();
        if !SUPPORTED_TRANSPORTS.contains(&transport.as_str()) {
            return Err(format!(
                "unsupported MCP transport: '{}'",
                self.config.transport
            ));
        }
        let delay = self.config.reconnect_delay_seconds;
        if !delay.is_finite() {
            return Err("MCP reconnect settings must be finite and non-negative".into());
        }
        if delay < 0.0 {
            return Err("MCP reconnect delay cannot be negative".into());
        }
        for variable in self
            .config
            .env_headers
            .values()
            .chain(self.config.token_env.iter())
        {
            if !is_env_var_name(variable) {
                return Err(format!(
                    "invalid MCP environment variable name: '{variable}'"
                ));
            }
        }
        if transport == "stdio" {
            if self.resolved_command().is_empty() {
                return Err(format!("MCP stdio server '{name}' has no command"));
            }
            return Ok(());
        }
        let url = self.resolved_url();
        let parsed = reqwest::Url::parse(&url)
            .map_err(|_| format!("MCP server '{name}' has an invalid URL"))?;
        let schemes: &[&str] = if transport == "ws" {
            &["ws", "wss"]
        } else {
            &["http", "https"]
        };
        if !schemes.contains(&parsed.scheme()) || parsed.host_str().is_none_or(str::is_empty) {
            return Err(format!("MCP server '{name}' has an invalid URL"));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("MCP URLs must not contain embedded credentials".into());
        }
        Ok(())
    }

    /// agenthicc's `_auth_state`: `needs_auth` when a credential variable the
    /// server depends on is unset, `authenticated` when all are set, and
    /// `not_required` when it depends on none.
    pub fn auth_state(&self) -> &'static str {
        let mut required: Vec<String> = self.config.env_headers.values().cloned().collect();
        if let Some(token) = self.config.token.as_deref() {
            if let Some(inner) = token.strip_prefix("${") {
                required.push(inner.trim_end_matches('}').to_string());
            }
        }
        if let Some(var) = self.config.token_env.as_deref() {
            if !var.is_empty() && !required.iter().any(|r| r == var) {
                required.push(var.to_string());
            }
        }
        let missing = required
            .iter()
            .any(|name| !name.is_empty() && std::env::var(name).map_or(true, |v| v.is_empty()));
        if missing {
            "needs_auth"
        } else if required.is_empty() {
            "not_required"
        } else {
            "authenticated"
        }
    }

    /// Remove every secret this server could leak from `error`.
    ///
    /// agenthicc's `_redact`: the token, every header value and every argv
    /// element of four or more characters are replaced with `<redacted>`.
    pub fn redact(&self, error: &str) -> String {
        let mut result = error.to_string();
        let mut secrets: Vec<String> = vec![self.resolved_token()];
        secrets.extend(self.resolved_headers().into_iter().map(|(_, v)| v));
        secrets.extend(self.resolved_command());
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        for secret in secrets {
            if secret.chars().count() >= 4 {
                result = result.replace(&secret, "<redacted>");
            }
        }
        result
    }
}

/// Expand `${NAME}` references from the environment, leaving unknown ones.
///
/// agenthicc's `_ENV_RE` only matches upper-case names; anything else is left
/// untouched, exactly as there.
pub fn expand_env(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) if is_env_var_name(&after[..end]) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        out.push_str("${");
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str("${");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> McpClientConfig {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn transports_normalise_through_agenthiccs_aliases() {
        for (raw, want) in [
            ("stdio", "stdio"),
            ("HTTP", "streamable_http"),
            ("streamable", "streamable_http"),
            ("websocket", "ws"),
            ("http+sse", "sse"),
            ("sse", "sse"),
        ] {
            let spec = ServerSpec::new(config(&format!(
                "name = \"x\"\ntransport = \"{raw}\"\nurl = \"https://h/\""
            )));
            assert_eq!(spec.transport(), want, "{raw}");
        }
    }

    #[test]
    fn validation_messages_match_agenthicc() {
        let cases = [
            (
                "name = \"a b\"\ncommand = \"x\"",
                "MCP server name must be non-empty and contain no spaces or ':'",
            ),
            (
                "name = \"a\"\ntransport = \"carrier-pigeon\"",
                "unsupported MCP transport: 'carrier-pigeon'",
            ),
            ("name = \"a\"", "MCP stdio server 'a' has no command"),
            (
                "name = \"a\"\ntransport = \"http\"\nurl = \"ftp://h\"",
                "MCP server 'a' has an invalid URL",
            ),
            (
                "name = \"a\"\ntransport = \"ws\"\nurl = \"https://h\"",
                "MCP server 'a' has an invalid URL",
            ),
            (
                "name = \"a\"\ntransport = \"http\"\nurl = \"https://u:p@h\"",
                "MCP URLs must not contain embedded credentials",
            ),
            (
                "name = \"a\"\ncommand = \"x\"\nreconnect_delay_seconds = -1.0",
                "MCP reconnect delay cannot be negative",
            ),
            (
                "name = \"a\"\ncommand = \"x\"\nenv_headers = { X = \"lower\" }",
                "invalid MCP environment variable name: 'lower'",
            ),
        ];
        for (toml, message) in cases {
            assert_eq!(
                ServerSpec::new(config(toml)).validate().unwrap_err(),
                message,
                "{toml}"
            );
        }
        assert!(
            ServerSpec::new(config("name = \"ok\"\ncommand = \"srv\""))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn a_legacy_single_string_command_is_split_without_a_shell() {
        let spec = ServerSpec::new(config("name = \"a\"\ncommand = \"npx -y 'my server'\""));
        assert_eq!(spec.resolved_command(), vec!["npx", "-y", "my server"]);
        let explicit = ServerSpec::new(config(
            "name = \"a\"\ncommand = \"/opt/my server/bin\"\nargs = [\"--x\"]",
        ));
        assert_eq!(
            explicit.resolved_command(),
            vec!["/opt/my server/bin", "--x"]
        );
    }

    #[test]
    fn env_references_expand_only_for_upper_case_names() {
        // SAFETY: unique variable name; read only by this test.
        unsafe { std::env::set_var("XAFT_SPEC_TEST_A", "value") };
        assert_eq!(expand_env("a${XAFT_SPEC_TEST_A}b"), "avalueb");
        assert_eq!(expand_env("${lower}"), "${lower}");
        assert_eq!(
            expand_env("${XAFT_SPEC_TEST_UNSET_ZZ}"),
            "${XAFT_SPEC_TEST_UNSET_ZZ}"
        );
        assert_eq!(expand_env("${"), "${");
    }

    #[test]
    fn auth_state_follows_the_referenced_variables() {
        let none = ServerSpec::new(config("name = \"a\"\ncommand = \"x\""));
        assert_eq!(none.auth_state(), "not_required");
        let missing = ServerSpec::new(config(
            "name = \"a\"\ncommand = \"x\"\ntoken_env = \"XAFT_SPEC_TEST_UNSET_TOKEN\"",
        ));
        assert_eq!(missing.auth_state(), "needs_auth");
        // SAFETY: unique variable name; read only by this test.
        unsafe { std::env::set_var("XAFT_SPEC_TEST_TOKEN", "sk-secret-value") };
        let set = ServerSpec::new(config(
            "name = \"a\"\ntransport = \"http\"\nurl = \"https://h/\"\ntoken = \"${XAFT_SPEC_TEST_TOKEN}\"",
        ));
        assert_eq!(set.auth_state(), "authenticated");
        assert_eq!(
            set.resolved_headers(),
            vec![(
                "Authorization".to_string(),
                "Bearer sk-secret-value".to_string()
            )]
        );
        assert_eq!(
            set.redact("401: bad token sk-secret-value"),
            "401: bad token <redacted>"
        );
    }
}
