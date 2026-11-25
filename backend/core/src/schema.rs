use serde_json::Value;

/// Minimal JSON Schema subset used by the broker. Unknown keywords are ignored,
/// but unsupported value types fail closed rather than being treated as valid.
pub fn validate(value: &Value, schema: &Value) -> Result<(), String> {
    validate_at(value, schema, "$")
}

fn type_matches(value: &Value, expected: &str, path: &str) -> Result<bool, String> {
    match expected {
        "object" => Ok(value.is_object()),
        "array" => Ok(value.is_array()),
        "string" => Ok(value.is_string()),
        "integer" => Ok(value.is_i64() || value.is_u64()),
        "number" => Ok(value.is_number()),
        "boolean" => Ok(value.is_boolean()),
        "null" => Ok(value.is_null()),
        other => Err(format!("{path}: unsupported schema type '{other}'")),
    }
}

fn validate_at(value: &Value, schema: &Value, path: &str) -> Result<(), String> {
    match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => return Err(format!("{path}: schema forbids any value")),
        Value::Object(_) => {}
        _ => return Err(format!("{path}: unsupported schema node")),
    }
    if let Some(schema_type) = schema.get("type") {
        match schema_type {
            Value::String(expected) => {
                if !type_matches(value, expected, path)? {
                    return Err(format!("{path}: expected {expected}"));
                }
            }
            Value::Array(choices) if !choices.is_empty() => {
                let mut matched = false;
                let mut names = Vec::with_capacity(choices.len());
                for choice in choices {
                    let expected = choice
                        .as_str()
                        .ok_or_else(|| format!("{path}: schema type array must contain strings"))?;
                    names.push(expected);
                    if type_matches(value, expected, path)? {
                        matched = true;
                    }
                }
                if !matched {
                    return Err(format!("{path}: expected one of [{}]", names.join(", ")));
                }
            }
            _ => return Err(format!("{path}: unsupported schema type constraint")),
        }
    }

    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        if !options.iter().any(|option| option == value) {
            return Err(format!("{path}: value is not an allowed enum member"));
        }
    }

    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if value.as_f64().is_some_and(|v| v < min) {
            return Err(format!("{path}: value is below minimum {min}"));
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if value.as_f64().is_some_and(|v| v > max) {
            return Err(format!("{path}: value is above maximum {max}"));
        }
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(format!("{path}: missing required field '{key}'"));
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (key, child) in object {
                if let Some(child_schema) = properties.get(key) {
                    validate_at(child, child_schema, &format!("{path}.{key}"))?;
                } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                    return Err(format!("{path}: unexpected field '{key}'"));
                }
            }
        } else if schema.get("additionalProperties") == Some(&Value::Bool(false))
            && !object.is_empty()
        {
            return Err(format!("{path}: fields are not permitted"));
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(items) = schema.get("items") {
            for (i, item) in array.iter().enumerate() {
                validate_at(item, items, &format!("{path}[{i}]"))?;
            }
        }
    }

    Ok(())
}

/// Glob used by policy resource rules: `*` matches within a path segment,
/// `**` matches any remaining characters including `/`. All other bytes are literal.
pub fn glob_match(pattern: &str, value: &str) -> bool {
    glob_match_impl(pattern, value, false)
}

/// Commands have no path-segment boundary: every wildcard may match `/`.
pub(crate) fn glob_match_command(pattern: &str, value: &str) -> bool {
    glob_match_impl(pattern, value, true)
}

