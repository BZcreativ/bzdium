//! Tiny JSON-Schema builders for tool input schemas.
//!
//! bzdium deliberately carries no schema-derive dependency (no schemars): the
//! whole agent layer is built on crates already in the tree. These helpers
//! keep every tool schema consistent and force a description on every
//! property, which is what makes the surface usable by an LLM without guessing.

use serde_json::{json, Map, Value};

/// Builds an object schema. Every property must already carry a description
/// (see the prop helpers below).
pub fn object(props: Value, required: &[&str]) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), json!("object"));
    obj.insert("properties".into(), props);
    obj.insert(
        "additionalProperties".into(),
        Value::Bool(false),
    );
    if !required.is_empty() {
        obj.insert(
            "required".into(),
            Value::Array(required.iter().map(|s| json!(s)).collect()),
        );
    }
    Value::Object(obj)
}

pub fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

pub fn string_enum(description: &str, values: &[&str]) -> Value {
    json!({ "type": "string", "description": description, "enum": values })
}

pub fn boolean(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

pub fn integer(description: &str, min: Option<i64>, max: Option<i64>) -> Value {
    let mut v = json!({ "type": "integer", "description": description });
    if let Some(m) = min {
        v["minimum"] = json!(m);
    }
    if let Some(m) = max {
        v["maximum"] = json!(m);
    }
    v
}

pub fn string_array(description: &str) -> Value {
    json!({ "type": "array", "description": description, "items": { "type": "string" } })
}

/// The dryRun property mixed into every mutating tool.
pub fn dry_run_prop() -> Value {
    json!({
        "type": "boolean",
        "default": false,
        "description": "Validate the request and return the projected effect without committing it. Use this to verify a payload before making an irreversible change."
    })
}

/// Assembles a property map from name -> schema pairs.
pub fn props(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_schema_has_strict_shape() {
        let s = object(props(vec![("id", string("Service id"))]), &["id"]);
        assert_eq!(s["type"], "object");
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["required"][0], "id");
        assert_eq!(s["properties"]["id"]["type"], "string");
    }

    #[test]
    fn every_helper_emits_a_description() {
        let all = vec![
            string("d"),
            boolean("d"),
            string_array("d"),
            integer("d", Some(0), None),
            string_enum("d", &["a"]),
            dry_run_prop(),
        ];
        for v in all {
            assert!(
                v.get("description").and_then(|d| d.as_str()).is_some_and(|d| !d.is_empty()),
                "every schema fragment must carry a description: {v}"
            );
        }
    }

    #[test]
    fn integer_bounds_are_applied() {
        let v = integer("count", Some(0), Some(10));
        assert_eq!(v["minimum"], 0);
        assert_eq!(v["maximum"], 10);
    }

    #[test]
    fn no_required_key_when_empty() {
        let s = object(props(vec![]), &[]);
        assert!(s.get("required").is_none());
    }
}
