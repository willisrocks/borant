//! Task schemas (spec section 8): JSON Schema for the properties of a task.

use serde_json::{Map, Value, json};

use crate::error::CoreError;

pub fn default_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "branch": { "type": "string" },
            "pr_url": { "type": "string" },
            "summary": { "type": "string" },
            "files": { "type": "array", "items": { "type": "string" } }
        },
        "additionalProperties": false
    })
}

/// Checks that a schema compiles and describes an object with `properties`.
pub fn check_schema(schema: &Value) -> Result<(), CoreError> {
    if !schema.get("properties").is_some_and(Value::is_object) {
        return Err(CoreError::InvalidSchema(
            "the schema must be an object schema with a 'properties' object".into(),
        ));
    }
    jsonschema::validator_for(schema)
        .map(|_| ())
        .map_err(|e| CoreError::InvalidSchema(e.to_string()))
}

pub fn validate(schema: &Value, properties: &Map<String, Value>) -> Result<(), CoreError> {
    let validator =
        jsonschema::validator_for(schema).map_err(|e| CoreError::InvalidSchema(e.to_string()))?;
    let instance = Value::Object(properties.clone());
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|e| {
            let at = e.instance_path().to_string();
            if at.is_empty() {
                e.to_string()
            } else {
                format!("{at}: {e}")
            }
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(CoreError::InvalidProperties { errors })
    }
}

/// Adds keys to a schema. A change can only add keys.
pub fn extend(schema: &Value, add: &Map<String, Value>) -> Result<Value, CoreError> {
    let mut out = schema.clone();
    let props = out
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| CoreError::InvalidSchema("the schema has no 'properties' object".into()))?;
    for (key, def) in add {
        if props.contains_key(key) {
            return Err(CoreError::SchemaChange { key: key.clone() });
        }
        props.insert(key.clone(), def.clone());
    }
    check_schema(&out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn default_accepts_known_keys() {
        let p = props(json!({"branch": "main", "files": ["a.rs"]}));
        assert!(validate(&default_schema(), &p).is_ok());
    }

    #[test]
    fn default_rejects_unknown_key() {
        let p = props(json!({"colour": "blue"}));
        let err = validate(&default_schema(), &p).unwrap_err();
        assert!(matches!(err, CoreError::InvalidProperties { .. }), "{err}");
        assert!(err.to_string().contains("colour"), "{err}");
    }

    #[test]
    fn default_rejects_wrong_type() {
        let p = props(json!({"files": "a.rs"}));
        let err = validate(&default_schema(), &p).unwrap_err();
        assert!(err.to_string().contains("/files"), "{err}");
    }

    #[test]
    fn extend_adds_keys() {
        let s = extend(
            &default_schema(),
            &props(json!({"estimate": {"type": "integer"}})),
        )
        .unwrap();
        assert!(validate(&s, &props(json!({"estimate": 3}))).is_ok());
    }

    #[test]
    fn extend_rejects_existing_key() {
        let err = extend(
            &default_schema(),
            &props(json!({"branch": {"type": "integer"}})),
        )
        .unwrap_err();
        assert_eq!(
            err,
            CoreError::SchemaChange {
                key: "branch".into()
            }
        );
    }

    #[test]
    fn check_schema_rejects_non_object_schema() {
        assert!(check_schema(&json!({"type": "string"})).is_err());
    }
}
