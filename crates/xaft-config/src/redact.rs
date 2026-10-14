//! The whole-config redacted view that `xaft config show` prints.
//!
//! # The defect this closes
//!
//! `xaft config show --format json|toml` handed the raw [`XaftConfig`] to
//! `serde_json` / `toml`, so everything the loader had just read was read back
//! out again: `provider.<name>.api_key`, `provider.<name>.headers` values,
//! `mcp.client[].token`, `tools.mcp_servers[].token`, and -- the case the
//! per-command redactors never covered, because it is a *value* rather than a
//! key -- a credential pasted into `env_headers`. Measured on
//! `target/debug/xaft` with four planted values in one file: all four appeared
//! in both the `json` and the `toml` output.
//!
//! agenthicc never had this defect: its `config show` prints
//! `Config.redacted_dict()`, never the raw config. This module is the xaft
//! equivalent of that method -- one function that turns a resolved config into
//! the only representation that may be serialized.
//!
//! # Why the rule is the *provider* rule
//!
//! [`key_is_sensitive`] is [`redact_value`]'s test, which is agenthicc's
//! `_SENSITIVE_KEY_PARTS`: a key containing `token`, `secret`, `password`,
//! `credential`, `authorization` or `api_key` is masked. Reusing it rather than
//! writing a config-specific list is the point -- `ProviderConfig::redacted`,
//! `McpClientConfig::redacted` and this walk now agree by construction about
//! what a secret-shaped key is, and a new field on any config struct is covered
//! the day it is added instead of the day someone remembers to redact it.
//!
//! # What is reconciled with `redacted_dict()`, and what is not
//!
//! The *policy* is reconciled: agenthicc redacts provider profiles through
//! `ProviderProfile.redacted()` and MCP servers through `McpServerConfig.redacted()`,
//! and this walk reproduces both the key rule they share and the variable-name
//! exception they both make. The *layout* is deliberately not reconciled:
//! agenthicc's `redacted_dict()` returns its own section dict (`execution`,
//! `behaviour`, `api`, ...), which is a Python-shaped view of a Python config.
//! xaft keeps the config's own shape, because the same `--format toml` output is
//! what a user diffs against `xaft.toml`, and a renamed field would make that
//! diff useless. What must match is which values survive, not what they are
//! called.
//!
//! One consequence of walking the serialized value rather than the struct: keys
//! come out in `serde_json::Map` order (sorted) instead of declaration order,
//! since `serde_json` is built without `preserve_order`. JSON objects are
//! unordered by definition and the TOML serializer re-orders values before
//! tables itself, so this is cosmetic -- but it is a visible difference from the
//! pre-redaction output and is recorded here so it is not mistaken for a bug.
//!
//! # The four exceptions, and the case the key rule alone gets wrong
//!
//! Masking on the key name is right for `api_key` and `token` and wrong in four
//! places, each of which is a *value* or a *name*:
//!
//! * **`env_headers`.** The map's keys are header names and its values are meant
//!   to be environment-variable names (`Authorization = "MCP_TOKEN"`). Masking
//!   every value would destroy the one thing the field exists to show; masking
//!   by key would miss `X-Api-Key = "CANARY"` entirely, because the hyphenated
//!   header name does not contain the substring `api_key`. Values go through
//!   [`is_env_var_name`]: a conforming variable name survives, anything else
//!   becomes [`NON_ENV_HEADER_VALUE`]. That is
//!   [`McpClientConfig::redacted_env_headers`](crate::McpClientConfig::redacted_env_headers)
//!   applied to the whole config, using the same predicate as `mcp add
//!   --token-env` and `mcp get`.
//! * **`headers` / `default_headers` / `extra_headers`.** These map a
//!   caller-chosen name to a credential, so the names survive and the values do
//!   not. Same reason as above: `Authorization` is a header *name*, and the key
//!   rule would mask the name and keep the credential. A `SecretRef`
//!   (`{ env = "NAME" }`) keeps its variable name, matching
//!   [`HeaderValue::redacted`](crate::HeaderValue::redacted).
//! * **`api_key_env` / `token_env`.** These hold the *name* of the variable the
//!   secret lives in -- what a user needs in order to fix a missing credential --
//!   and [`ProviderConfig::redacted`](crate::ProviderConfig::redacted) prints
//!   `api_key_env` verbatim for the same reason.
//! * **Non-string scalars are never masked.** `guardrail.secret_leakage` is a
//!   `bool` whose key contains `secret`; a credential is always a string, so
//!   masking it would replace a real switch with `"<redacted>"` and make the
//!   command wrong rather than merely terse. An empty string is "unset" and is
//!   left empty for the same reason.