/// Rolling prefix-match rows bound time to O(pattern * value) and memory to O(value).
fn glob_match_impl(pattern: &str, value: &str, single_star_matches_slash: bool) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut previous = vec![false; value.len() + 1];
    let mut current = vec![false; value.len() + 1];
    previous[0] = true;
    let mut index = 0;
    while index < pattern.len() {
        if pattern[index] == b'*' {
            let double_star = pattern.get(index + 1) == Some(&b'*');
            let matches_slash = double_star || single_star_matches_slash;
            current[0] = previous[0];
            for j in 1..=value.len() {
                // Either match no bytes, or extend this wildcard by one byte.
                current[j] =
                    previous[j] || (current[j - 1] && (matches_slash || value[j - 1] != b'/'));
            }
            index += if double_star { 2 } else { 1 };
        } else {
            current[0] = false;
            for j in 1..=value.len() {
                current[j] = previous[j - 1] && pattern[index] == value[j - 1];
            }
            index += 1;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[value.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_objects_and_rejects_extra_fields() {
        let schema = json!({
            "type": "object",
            "properties": {"path": {"type": "string"}, "limit": {"type": "integer", "minimum": 1}},
            "required": ["path"],
            "additionalProperties": false
        });
        assert!(validate(&json!({"path": "a", "limit": 2}), &schema).is_ok());
        assert!(validate(&json!({"path": "a", "limit": 0}), &schema).is_err());
        assert!(validate(&json!({"limit": 2}), &schema).is_err());
        assert!(validate(&json!({"path": "a", "extra": true}), &schema).is_err());
    }

    #[test]
    fn globs_respect_segment_boundaries() {
        assert!(glob_match("repo:src/**", "repo:src/main.rs"));
        assert!(glob_match("repo:src/*", "repo:src/main.rs"));
        assert!(!glob_match("repo:src/*", "repo:src/sub/main.rs"));
        assert!(glob_match("repo:src/**/main.rs", "repo:src/sub/main.rs"));
        assert!(!glob_match("repo:src/**", "repo:tests/main.rs"));
        assert!(!glob_match("**/main.rs", "main.rs"));
        assert!(glob_match("file?.rs", "file?.rs"));
        assert!(!glob_match("file?.rs", "file1.rs"));
        assert!(glob_match("*/é.rs", "src/é.rs"));
    }

    #[test]
    fn many_double_stars_match_and_reject_without_backtracking() {
        let pattern = format!("{}b", "**a".repeat(64));
        assert!(!glob_match(&pattern, &format!("{}c", "a".repeat(128))));
        assert!(glob_match(&pattern, &format!("{}b", "a".repeat(128))));
    }

    #[test]
    fn globs_agree_with_small_reference_exhaustively() {
        // Deliberately bounded inputs let the original recursive semantics serve
        // as an independent reference, never as the production matching engine.
        fn reference(pattern: &[u8], value: &[u8]) -> bool {
            let Some(&first) = pattern.first() else {
                return value.is_empty();
            };
            if first == b'*' {
                let double = pattern.get(1) == Some(&b'*');
                let limit = if double {
                    value.len()
                } else {
                    value
                        .iter()
                        .position(|&byte| byte == b'/')
                        .unwrap_or(value.len())
                };
                let rest = &pattern[if double { 2 } else { 1 }..];
                return (0..=limit).any(|i| reference(rest, &value[i..]));
            }
            value.first() == Some(&first) && reference(&pattern[1..], &value[1..])
        }

        fn words(alphabet: &[char], max_len: usize) -> Vec<String> {
            let mut words = vec![String::new()];
            let mut start = 0;
            for _ in 0..max_len {
                let end = words.len();
                for i in start..end {
                    for letter in alphabet {
                        words.push(format!("{}{letter}", words[i]));
                    }
                }
                start = end;
            }
            words
        }

        let patterns = words(&['a', '/', '*', '?'], 5);
        let values = words(&['a', '/', '?'], 3);
        for pattern in &patterns {
            for value in &values {
                assert_eq!(
                    glob_match(pattern, value),
                    reference(pattern.as_bytes(), value.as_bytes()),
                    "pattern={pattern:?}, value={value:?}"
                );
            }
        }
    }

    #[test]
    fn supports_array_type_unions_and_boolean_schemas() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "opt": { "type": ["string", "null"] },
                "anything": true,
                "nothing": false
            }
        });
        assert!(validate(&serde_json::json!({"opt": "ok"}), &schema).is_ok());
        assert!(validate(&serde_json::json!({"opt": null}), &schema).is_ok());
        assert!(validate(&serde_json::json!({"opt": 42}), &schema).is_err());
        assert!(validate(&serde_json::json!({"anything": [1, 2]}), &schema).is_ok());
        assert!(validate(&serde_json::json!({"nothing": 1}), &schema).is_err());
    }
}
