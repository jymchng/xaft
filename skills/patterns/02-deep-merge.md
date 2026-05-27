# Deep Merge Pattern

## Purpose

Configuration in xaft is layered: a base config provides sensible defaults, a project-level `.xaft.toml` overrides project-specific settings, and command-line flags override everything. When these layers conflict, the system must merge them correctly. Simple "last writer wins" is insufficient because nested objects need to be merged recursively (you want to override `guardrail.cost_limit` without losing `guardrail.auto_approve_reads`). The deep merge pattern specifies exactly how `serde_json::Value` trees are combined: objects recurse key-by-key, scalars and arrays override completely, and `null` in the override means "not set" (preserve the base value). This is not a generic JSON merge—it is a configuration-specific merge with explicit null semantics that prevent accidental value clearing.

## Mental Model

Think of deep merge as overlay transparencies. The base config is the bottom transparency with all the defaults filled in. Each override layer is another transparency placed on top. Where the overlay has a value, it covers the base. Where the overlay is transparent (null), the base shows through. For nested objects, the overlay doesn't replace the entire object—it only covers the keys it specifies. For scalars and arrays, the overlay completely replaces the base because partial scalar/array merging is ambiguous (what does "merge two temperatures" mean?). The key insight is that `null` is not "set to null"—it is "not set at all," which preserves the base value. This distinction is critical for command-line flags like `--cost-limit null` which should mean "use the config file value," not "clear the cost limit."

## Extension Patterns

When adding a new config key that should be deep-merged, ensure it is a JSON object (not a scalar) if it has sub-keys that might be overridden independently. When adding a new config layer (e.g., an environment variable layer), call `deep_merge(base, override)` with the environment values as the override. When adding a CLI flag that clears a config value, use `--flag explicit_value` syntax rather than `--flag null`, since `null` has special merge semantics. When implementing deep merge for a new config type, follow the `serde_json::Value` rules: `Value::Object` recurses key-by-key, `Value::Null` in the override preserves the base, and all other types (String, Number, Bool, Array) override completely.

## Common Pitfalls

- **Treating `null` as "clear the value"**: If `null` in the override clears the base value, then there is no way to say "don't override this field" in a config file. This breaks the layering model. Always treat `null` as "preserve base."
- **Merging arrays element-by-element**: What does it mean to merge `[1, 2, 3]` with `[4, 5]`? Append? Replace by index? There is no universal answer. Arrays always override completely to avoid ambiguity.
- **Deep merging scalars**: Two strings or two numbers cannot be meaningfully "merged." If `temperature` is `0.7` in the base and `0.1` in the override, the override wins—there is no recursion into a scalar.
- **Forgetting to handle the `null`-preserves-base case**: A naive implementation that does `override.clone()` when the override is not `Value::Null` will correctly skip `null` overrides, but a buggy implementation that does `match (base, override) { (_, Value::Null) => override.clone(), ... }` will return `null` instead of preserving the base.
- **Mutating the base during merge**: Deep merge must produce a new `Value` without modifying the inputs. Mutating the base would make it impossible to merge the same base with different overrides (e.g., merging CLI flags and config file against the same defaults).

## Invariants

1. Object merging must recurse key-by-key. Keys present only in the base are preserved. Keys present only in the override are added. Keys present in both are recursively merged.
2. Scalar and array values in the override must replace the base value completely. No element-wise array merging.
3. `Value::Null` in the override must preserve the base value. Null means "not set," not "clear."
4. Deep merge must not mutate either input. It must produce a new `Value`.
5. The merge order is base-first: `deep_merge(base, override)` means override wins on conflicts.
6. For config layering, the order is: defaults → project config → user config → CLI flags. Each subsequent layer is merged as the override.

## Examples

```rust
/// Deep merge two serde_json::Value trees.
/// Objects recurse key-by-key, scalars and arrays override completely,
/// null in override preserves the base value.
pub fn deep_merge(base: &Value, override_val: &Value) -> Value {
    match (base, override_val) {
        // Both objects: recurse key-by-key
        (Value::Object(base_map), Value::Object(over_map)) => {
            let mut result = base_map.clone();
            for (key, over_val) in over_map {
                // Null in override means "not set" → preserve base
                if over_val.is_null() {
                    continue;
                }
                let merged = match result.get(key) {
                    Some(base_val) => deep_merge(base_val, over_val),
                    None => over_val.clone(),
                };
                result.insert(key.clone(), merged);
            }
            Value::Object(result)
        }
        // Null override → preserve base
        (_, Value::Null) => base.clone(),
        // Scalar or array override → replace completely
        _ => override_val.clone(),
    }
}

// Config layering example
let defaults = serde_json::json!({
    "guardrail": {
        "cost_limit": 10.0,
        "auto_approve_reads": true,
        "auto_approve_writes": false
    },
    "model": "claude-sonnet-4-20250514"
});

let project_config = serde_json::json!({
    "guardrail": {
        "cost_limit": 25.0,
        // auto_approve_reads not specified → preserved from defaults
        // null means "not set" → preserved from defaults
        "auto_approve_writes": null
    }
});

let cli_flags = serde_json::json!({
    "model": "claude-opus-4-20250514"
});

let merged = deep_merge(&deep_merge(&defaults, &project_config), &cli_flags);
// Result:
// {
//   "guardrail": {
//     "cost_limit": 25.0,           // overridden by project config
//     "auto_approve_reads": true,   // preserved from defaults
//     "auto_approve_writes": false  // null in override → preserved from defaults
//   },
//   "model": "claude-opus-4-20250514"  // overridden by CLI flags
// }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_preserves_base() {
        let base = serde_json::json!({"key": "value"});
        let override_val = serde_json::json!({"key": null});
        let result = deep_merge(&base, &override_val);
        assert_eq!(result["key"], "value");
    }

    #[test]
    fn scalar_overrides_completely() {
        let base = serde_json::json!({"temp": 0.7});
        let override_val = serde_json::json!({"temp": 0.1});
        let result = deep_merge(&base, &override_val);
        assert_eq!(result["temp"], 0.1);
    }

    #[test]
    fn objects_recurse() {
        let base = serde_json::json!({"a": {"x": 1, "y": 2}});
        let override_val = serde_json::json!({"a": {"y": 3}});
        let result = deep_merge(&base, &override_val);
        assert_eq!(result["a"]["x"], 1); // preserved
        assert_eq!(result["a"]["y"], 3); // overridden
    }
}
```
