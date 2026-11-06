use crate::data::release_data::{MetaKind, ReleaseData};
use serde_json::{Value, json};
use std::ops::Deref;
use std::sync::Arc;

/// One example harvested from a test file, attached to the rule its
/// `filePath` points at.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecExample {
    pub name: Arc<str>,
    pub input: Value,
    pub source_test_file: Arc<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Graph,
    Policy,
}

impl RuleKind {
    fn as_str(self) -> &'static str {
        match self {
            RuleKind::Graph => "graph",
            RuleKind::Policy => "policy",
        }
    }
}

/// Everything the OpenAPI document builder needs to know about one rule.
#[derive(Debug, Clone)]
pub struct SpecEntry {
    /// Original-cased path (falls back to the lowercased evaluation key).
    pub path: Arc<str>,
    pub kind: RuleKind,
    pub title: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    /// Immutable content identity when the source stores one (file versionId).
    pub content_hash: Option<Arc<str>>,
    pub input_schema: Option<Arc<Value>>,
    pub output_schema: Option<Arc<Value>>,
    /// Example request body shaped from the derived input type.
    pub skeleton: Option<Arc<Value>>,
    pub examples: Vec<SpecExample>,
}

/// What one document is generated from: the project reference used in
/// URLs/titles (project key, or the request's path segment when the release
/// metadata is absent) plus the loaded release and its rules.
pub struct RulesDocumentSource<'a> {
    pub project_ref: &'a str,
    pub release: Option<&'a ReleaseData>,
    pub entries: &'a [SpecEntry],
}

/// Rust port of BRMS `buildRulesOpenApiDocument` (rules-openapi.ts), minus
/// compiled schemas/skeletons (business-edition engine) and policies.
pub fn build_rules_openapi(source: RulesDocumentSource) -> Value {
    let version = source
        .release
        .and_then(|r| r.release_version())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "1.0.0".to_string());

    let meta = meta_schema(source.release);

    let mut paths = serde_json::Map::new();
    for entry in source.entries {
        paths.insert(
            format!("/evaluate/{}", entry.path),
            operation_for(entry, meta.as_ref()),
        );
    }

    let source_descriptor = match source.release.and_then(|r| r.meta_kind()) {
        Some(MetaKind::Environment) => {
            let release_data = source
                .release
                .expect("environment kind implies release data");
            json!({
                "type": "environment",
                "environmentId": release_data.environment_id().map(Deref::deref),
                "environmentKey": release_data
                    .environment
                    .as_ref()
                    .and_then(|e| e.key.as_deref()),
                "releaseId": release_data.release_id().map(Deref::deref),
            })
        }
        _ => json!({
            "type": "release",
            "releaseId": source.release.and_then(|r| r.release_id()).map(Deref::deref),
            "version": source.release.and_then(|r| r.release_version()).map(Deref::deref),
        }),
    };

    json!({
        "openapi": "3.0.3",
        "info": {
            "title": format!("{} rules API", source.project_ref),
            "version": version,
        },
        "x-gorules": {
            "source": source_descriptor
        },
        "servers": [{
            "url": format!("/api/rules/{}", source.project_ref),
            "description": format!("Deployed release {version}"),
        }],
        "security": [{ "accessToken": [] }],
        "components": {
            "securitySchemes": {
                "accessToken": { "type": "apiKey", "in": "header", "name": "X-Access-Token" }
            }
        },
        "paths": paths,
    })
}

