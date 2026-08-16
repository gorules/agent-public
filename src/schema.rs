use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use zen_expression::variable::VariableType;

fn quoted(value: &str) -> String {
    Value::String(value.to_string()).to_string()
}

/// Renders an engine type as the TypeScript source tsgo type-checks against.
/// JSON string syntax is a subset of TypeScript's, so quoting through serde
/// produces valid literals and property keys.
pub fn to_typescript(variable_type: &VariableType) -> String {
    match variable_type {
        VariableType::Any => "any".to_string(),
        VariableType::Null => "null".to_string(),
        VariableType::Bool => "boolean".to_string(),
        VariableType::String => "string".to_string(),
        VariableType::Number => "number".to_string(),
        VariableType::Date => "Date".to_string(),
        VariableType::Interval => "string".to_string(),
        VariableType::Const(value) => quoted(value),
        VariableType::Enum(_, values) if values.is_empty() => "never".to_string(),
        VariableType::Enum(_, values) => values
            .iter()
            .map(|value| quoted(value))
            .collect::<Vec<_>>()
            .join(" | "),
        VariableType::Array(items) => format!("Array<{}>", to_typescript(items)),
        VariableType::Object(fields) => {
            let fields = fields.borrow();
            let mut keys: Vec<_> = fields.keys().collect();
            keys.sort();

            let body = keys
                .into_iter()
                .map(|key| {
                    let field = fields.get(key).expect("key came from this map");
                    format!("{}: {};", quoted(key), to_typescript(field))
                })
                .collect::<Vec<_>>()
                .join(" ");

            if body.is_empty() {
                "{}".to_string()
            } else {
                format!("{{ {body} }}")
            }
        }
        VariableType::Nullable(inner) => {
            format!("({} | null | undefined)", to_typescript(inner))
        }
    }
}

fn leaf_schema(variable_type: &VariableType) -> Value {
    match variable_type {
        VariableType::Any => json!({}),
        VariableType::Null => json!({ "type": "null" }),
        VariableType::Bool => json!({ "type": "boolean" }),
        VariableType::String => json!({ "type": "string" }),
        VariableType::Number => json!({ "type": "number" }),
        VariableType::Date => json!({ "type": "string", "format": "date-time" }),
        VariableType::Interval => json!({ "type": "string" }),
        VariableType::Const(value) => json!({ "const": value.as_ref() }),
        VariableType::Enum(name, values) => {
            let mut schema = json!({
                "enum": values.iter().map(|v| v.as_ref()).collect::<Vec<_>>()
            });
            if let Some(name) = name {
                schema["title"] = json!(name.as_ref());
            }
            schema
        }
        VariableType::Array(items) => json!({ "type": "array", "items": leaf_schema(items) }),
        VariableType::Object(fields) => {
            let fields = fields.borrow();
            let mut properties = Map::new();
            let mut required = BTreeSet::new();

            for (key, field) in fields.iter() {
                properties.insert(key.to_string(), leaf_schema(field));
                if !matches!(field, VariableType::Nullable(_)) {
                    required.insert(key.to_string());
                }
            }

            let mut schema = json!({
                "type": "object",
                "properties": properties,
                "additionalProperties": false,
            });
            if !required.is_empty() {
                schema["required"] = json!(required.into_iter().collect::<Vec<_>>());
            }
            schema
        }
        VariableType::Nullable(inner) => {
            json!({ "anyOf": [leaf_schema(inner), { "type": "null" }] })
        }
    }
}

fn set_at_path(root: &mut Value, path: &[&str], leaf: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };

    let mut cursor = root;
    for segment in parents {
        if cursor.get("type").and_then(Value::as_str) != Some("object") {
            cursor["type"] = json!("object");
            cursor["additionalProperties"] = json!(false);
        }
        if !cursor["properties"].is_object() {
            cursor["properties"] = json!({});
        }

        let properties = &mut cursor["properties"];
        if !properties[*segment].is_object() {
            properties[*segment] = json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            });
        }
        cursor = &mut properties[*segment];
    }

    if !cursor["properties"].is_object() {
        cursor["properties"] = json!({});
    }
    cursor["properties"][*last] = leaf;
}

/// Wraps a whole resolved type as a root schema. `None` when the type carries
/// no information — `any`, or an object the engine could not give any fields —
/// so the caller can leave the entry's schema unset rather than publishing an
/// `additionalProperties: false` object that forbids every key.
pub fn from_type(variable_type: &VariableType, id: &str) -> Option<Value> {
    match variable_type {
        VariableType::Any => return None,
        VariableType::Object(fields) if fields.borrow().is_empty() => return None,
        _ => {}
    }

    let mut schema = leaf_schema(variable_type);
    let map = schema.as_object_mut()?;

    map.insert(
        "$schema".to_string(),
        json!("http://json-schema.org/draft-07/schema#"),
    );
    map.insert("$id".to_string(), json!(id));
    Some(schema)
}