use serde_json::{Map, Value};

use crate::provider::{REDACTED, key_is_sensitive};
use crate::types::{NON_ENV_HEADER_VALUE, XaftConfig, is_env_var_name};

/// Keys whose value is the *name* of an environment variable, not the secret.
const VARIABLE_NAME_KEYS: &[&str] = &["api_key_env", "token_env"];

/// Keys whose object value maps a caller-chosen name to a credential.
const CREDENTIAL_MAPS: &[&str] = &["headers", "default_headers", "extra_headers"];

/// Keys whose object value maps a header name to an environment-variable name.
const ENV_HEADER_MAPS: &[&str] = &["env_headers"];

impl XaftConfig {
    /// The JSON view of this config that is safe to serialize.
    ///
    /// Callers must print *this* and not `self`: `serde_json::to_string_pretty(&config)`
    /// is the historical leak, and it is not distinguishable at the call site
    /// from the redacted value once both are `Serialize`. `config show`'s
    /// `render_serialized_config` is the single call site, so there is one
    /// place to check.
    pub fn redacted(&self) -> Value {
        // `to_value` cannot fail for a config that round-tripped through the
        // loader (no non-string map keys, no floats that JSON cannot hold), and
        // an empty object is the safe answer if it somehow does: it prints no
        // config rather than an unredacted one.
        match serde_json::to_value(self) {
            Ok(value) => walk(value),
            Err(_) => Value::Object(Map::new()),
        }
    }
}

/// Walk one value, deciding every child by its own key.
fn walk(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, child)| {
                    let redacted = redact_child(&key, child);
                    (key, redacted)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(walk).collect()),
        scalar => scalar,
    }
}

/// The redacted form of `value`, which sits under `key` in its parent.
///
/// The container rules deliberately come first: `extra_headers` has no
/// sensitive substring but holds credentials, while a header *named*
/// `Authorization` does have one and must still be preserved as a name.
fn redact_child(key: &str, value: Value) -> Value {
    if ENV_HEADER_MAPS.contains(&key) {
        return redact_env_headers(value);
    }
    if CREDENTIAL_MAPS.contains(&key) {
        return mask_map_values(value);
    }
    if VARIABLE_NAME_KEYS.contains(&key) {
        return value;
    }
    if key_is_sensitive(key) {
        return mask(value);
    }
    walk(value)
}

/// Mask the credential in a value sitting under a sensitive key.
fn mask(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(if text.is_empty() {
            // "unset" is not a secret; masking it would claim a credential the
            // user has not configured.
            text
        } else {
            REDACTED.to_string()
        }),
        // A container under a sensitive key -- a `SecretRef`'s `{ env = "NAME" }`,
        // or a `credentials = { ... }` table -- is walked so its own leaves
        // decide: a variable name survives, a literal does not.
        Value::Array(_) | Value::Object(_) => walk(value),
        // Bool, number and null cannot be credentials. `guardrail.secret_leakage`
        // is the bool this arm exists for.
        scalar => scalar,
    }
}

/// `env_headers` with any value that is not an environment-variable name
/// replaced by [`NON_ENV_HEADER_VALUE`]. Header names always survive.
fn redact_env_headers(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(header, entry)| {
                    let shown = match &entry {
                        Value::String(name) if is_env_var_name(name) => entry.clone(),
                        _ => Value::String(NON_ENV_HEADER_VALUE.to_string()),
                    };
                    (header, shown)
                })
                .collect(),
        ),
        other => other,
    }
}

/// Every value masked, every name kept.
fn mask_map_values(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(name, entry)| {
                    let shown = match entry {
                        Value::String(_) => Value::String(REDACTED.to_string()),
                        // `{ env = "NAME" }` is a reference, not a literal.
                        container => walk(container),
                    };
                    (name, shown)
                })
                .collect(),
        ),
        other => other,
    }
}