/// Rust port of BRMS `evaluateMetaSchema` (rules-openapi.ts); the main/branch/
/// commit branches are unreachable on the agent and not ported.
fn meta_schema(release: Option<&ReleaseData>) -> Option<Value> {
    let release_data = release?;
    let kind = release_data.meta_kind()?;

    let type_value = match kind {
        MetaKind::Environment => "environment",
        MetaKind::Release => "release",
    };

    let mut required = vec!["type", "path", "project", "release"];
    let mut properties = json!({
        "type": {
            "type": "string",
            "enum": [type_value],
            "description": "Evaluation target: main | branch | commit | release | environment"
        },
        "path": { "type": "string", "description": "Rule file path that was evaluated" },
        "project": {
            "type": "object",
            "required": ["id", "key", "name"],
            "properties": {
                "id": { "type": "string", "format": "uuid" },
                "key": { "type": "string", "nullable": true },
                "name": { "type": "string" }
            }
        },
        "release": {
            "type": "object",
            "description": "Release the rule was evaluated from",
            "required": ["id"],
            "properties": {
                "id": { "type": "string", "format": "uuid" },
                "name": { "type": "string", "nullable": true },
                "version": {
                    "type": "string",
                    "nullable": true,
                    "description": "Semantic version; null for draft releases"
                },
                "status": {
                    "type": "string",
                    "enum": ["draft", "published"],
                    "description": "Draft releases carry no version; publishing assigns one"
                },
                "commitId": {
                    "type": "string",
                    "format": "uuid",
                    "nullable": true,
                    "description": "Commit the release was created from"
                }
            }
        }
    });

    if kind == MetaKind::Environment {
        properties["environment"] = json!({
            "type": "object",
            "required": ["id", "key", "name"],
            "properties": {
                "id": { "type": "string", "format": "uuid" },
                "key": { "type": "string", "nullable": true },
                "name": { "type": "string" }
            }
        });
        required.push("environment");
    }

    Some(json!({
        "type": "object",
        "description": "Details of the evaluation target: who evaluated what, and against which content",
        "required": required,
        "properties": properties
    }))
}

fn operation_for(entry: &SpecEntry, meta_schema: Option<&Value>) -> Value {
    let mut examples = serde_json::Map::new();
    for (index, example) in entry.examples.iter().enumerate() {
        examples.insert(
            format!("example_{}", index + 1),
            json!({
                "summary": example.name.deref(),
                "value": { "context": example.input },
                "x-source": example.source_test_file.deref(),
            }),
        );
    }

    let mut request_content = json!({
        "schema": {
            "type": "object",
            "required": ["context"],
            "properties": {
                "context": to_openapi_schema(entry.input_schema.as_deref()),
                "trace": { "type": "boolean", "description": "Include decision trace in response" }
            }
        }
    });
    if !examples.is_empty() {
        request_content["examples"] = Value::Object(examples);
    }

    let mut response_schema = json!({
        "type": "object",
        "properties": {
            "performance": { "type": "string" },
            "result": to_openapi_schema(entry.output_schema.as_deref()),
            "trace": { "type": "object", "description": "Present when the request body sets trace=true" }
        }
    });
    if let Some(meta) = meta_schema {
        response_schema["required"] = json!(["performance", "result", "meta"]);
        response_schema["properties"]["meta"] = meta.clone();
    }

    let mut gorules = json!({
        "path": entry.path.deref(),
        "kind": entry.kind.as_str(),
        "contentHash": entry.content_hash.as_deref(),
        "hasInputSchema": entry.input_schema.is_some(),
        "hasOutputSchema": entry.output_schema.is_some(),
    });
    if let Some(skeleton) = &entry.skeleton {
        gorules["skeleton"] = skeleton.as_ref().clone();
    }

    let mut post = json!({
        "operationId": operation_id(&entry.path),
        "summary": entry.title.as_deref().unwrap_or_else(|| file_name(&entry.path)),
        "tags": [entry.kind.as_str()],
        "x-gorules": gorules,
        "requestBody": {
            "required": true,
            "content": { "application/json": request_content }
        },
        "responses": {
            "200": {
                "description": "Evaluation result",
                "content": { "application/json": { "schema": response_schema } }
            }
        }
    });
    if let Some(description) = &entry.description {
        post["description"] = json!(description.deref());
    }

    json!({ "post": post })
}

/// `evaluate_` + path with non-alphanumeric runs collapsed to `_`, trimmed.
fn operation_id(path: &str) -> String {
    let sanitized = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>();
    let collapsed = sanitized
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    format!("evaluate_{collapsed}")
}