/// Rust port of BRMS `buildSchemaFromProperties` (json-schema.ts): stitches the
/// engine's dotted property paths back into a nested object schema.
pub fn from_properties<'a>(
    properties: impl IntoIterator<Item = (&'a str, &'a VariableType)>,
    id: &str,
) -> Value {
    let mut root = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$id": id,
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    });

    let mut required = BTreeSet::new();
    for (path, variable_type) in properties {
        let segments: Vec<&str> = path.split('.').collect();
        set_at_path(&mut root, &segments, leaf_schema(variable_type));

        if let Some(first) = segments.first()
            && !matches!(variable_type, VariableType::Nullable(_))
        {
            required.insert(first.to_string());
        }
    }

    if !required.is_empty() {
        root["required"] = json!(required.into_iter().collect::<Vec<_>>());
    }

    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn object(fields: &[(&str, VariableType)]) -> VariableType {
        let map = fields
            .iter()
            .map(|(key, value)| (Rc::from(*key), value.clone()))
            .collect();
        VariableType::Object(Rc::new(RefCell::new(map)))
    }

    #[test]
    fn scalar_typescript_rendering() {
        assert_eq!(to_typescript(&VariableType::Bool), "boolean");
        assert_eq!(to_typescript(&VariableType::Date), "Date");
        assert_eq!(to_typescript(&VariableType::Interval), "string");
        assert_eq!(to_typescript(&VariableType::Const(Rc::from("a"))), "\"a\"");
        assert_eq!(
            to_typescript(&VariableType::Enum(
                None,
                vec![Rc::from("a"), Rc::from("b")]
            )),
            "\"a\" | \"b\""
        );
        assert_eq!(
            to_typescript(&VariableType::Nullable(Rc::new(VariableType::Number))),
            "(number | null | undefined)"
        );
    }

    #[test]
    fn object_typescript_is_key_sorted_and_quoted() {
        let rendered = to_typescript(&object(&[
            ("b", VariableType::Number),
            ("a", VariableType::String),
            ("needs quoting", VariableType::Bool),
        ]));

        assert_eq!(
            rendered,
            "{ \"a\": string; \"b\": number; \"needs quoting\": boolean; }"
        );
    }

    #[test]
    fn nested_array_typescript() {
        let rendered = to_typescript(&VariableType::Array(Rc::new(object(&[(
            "id",
            VariableType::Number,
        )]))));

        assert_eq!(rendered, "Array<{ \"id\": number; }>");
    }

    #[test]
    fn dotted_paths_nest_into_objects() {
        let cart = VariableType::Number;
        let name = VariableType::String;
        let schema = from_properties(
            [("customer.name", &name), ("cart.total", &cart)],
            "Pricing Rule",
        );

        assert_eq!(schema["$id"], json!("Pricing Rule"));
        assert_eq!(schema["required"], json!(["cart", "customer"]));
        assert_eq!(
            schema["properties"]["customer"]["properties"]["name"],
            json!({ "type": "string" })
        );
        assert_eq!(
            schema["properties"]["cart"]["properties"]["total"],
            json!({ "type": "number" })
        );
        assert_eq!(
            schema["properties"]["customer"]["additionalProperties"],
            json!(false)
        );
    }

    #[test]
    fn nullable_top_level_properties_are_not_required() {
        let optional = VariableType::Nullable(Rc::new(VariableType::String));
        let required = VariableType::Number;
        let schema = from_properties([("note", &optional), ("amount", &required)], "r");

        assert_eq!(schema["required"], json!(["amount"]));
        assert_eq!(
            schema["properties"]["note"],
            json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
        );
    }

    #[test]
    fn object_leaf_marks_non_nullable_fields_required() {
        let value = object(&[
            ("id", VariableType::Number),
            (
                "note",
                VariableType::Nullable(Rc::new(VariableType::String)),
            ),
        ]);
        let schema = from_properties([("customer", &value)], "r");

        let customer = &schema["properties"]["customer"];
        assert_eq!(customer["required"], json!(["id"]));
        assert_eq!(customer["properties"]["id"], json!({ "type": "number" }));
    }

    #[test]
    fn any_becomes_an_open_schema() {
        let any = VariableType::Any;
        let schema = from_properties([("passthrough", &any)], "r");

        assert_eq!(schema["properties"]["passthrough"], json!({}));
    }
}
