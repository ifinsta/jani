use serde_json::Value;

/// Minimal JSON Schema subset used by the broker. Unknown keywords are ignored,
/// but unsupported value types fail closed rather than being treated as valid.
pub fn validate(value: &Value, schema: &Value) -> Result<(), String> {
    validate_at(value, schema, "$")
}

fn validate_at(value: &Value, schema: &Value, path: &str) -> Result<(), String> {
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let type_ok = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            other => return Err(format!("{path}: unsupported schema type '{other}'")),
        };
        if !type_ok {
            return Err(format!("{path}: expected {expected}"));
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
/// `**` matches any remaining characters including `/`.
pub fn glob_match(pattern: &str, value: &str) -> bool {
    fn inner(p: &[u8], v: &[u8]) -> bool {
        if p.is_empty() {
            return v.is_empty();
        }
        if p[0] == b'*' {
            if p.get(1) == Some(&b'*') {
                let rest = &p[2..];
                for i in 0..=v.len() {
                    if inner(rest, &v[i..]) {
                        return true;
                    }
                }
                return false;
            }
            let rest = &p[1..];
            for i in 0..=v.len() {
                if i > 0 && v[i - 1] == b'/' {
                    break;
                }
                if inner(rest, &v[i..]) {
                    return true;
                }
            }
            return false;
        }
        if v.first().is_some_and(|c| *c == p[0]) {
            inner(&p[1..], &v[1..])
        } else {
            false
        }
    }
    inner(pattern.as_bytes(), value.as_bytes())
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
    }
}