/// Embed a declared JSON schema, dropping metadata keys OpenAPI 3.0 rejects;
/// generic object when nothing (usable) is declared.
fn to_openapi_schema(schema: Option<&Value>) -> Value {
    let Some(Value::Object(map)) = schema else {
        return json!({ "type": "object" });
    };

    let mut map = map.clone();
    map.remove("$schema");
    map.remove("$id");
    Value::Object(map)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::release_data::{
        ReleaseData, ReleaseDataEnvironment, ReleaseDataProject, ReleaseDataRelease,
    };
    use serde_json::json;

    fn release() -> ReleaseData {
        ReleaseData {
            version: Some(Arc::from("1")),
            project: Some(ReleaseDataProject {
                id: Some(Arc::from("11111111-1111-1111-1111-111111111111")),
                key: Some(Arc::from("rules-project")),
                name: None,
            }),
            access_tokens: vec![Arc::from("secret-token")],
            release: Some(ReleaseDataRelease {
                id: Some(Arc::from("22222222-2222-2222-2222-222222222222")),
                version: Some(Arc::from("1.2.3")),
                name: None,
                status: None,
                commit_id: None,
            }),
            environment: None,
        }
    }

    fn environment_release() -> ReleaseData {
        let mut data = release();
        data.environment = Some(ReleaseDataEnvironment {
            id: Some(Arc::from("77777777-7777-7777-7777-777777777777")),
            key: Some(Arc::from("production")),
            name: Some(Arc::from("Production")),
        });
        data
    }

    fn entries() -> Vec<SpecEntry> {
        vec![
            SpecEntry {
                path: Arc::from("Pricing Rule"),
                kind: RuleKind::Graph,
                title: Some(Arc::from("Pricing")),
                description: Some(Arc::from("Cart pricing rules")),
                content_hash: Some(Arc::from("3a5b")),
                input_schema: Some(Arc::new(json!({
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "type": "object",
                    "properties": { "cartTotal": { "type": "number" } }
                }))),
                output_schema: Some(Arc::new(json!({ "type": "object" }))),
                skeleton: Some(Arc::new(json!({ "cartTotal": 0 }))),
                examples: vec![SpecExample {
                    name: Arc::from("small cart"),
                    input: json!({ "cartTotal": 10 }),
                    source_test_file: Arc::from("Pricing Rule.test"),
                }],
            },
            SpecEntry {
                path: Arc::from("plain-rule"),
                kind: RuleKind::Policy,
                title: None,
                description: None,
                content_hash: None,
                input_schema: None,
                output_schema: None,
                skeleton: None,
                examples: vec![],
            },
        ]
    }

    #[test]
    fn document_shape_with_release() {
        let release = release();
        let entries = entries();
        let document = build_rules_openapi(RulesDocumentSource {
            project_ref: "rules-project",
            release: Some(&release),
            entries: &entries,
        });

        assert_eq!(document["openapi"], json!("3.0.3"));
        assert_eq!(document["info"]["title"], json!("rules-project rules API"));
        assert_eq!(document["info"]["version"], json!("1.2.3"));
        assert_eq!(
            document["x-gorules"]["source"],
            json!({
                "type": "release",
                "releaseId": "22222222-2222-2222-2222-222222222222",
                "version": "1.2.3"
            })
        );
        assert_eq!(
            document["servers"][0]["url"],
            json!("/api/rules/rules-project")
        );
        assert_eq!(
            document["servers"][0]["description"],
            json!("Deployed release 1.2.3")
        );
        assert_eq!(document["security"], json!([{ "accessToken": [] }]));
        assert_eq!(
            document["components"]["securitySchemes"]["accessToken"],
            json!({ "type": "apiKey", "in": "header", "name": "X-Access-Token" })
        );
    }

    #[test]
    fn rich_entry_operation() {
        let release = release();
        let entries = entries();
        let document = build_rules_openapi(RulesDocumentSource {
            project_ref: "rules-project",
            release: Some(&release),
            entries: &entries,
        });

        let post = &document["paths"]["/evaluate/Pricing Rule"]["post"];
        assert_eq!(post["operationId"], json!("evaluate_Pricing_Rule"));
        assert_eq!(post["summary"], json!("Pricing"));
        assert_eq!(post["description"], json!("Cart pricing rules"));
        assert_eq!(post["tags"], json!(["graph"]));
        assert_eq!(
            post["x-gorules"],
            json!({
                "path": "Pricing Rule",
                "kind": "graph",
                "contentHash": "3a5b",
                "hasInputSchema": true,
                "hasOutputSchema": true,
                "skeleton": { "cartTotal": 0 }
            })
        );

        let content = &post["requestBody"]["content"]["application/json"];
        assert_eq!(
            content["schema"]["properties"]["context"],
            json!({ "type": "object", "properties": { "cartTotal": { "type": "number" } } }),
            "$schema key stripped, declared schema propagated"
        );
        assert_eq!(content["schema"]["required"], json!(["context"]));
        assert_eq!(
            content["examples"]["example_1"],
            json!({
                "summary": "small cart",
                "value": { "context": { "cartTotal": 10 } },
                "x-source": "Pricing Rule.test"
            })
        );

        let response_schema = &post["responses"]["200"]["content"]["application/json"]["schema"];
        assert_eq!(
            response_schema["properties"]["performance"],
            json!({ "type": "string" })
        );
        assert_eq!(
            response_schema["properties"]["result"],
            json!({ "type": "object" })
        );
        assert_eq!(
            response_schema["required"],
            json!(["performance", "result", "meta"])
        );
        assert_eq!(
            response_schema["properties"]["trace"],
            json!({ "type": "object", "description": "Present when the request body sets trace=true" })
        );
        let meta = &response_schema["properties"]["meta"];
        assert_eq!(meta["properties"]["type"]["enum"], json!(["release"]));
        assert_eq!(
            meta["required"],
            json!(["type", "path", "project", "release"])
        );
        assert!(meta["properties"].get("environment").is_none());
        assert_eq!(meta["properties"]["release"]["required"], json!(["id"]));
    }

    #[test]
    fn environment_config_document() {
        let release = environment_release();
        let entries = entries();
        let document = build_rules_openapi(RulesDocumentSource {
            project_ref: "rules-project",
            release: Some(&release),
            entries: &entries,
        });

        assert_eq!(
            document["x-gorules"]["source"],
            json!({
                "type": "environment",
                "environmentId": "77777777-7777-7777-7777-777777777777",
                "environmentKey": "production",
                "releaseId": "22222222-2222-2222-2222-222222222222"
            })
        );

        let meta = &document["paths"]["/evaluate/plain-rule"]["post"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["properties"]["meta"];
        assert_eq!(meta["properties"]["type"]["enum"], json!(["environment"]));
        assert_eq!(
            meta["required"],
            json!(["type", "path", "project", "release", "environment"])
        );
        assert_eq!(
            meta["properties"]["environment"]["required"],
            json!(["id", "key", "name"])
        );
    }

    #[test]
    fn bare_entry_and_no_release_fallbacks() {
        let entries = entries();
        let document = build_rules_openapi(RulesDocumentSource {
            project_ref: "some-project",
            release: None,
            entries: &entries,
        });

        assert_eq!(document["info"]["version"], json!("1.0.0"));
        assert_eq!(
            document["x-gorules"]["source"],
            json!({ "type": "release", "releaseId": null, "version": null })
        );

        let post = &document["paths"]["/evaluate/plain-rule"]["post"];
        assert_eq!(
            post["summary"],
            json!("plain-rule"),
            "summary falls back to file name"
        );
        assert_eq!(post.get("description"), None);
        assert_eq!(post["x-gorules"]["contentHash"], json!(null));
        assert_eq!(post["x-gorules"]["hasInputSchema"], json!(false));
        assert_eq!(post["x-gorules"]["hasOutputSchema"], json!(false));
        assert_eq!(
            post["requestBody"]["content"]["application/json"]["schema"]["properties"]["context"],
            json!({ "type": "object" }),
            "generic object fallback"
        );
        assert_eq!(
            post["requestBody"]["content"]["application/json"].get("examples"),
            None,
            "no examples key when none exist"
        );
        assert_eq!(
            post["responses"]["200"]["content"]["application/json"]["schema"]["properties"]["result"],
            json!({ "type": "object" }),
            "response schema falls back to generic object"
        );
        let response_schema = &post["responses"]["200"]["content"]["application/json"]["schema"];
        assert_eq!(
            response_schema.get("required"),
            None,
            "no required without meta"
        );
        assert!(response_schema["properties"].get("meta").is_none());
        assert_eq!(
            response_schema["properties"]["trace"]["type"],
            json!("object")
        );
    }

    #[test]
    fn operation_id_sanitization() {
        assert_eq!(
            operation_id("first level/nested-sample"),
            "evaluate_first_level_nested_sample"
        );
        assert_eq!(operation_id("__weird__ path!!"), "evaluate_weird_path");
    }
}
