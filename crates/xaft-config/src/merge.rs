//! Deep merge logic for layered configuration.
//!
//! Merge strategy:
//! - Scalars (string, int, bool, null): override wins — full replacement
//! - Arrays: override wins — full replacement (no concatenation)
//! - Objects/maps: recursive deep merge — each key merged independently
//! - `null` in override: does NOT clear a `Some` value from base

use serde_json::Value;

/// Recursively deep-merge `override_val` into `base`.
///
/// - Object keys from `override_val` are merged into `base` recursively.
/// - Non-object values (scalars and arrays) fully replace the base value.
/// - A `null` override does NOT clear an existing non-null base value.
pub fn deep_merge(base: &mut Value, override_val: Value) {
    match (base, override_val) {
        // Both objects → recurse
        (Value::Object(base_map), Value::Object(over_map)) => {
            for (key, over_val) in over_map {
                let entry = base_map
                    .entry(key)
                    .or_insert(Value::Null);
                deep_merge(entry, over_val);
            }
        }
        // Override is null → preserve base (null = "not set")
        (_, Value::Null) => {}
        // Everything else → override wins
        (base, over) => *base = over,
    }
}

/// Merge two `XaftConfig` values by converting to JSON, merging, then converting back.
///
/// Returns the merged value as a JSON `Value` (caller deserializes to `XaftConfig`).
pub fn merge_configs(
    base: &crate::types::XaftConfig,
    override_cfg: &crate::types::XaftConfig,
) -> Result<Value, crate::error::ConfigError> {
    let mut base_val = serde_json::to_value(base)?;
    let over_val = serde_json::to_value(override_cfg)?;
    deep_merge(&mut base_val, over_val);
    Ok(base_val)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_scalars_override_wins() {
        let mut base = json!({"key": "base_value"});
        deep_merge(&mut base, json!({"key": "override_value"}));
        assert_eq!(base["key"], "override_value");
    }

    #[test]
    fn merge_null_override_preserves_base() {
        let mut base = json!({"key": "base_value"});
        deep_merge(&mut base, json!({"key": null}));
        assert_eq!(base["key"], "base_value");
    }

    #[test]
    fn merge_objects_recursive() {
        let mut base = json!({
            "core": {"log_level": "info", "telemetry": true},
        });
        deep_merge(&mut base, json!({
            "core": {"log_level": "debug"},
        }));
        assert_eq!(base["core"]["log_level"], "debug");
        assert_eq!(base["core"]["telemetry"], true); // preserved
    }

    #[test]
    fn merge_arrays_full_replace() {
        let mut base = json!({"items": [1, 2, 3]});
        deep_merge(&mut base, json!({"items": [4, 5]}));
        assert_eq!(base["items"], json!([4, 5]));
    }

    #[test]
    fn merge_new_keys_added() {
        let mut base = json!({"a": 1});
        deep_merge(&mut base, json!({"b": 2}));
        assert_eq!(base["a"], 1);
        assert_eq!(base["b"], 2);
    }

    #[test]
    fn merge_missing_override_key_preserved() {
        let mut base = json!({"a": 1, "b": 2});
        deep_merge(&mut base, json!({"a": 99}));
        assert_eq!(base["a"], 99);
        assert_eq!(base["b"], 2); // unchanged
    }

    #[test]
    fn merge_deeply_nested() {
        let mut base = json!({
            "provider": {
                "anthropic": {"api_key": "", "base_url": "https://api.anthropic.com", "max_retries": 3}
            }
        });
        deep_merge(&mut base, json!({
            "provider": {
                "anthropic": {"api_key": "secret-key"}
            }
        }));
        assert_eq!(base["provider"]["anthropic"]["api_key"], "secret-key");
        assert_eq!(base["provider"]["anthropic"]["base_url"], "https://api.anthropic.com");
        assert_eq!(base["provider"]["anthropic"]["max_retries"], 3);
    }

    #[test]
    fn merge_add_new_provider() {
        let mut base = json!({
            "provider": {
                "anthropic": {"type": "anthropic"}
            }
        });
        deep_merge(&mut base, json!({
            "provider": {
                "openai": {"type": "openai"}
            }
        }));
        assert!(base["provider"]["anthropic"].is_object());
        assert!(base["provider"]["openai"].is_object());
    }
}