/// [`XaftConfig::redacted`] with null-valued members removed, for TOML.
///
/// TOML has no null, and `serde_json::Value::Null` reaches the TOML serializer
/// as a *unit*, which it rejects outright: `toml::to_string_pretty(&config.redacted())`
/// fails, and the handler's `unwrap_or_default()` would print **nothing** --
/// `config show --format toml` silently becoming a no-op is a worse outcome than
/// the leak it was fixing.
///
/// The struct path never hit this because `Option::None` arrives at the TOML
/// serializer as `serialize_none`, which it skips; only a JSON `Value` carries
/// an explicit null. Dropping those members reproduces exactly what
/// `toml::to_string_pretty(&config)` wrote for unset fields: an absent key.
/// Derived from measurements on `target/debug/xaft`, where the `json` output
/// carried 38 nulls (e.g. `"url": null`) that the `toml` output simply omitted.
///
/// Only object members are dropped. A null *array element* is left alone,
/// which keeps every index stable; no array in the config can hold one, since
/// TOML cannot express it in the first place.
pub fn without_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, child)| !child.is_null())
                .map(|(key, child)| (key, without_nulls(child)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(without_nulls).collect()),
        scalar => scalar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a config the same way the loader does, so the test never touches
    /// `$HOME`, a project `.xaft/`, or any `XAFT_*` variable.
    fn config_from_toml(src: &str) -> XaftConfig {
        toml::from_str(src).expect("valid config TOML")
    }

    /// One config carrying a distinct canary in every place this module claims
    /// to protect, so a single assertion can prove the whole surface.
    ///
    /// The canaries are deliberately *not* credential-shaped: a realistic key
    /// would be masked by the test harness's own output filter, and the tests
    /// would pass without the redactor doing anything. The `env_headers` value
    /// is lowercase on purpose too -- an all-caps canary such as
    /// `CANARYMCPHEADER` *is* a valid environment-variable name
    /// (`^[A-Z_][A-Z0-9_]*$`), so the env-header rule under test would keep it
    /// and the assertion would be vacuous.
    fn planted() -> XaftConfig {
        config_from_toml(
            r#"
[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "CANARYPROVIDERKEY"
api_key_env = "ANTHROPIC_KEY_VAR"
headers = { Authorization = "CANARYPROVIDERHEADER" }

[[mcp.client]]
name = "stdio"
transport = "stdio"
command = "npx"
token = "CANARYMCPTOKEN"
env_headers = { Authorization = "canary-mcp-header", X-Api-Key = "MCP_TOKEN_VAR" }

[[tools.mcp_servers]]
name = "remote"
transport = "http"
url = "https://mcp.example.invalid/mcp"
token = "CANARYTOOLSTOKEN"
env_headers = { X-Api-Key = "TOOLS_TOKEN_VAR" }
"#,
        )
    }

    /// Render through the same serializer `config show --format json` uses, so
    /// the assertion is about the bytes that reach stdout.
    fn rendered(config: &XaftConfig) -> String {
        serde_json::to_string_pretty(&config.redacted()).expect("redacted config is JSON")
    }

    /// Assert the canary is absent and, separately, that the redactor ran at
    /// all -- an empty string would satisfy `!contains` on its own.
    fn assert_masked(out: &str, canary: &str) {
        assert!(
            !out.contains(canary),
            "the planted value {canary} reached the output"
        );
        assert!(
            out.contains(REDACTED),
            "no value was masked at all, so the {canary} assertion proves nothing"
        );
    }

    #[test]
    fn a_planted_provider_api_key_never_reaches_the_output() {
        assert_masked(&rendered(&planted()), "CANARYPROVIDERKEY");
    }

    #[test]
    fn a_planted_provider_header_value_never_reaches_the_output() {
        let out = rendered(&planted());
        assert_masked(&out, "CANARYPROVIDERHEADER");
        // ...but the header *name* must survive: it is what tells the reader
        // which credential is misconfigured.
        assert!(out.contains("Authorization"), "the header name was dropped");
    }

    #[test]
    fn a_planted_mcp_token_never_reaches_the_output_for_either_spelling() {
        let out = rendered(&planted());
        assert_masked(&out, "CANARYMCPTOKEN");
        assert_masked(&out, "CANARYTOOLSTOKEN");
    }

    #[test]
    fn a_planted_env_header_value_never_reaches_the_output() {
        // The case the key rule alone misses: `X-Api-Key` has hyphens, so it
        // does not contain the substring `api_key`, and the *value* is the
        // credential.
        assert_masked(&rendered(&planted()), "canary-mcp-header");
    }

    #[test]
    fn a_conforming_env_header_variable_name_survives_in_both_spellings() {
        let out = rendered(&planted());
        for name in ["MCP_TOKEN_VAR", "TOOLS_TOKEN_VAR"] {
            assert!(
                out.contains(name),
                "{name} is a variable name, not a secret, and must be shown"
            );
        }
    }

    #[test]
    fn variable_name_fields_survive_because_they_name_the_secret() {
        assert!(rendered(&planted()).contains("ANTHROPIC_KEY_VAR"));
    }

    #[test]
    fn the_name_of_a_bad_env_header_value_says_why_it_is_missing() {
        // The placeholder has to distinguish "typo" from "secret"; a bare
        // `<redacted>` would leave the user guessing between opposite fixes.
        assert!(rendered(&planted()).contains(NON_ENV_HEADER_VALUE));
    }

    #[test]
    fn a_non_string_under_a_sensitive_key_is_not_masked() {
        // `guardrail.secret_leakage` is a bool whose key contains `secret`.
        // Masking it would turn a real switch into `"<redacted>"`.
        let config = config_from_toml(
            r#"
[guardrail]
secret_leakage = true
file_destruction = false
"#,
        );
        let out = rendered(&config);
        assert!(
            out.contains("\"secret_leakage\": true"),
            "the guardrail switch was masked: {out}"
        );
        assert!(out.contains("\"file_destruction\": false"));
    }

    #[test]
    fn an_unset_api_key_is_not_reported_as_a_secret() {
        // An empty string means "not configured". Masking it would claim a
        // credential the user does not have.
        let config = config_from_toml(
            r#"
[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
"#,
        );
        let out = rendered(&config);
        assert!(
            out.contains("\"api_key\": \"\""),
            "the empty api_key changed: {out}"
        );
    }

    #[test]
    fn a_redacted_config_is_still_serializable_as_toml() {
        // `config show --format toml` serializes the same value, minus the
        // nulls TOML cannot express. Pinned because the handler falls back to
        // an empty string on a serializer error, so a redactor that introduced
        // a null would make `--format toml` print nothing -- and a test that
        // only asserted the absence of a canary would still pass.
        let toml = toml::to_string_pretty(&without_nulls(planted().redacted()))
            .expect("redacted config is TOML once nulls are dropped");
        assert!(!toml.contains("CANARY"), "a canary reached the TOML output");
        assert!(
            toml.contains("base_url"),
            "the TOML output is not the config: {toml}"
        );
    }

    #[test]
    fn without_nulls_drops_unset_members_without_touching_anything_else() {
        // The JSON view keeps `null` for an unset field (`"url": null`), which
        // is what the pre-redaction output did; only TOML needs it gone.
        let json = planted().redacted();
        assert!(
            serde_json::to_string(&json).unwrap().contains("null"),
            "the JSON view should still report unset members as null"
        );
        let pruned = without_nulls(json);
        assert!(!serde_json::to_string(&pruned).unwrap().contains("null"));
        // Nothing that had a value was dropped.
        assert_eq!(
            pruned["provider"]["anthropic"]["base_url"],
            serde_json::json!("https://api.anthropic.com")
        );
    }

    #[test]
    fn the_config_is_still_recognisable_after_redaction() {
        // The command exists to show the resolved config, so a redactor that
        // blanked the whole document would be "safe and useless".
        let out = rendered(&planted());
        for expected in [
            "https://api.anthropic.com",
            "anthropic",
            "stdio",
            "npx",
            "https://mcp.example.invalid/mcp",
        ] {
            assert!(out.contains(expected), "missing {expected} in {out}");
        }
    }
}
