//! Environment variable interpolation in config string values.
//!
//! Config values may contain `${VAR_NAME}` references which are expanded at
//! load time. If the referenced variable is not set, the placeholder is left
//! unchanged.

use serde_json::Value;

/// Recursively expand `${VAR}` references in all string values of `val`.
pub fn interpolate_strings(val: &mut Value) {
    match val {
        Value::String(s) => {
            *s = interpolate_env(s);
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                interpolate_strings(v);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                interpolate_strings(v);
            }
        }
        _ => {}
    }
}

/// Expand `${VAR_NAME}` in a single string.
///
/// Unknown variables are left as `${VAR_NAME}` (not replaced with empty string).
pub fn interpolate_env(s: &str) -> String {
    // Use a simple manual scanner to avoid pulling in the `regex` crate here.
    let mut result = String::with_capacity(s.len());
    let mut rest = s;

    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 2..];

        if let Some(end) = rest.find('}') {
            let var_name = &rest[..end];
            rest = &rest[end + 1..];

            if !var_name.is_empty()
                && var_name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                match std::env::var(var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => {
                        // Leave placeholder intact
                        result.push_str("${");
                        result.push_str(var_name);
                        result.push('}');
                    }
                }
            } else {
                // Malformed: put it back verbatim
                result.push_str("${");
                result.push_str(var_name);
                result.push('}');
            }
        } else {
            // No closing brace — put `${` back and stop scanning
            result.push_str("${");
            result.push_str(rest);
            rest = "";
        }
    }

    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_vars_unchanged() {
        assert_eq!(interpolate_env("hello world"), "hello world");
    }

    #[test]
    fn known_var_expanded() {
        unsafe { std::env::set_var("XAFT_TEST_INTERP_VAR", "expanded_value") }
        assert_eq!(
            interpolate_env("prefix_${XAFT_TEST_INTERP_VAR}_suffix"),
            "prefix_expanded_value_suffix"
        );
        unsafe { std::env::remove_var("XAFT_TEST_INTERP_VAR") }
    }

    #[test]
    fn unknown_var_preserved() {
        unsafe { std::env::remove_var("XAFT_NONEXISTENT_12345") }
        assert_eq!(
            interpolate_env("key=${XAFT_NONEXISTENT_12345}"),
            "key=${XAFT_NONEXISTENT_12345}"
        );
    }

    #[test]
    fn multiple_vars_in_string() {
        unsafe { std::env::set_var("XAFT_HOST", "localhost") }
        unsafe { std::env::set_var("XAFT_PORT", "8080") }
        assert_eq!(
            interpolate_env("http://${XAFT_HOST}:${XAFT_PORT}/api"),
            "http://localhost:8080/api"
        );
        unsafe { std::env::remove_var("XAFT_HOST") }
        unsafe { std::env::remove_var("XAFT_PORT") }
    }

    #[test]
    fn unclosed_brace_preserved() {
        assert_eq!(interpolate_env("${UNCLOSED"), "${UNCLOSED");
    }

    #[test]
    fn interpolate_nested_json() {
        unsafe { std::env::set_var("XAFT_NESTED_TEST", "replaced") }
        let mut val = serde_json::json!({
            "key": "${XAFT_NESTED_TEST}",
            "nested": {
                "another": "value_${XAFT_NESTED_TEST}"
            }
        });
        interpolate_strings(&mut val);
        assert_eq!(val["key"], "replaced");
        assert_eq!(val["nested"]["another"], "value_replaced");
        unsafe { std::env::remove_var("XAFT_NESTED_TEST") }
    }
}
