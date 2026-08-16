use crate::rules_spec::SpecEntry;
use crate::schema;
use crate::tsgo;
use std::sync::Arc;
use zen_engine::model::DecisionContent;
use zen_engine::policy::{ScopeRequest, Workspace};

/// Fills in the schemas a rule does not declare, by analysing the release as
/// one workspace. Input/output properties come from the engine; the TypeScript
/// return types of function nodes come from tsgo, which is what lets a graph
/// whose shape is only decided inside a function still publish an output
/// schema.
///
/// Declared schemas win — an author who wrote one on an input node meant it.
/// Every step degrades to leaving the entry as it was, so a rule the engine
/// cannot analyse still appears in the document with its declared shape.
pub fn enrich(entries: &mut [SpecEntry], documents: Vec<(Arc<str>, Arc<DecisionContent>)>) {
    if entries.is_empty() {
        return;
    }

    let mut workspace = Workspace::new();
    for (path, content) in documents {
        workspace.set_document_arc(path, content);
    }

    if let Some(analyzer) = tsgo::analyzer() {
        workspace.set_function_resolver(move |source, input| {
            analyzer
                .resolve(source, &schema::to_typescript(input))
                .map(|resolved| resolved.to_string())
        });
    }

    for entry in entries {
        let request = ScopeRequest::for_policy(entry.path.clone());
        let signature = workspace
            .graph_analysis(&entry.path)
            .map(|analysis| analysis.signature.clone());

        if entry.input_schema.is_none() {
            let inputs = workspace.inputs(&request);
            entry.input_schema = if inputs.is_empty() {
                signature
                    .as_ref()
                    .and_then(|signature| schema::from_type(&signature.input, &entry.path))
            } else {
                Some(schema::from_properties(
                    inputs
                        .iter()
                        .map(|input| (input.path.as_ref(), &input.resolved_type)),
                    &entry.path,
                ))
            }
            .map(Arc::new);
        }

        if entry.output_schema.is_none() {
            // Graphs have no per-property output list; their whole output type
            // is the graph signature, which is where a function node's tsgo
            // resolved type lands.
            let outputs = workspace.outputs(&request);
            entry.output_schema = if outputs.is_empty() {
                signature
                    .as_ref()
                    .and_then(|signature| schema::from_type(&signature.output, &entry.path))
            } else {
                Some(schema::from_properties(
                    outputs
                        .iter()
                        .map(|output| (output.path.as_ref(), &output.resolved_type)),
                    &entry.path,
                ))
            }
            .map(Arc::new);
        }

        // An unknown path still skeletons to `{}`, which says nothing.
        let skeleton = workspace.input_skeleton(&request);
        if skeleton.as_object().is_some_and(|map| !map.is_empty()) {
            entry.skeleton = Some(Arc::new(skeleton));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TsgoConfig;
    use crate::rules_spec::RuleKind;
    use serde_json::{Value, json};

    fn document(value: Value) -> Arc<DecisionContent> {
        Arc::new(serde_json::from_value(value).unwrap())
    }

    fn entry(path: &str) -> SpecEntry {
        SpecEntry {
            path: Arc::from(path),
            kind: RuleKind::Graph,
            title: None,
            description: None,
            content_hash: None,
            input_schema: None,
            output_schema: None,
            skeleton: None,
            examples: vec![],
        }
    }

    /// Nothing declared on the input node — the shape can only come from the
    /// engine's analysis of what the expression reads.
    fn expression_graph() -> Value {
        json!({
            "nodes": [
                { "id": "in", "name": "request", "type": "inputNode", "content": {} },
                {
                    "id": "expr", "name": "compute", "type": "expressionNode",
                    "content": { "expressions": [
                        { "id": "e1", "key": "total", "value": "cart.price * cart.quantity" }
                    ]}
                },
                { "id": "out", "name": "response", "type": "outputNode", "content": {} }
            ],
            "edges": [
                { "id": "e-a", "sourceId": "in", "targetId": "expr" },
                { "id": "e-b", "sourceId": "expr", "targetId": "out" }
            ]
        })
    }

    /// The output shape exists only inside TypeScript, so deriving it requires
    /// tsgo to have actually type-checked the handler.
    fn function_graph() -> Value {
        json!({
            "nodes": [
                { "id": "in", "name": "request", "type": "inputNode", "content": {
                    "schema": "{\"type\":\"object\",\"properties\":{\"amount\":{\"type\":\"number\"}},\"required\":[\"amount\"]}"
                }},
                { "id": "fn", "name": "score", "type": "functionNode", "content": {
                    "source": "export const handler = async (input) => ({ score: input.amount * 2, tier: 'gold' as const });"
                }},
                { "id": "out", "name": "response", "type": "outputNode", "content": {} }
            ],
            "edges": [
                { "id": "e-a", "sourceId": "in", "targetId": "fn" },
                { "id": "e-b", "sourceId": "fn", "targetId": "out" }
            ]
        })
    }

    fn enrich_one(path: &str, content: Value) -> SpecEntry {
        let mut entries = vec![entry(path)];
        enrich(&mut entries, vec![(Arc::from(path), document(content))]);
        entries.remove(0)
    }

    #[test]
    fn derives_input_schema_from_expression_references() {
        let entry = enrich_one("Pricing Rule", expression_graph());
        let input = entry
            .input_schema
            .as_deref()
            .expect("input schema derived from expression references");

        assert_eq!(input["$id"], json!("Pricing Rule"));
        assert_eq!(input["required"], json!(["cart"]));
        assert!(
            input["properties"]["cart"]["properties"]
                .get("price")
                .is_some(),
            "dotted reference cart.price nests: {input}"
        );
    }

    #[test]
    fn derives_skeleton_from_inferred_inputs() {
        let entry = enrich_one("Pricing Rule", expression_graph());

        assert_eq!(
            entry.skeleton.as_deref(),
            Some(&json!({ "cart": { "price": null, "quantity": null } }))
        );
    }

    /// The end-to-end proof that tsgo is wired in: `tier` is only a `"gold"`
    /// literal because the TypeScript compiler said so.
    #[test]
    fn derives_output_schema_through_tsgo() {
        tsgo::init(&TsgoConfig::default());

        let entry = enrich_one("Scoring", function_graph());
        let output = entry
            .output_schema
            .as_deref()
            .expect("output schema derived from the function node's return type");

        assert_eq!(output["properties"]["score"], json!({ "type": "number" }));
        assert_eq!(output["properties"]["tier"], json!({ "const": "gold" }));
        assert_eq!(output["required"], json!(["score", "tier"]));
    }

    #[test]
    fn declared_schemas_are_not_overwritten() {
        let declared = Arc::new(json!({ "type": "object", "properties": { "declared": {} } }));
        let mut entries = vec![SpecEntry {
            input_schema: Some(declared.clone()),
            ..entry("Pricing Rule")
        }];

        enrich(
            &mut entries,
            vec![(Arc::from("Pricing Rule"), document(expression_graph()))],
        );

        assert_eq!(
            entries[0].input_schema.as_deref(),
            Some(declared.as_ref()),
            "an author's declared schema wins over the derived one"
        );
    }

    /// An output the engine cannot pin down must stay absent rather than
    /// become an object that forbids every key.
    #[test]
    fn uninformative_types_leave_the_schema_unset() {
        let entry = enrich_one("Pricing Rule", expression_graph());

        assert_eq!(entry.output_schema, None);
    }

    #[test]
    fn unknown_paths_are_left_alone() {
        let mut entries = vec![entry("missing-rule")];
        enrich(&mut entries, vec![]);

        assert_eq!(entries[0].input_schema, None);
        assert_eq!(entries[0].output_schema, None);
        assert_eq!(entries[0].skeleton, None);
    }

    #[test]
    fn empty_entries_short_circuit() {
        let mut entries: Vec<SpecEntry> = vec![];
        enrich(&mut entries, vec![]);
    }
}
