use std::collections::HashMap;
use std::ffi::OsStr;
use std::future::Future;
use std::io::{Read, Seek};
use std::ops::{Deref, DerefMut};
use std::os::unix::ffi::OsStrExt;
use std::path::Component;
use std::pin::Pin;
use std::sync::Arc;

use crate::data::extended_decision::{FileContent, FileDecisionGraph, FileTestContent};
use crate::data::release_data::ReleaseData;
use crate::rules_spec::{RuleKind, SpecEntry, SpecExample};
use anyhow::anyhow;
use zen_engine::DecisionEngine;
use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zen_engine::model::{DecisionContent, DecisionNodeKind};
use zip::ZipArchive;
use zip::read::ZipFile;
use zip::result::ZipResult;

#[derive(Default, Debug)]
pub struct ImmutableLoader {
    release_data: Option<ReleaseData>,
    content: HashMap<String, FileDecisionGraph>,
    examples: HashMap<String, Vec<SpecExample>>,
}

impl ImmutableLoader {
    pub fn new(
        content: HashMap<String, FileDecisionGraph>,
        examples: HashMap<String, Vec<SpecExample>>,
        release_data: Option<ReleaseData>,
    ) -> Self {
        Self {
            content,
            examples,
            release_data,
        }
    }

    pub fn into_engine(self) -> DecisionEngine {
        DecisionEngine::default().with_loader(Arc::new(self))
    }

    pub fn release_data(&self) -> Option<&ReleaseData> {
        self.release_data.as_ref()
    }

    pub fn get_version(&self, path: &str) -> Option<Arc<str>> {
        self.content.get(path)?.meta.version_id.clone()
    }

    pub fn decision_keys(&self) -> Vec<String> {
        self.content.keys().cloned().collect()
    }

    pub fn can_access(&self, token: &str) -> bool {
        self.release_data()
            .map(|rd| rd.access_tokens.iter().any(|at| at.deref().eq(token)))
            .unwrap_or(true)
    }

    fn display_path(key: &str, graph: &FileDecisionGraph) -> Arc<str> {
        graph
            .meta
            .display_path
            .clone()
            .unwrap_or_else(|| Arc::from(key))
    }

    /// Original-cased paths paired with their content, for workspace-wide
    /// analysis. Keyed by display path rather than the lowercased evaluation
    /// key because that is how graphs reference one another.
    pub fn documents(&self) -> Vec<(Arc<str>, Arc<DecisionContent>)> {
        self.content
            .iter()
            .map(|(key, graph)| (Self::display_path(key, graph), graph.content.clone()))
            .collect()
    }

    pub fn spec_entries(&self) -> Vec<SpecEntry> {
        let entries = self
            .content
            .iter()
            .map(|(key, graph)| {
                let mut input_schema = None;
                let mut output_schema = None;
                let mut kind = RuleKind::Policy;
                if let Some(graph_content) = graph.content.as_graph() {
                    kind = RuleKind::Graph;
                    for node in &graph_content.nodes {
                        match &node.kind {
                            DecisionNodeKind::InputNode { content } if input_schema.is_none() => {
                                input_schema = content.schema.clone();
                            }
                            DecisionNodeKind::OutputNode { content } if output_schema.is_none() => {
                                output_schema = content.schema.clone();
                            }
                            _ => {}
                        }
                    }
                }

                SpecEntry {
                    path: Self::display_path(key, graph),
                    kind,
                    title: graph.title.clone(),
                    description: graph.description.clone(),
                    content_hash: graph.meta.version_id.clone(),
                    input_schema,
                    output_schema,
                    skeleton: None,
                    examples: self
                        .examples
                        .get(&key.to_lowercase())
                        .cloned()
                        .unwrap_or_default(),
                }
            })
            .collect::<Vec<_>>();

        // No need to sort: `paths` is built into a `serde_json::Map`, which is a
        // BTreeMap here (this crate does not enable serde_json's `preserve_order`
        // feature), so the emitted document is ordered by path regardless of the
        // Vec's order.
        entries
    }
}

/// Examples are a sample, not the suite — a rule can carry hundreds of cases.
const MAX_EXAMPLES_PER_RULE: usize = 3;

/// Turn parsed test files (with their original source path) into per-rule
/// example lists keyed by lowercased target `filePath`.
pub(crate) fn collect_examples(
    test_files: Vec<(String, FileTestContent)>,
) -> HashMap<String, Vec<SpecExample>> {
    let mut examples: HashMap<String, Vec<SpecExample>> = HashMap::new();

    for (source_path, test) in test_files {
        let Some(file_path) = &test.file_path else {
            continue;
        };
        if test.disabled {
            continue;
        }

        let source: Arc<str> = Arc::from(source_path.as_str());
        let list = examples.entry(file_path.to_lowercase()).or_default();
        for case in &test.test_cases {
            if list.len() >= MAX_EXAMPLES_PER_RULE {
                break;
            }
            if case.disabled {
                continue;
            }

            list.push(SpecExample {
                name: case.name.clone().unwrap_or_else(|| source.clone()),
                input: case.input.clone(),
                source_test_file: source.clone(),
            });
        }
    }

    examples
}

impl DecisionLoader for ImmutableLoader {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + Send + 'a>> {
        Box::pin(async move {
            let lower_key = key.to_lowercase();
            let Some(data) = self.content.get(lower_key.as_str()) else {
                return Err(LoaderError::NotFound(lower_key));
            };

            Ok(data.content.clone())
        })
    }
}

pub struct ProtectedZipArchive<R> {
    pub password: Option<Arc<str>>,
    pub archive: ZipArchive<R>,
}

impl<R> Deref for ProtectedZipArchive<R> {
    type Target = ZipArchive<R>;

    fn deref(&self) -> &Self::Target {
        &self.archive
    }
}

impl<R> DerefMut for ProtectedZipArchive<R> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.archive
    }
}

impl<R> ProtectedZipArchive<R>
where
    R: Read + Seek,
{
    pub fn by_index_try_decrypt(&mut self, file_number: usize) -> ZipResult<ZipFile<'_, R>> {
        let Some(password) = self.password.clone() else {
            return self.by_index(file_number);
        };

        let is_ok = self
            .by_index_decrypt(file_number, password.as_bytes())
            .is_ok();
        if is_ok {
            self.by_index_decrypt(file_number, password.as_bytes())
        } else {
            self.by_index(file_number)
        }
    }
}

// Sync
impl<R> TryFrom<ProtectedZipArchive<R>> for ImmutableLoader
where
    R: Read + Seek,
{
    type Error = anyhow::Error;

    fn try_from(mut archive: ProtectedZipArchive<R>) -> Result<Self, Self::Error> {
        let config_prefix = ".config";

        let release_data = archive
            .index_for_name(".config/project.json")
            .map(|index| archive.by_index_try_decrypt(index).ok())
            .flatten()
            .map(|f| {
                if !f.is_file() {
                    return None;
                }

                ReleaseData::from_json_reader(f)
            })
            .flatten();

        let mut contents: HashMap<String, FileDecisionGraph> = HashMap::new();
        let mut test_files: Vec<(String, FileTestContent)> = Vec::new();

        for i in 0..archive.len() {
            let Ok(file_reader) = archive.by_index_try_decrypt(i) else {
                return Err(anyhow!("failed to load file on index {i}"));
            };

            if !file_reader.is_file() {
                continue;
            }

            let Some(enclosed_name) = file_reader.enclosed_name() else {
                continue;
            };

            if enclosed_name.components().nth(0)
                == Some(Component::Normal(OsStr::from_bytes(
                    config_prefix.as_bytes(),
                )))
            {
                continue;
            }

            let original_name = file_reader.name().to_string();
            let name = original_name.to_lowercase();
            let Ok(content) = serde_json::from_reader::<_, FileContent>(file_reader) else {
                return Err(anyhow!("failed to parse decision content for file {name}"));
            };

            match content {
                FileContent::Graph(mut graph) => {
                    graph.meta.display_path = Some(Arc::from(original_name.as_str()));
                    contents.insert(name, graph);
                }
                FileContent::Test(test) => test_files.push((original_name, test)),
                FileContent::Unknown => {}
            }
        }

        let examples = collect_examples(test_files);
        Ok(Self::new(contents, examples, release_data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::extended_decision::FileContent;
    use serde_json::json;

    fn graph(value: serde_json::Value, display_path: &str) -> (String, FileDecisionGraph) {
        let FileContent::Graph(mut graph) = serde_json::from_value::<FileContent>(value).unwrap()
        else {
            panic!("expected graph");
        };
        graph.meta.display_path = Some(Arc::from(display_path));
        (display_path.to_lowercase(), graph)
    }

    fn test_file(value: serde_json::Value) -> FileTestContent {
        let FileContent::Test(test) = serde_json::from_value::<FileContent>(value).unwrap() else {
            panic!("expected test file");
        };
        test
    }

    #[test]
    fn collect_examples_caps_and_skips_disabled() {
        let files = vec![
            (
                "Pricing Rule.test".to_string(),
                test_file(json!({
                    "contentType": "test",
                    "filePath": "Pricing Rule",
                    "testCases": [
                        { "name": "one", "input": { "n": 1 } },
                        { "name": "off", "input": { "n": 0 }, "disabled": true },
                        { "input": { "n": 3 } },
                        { "name": "four", "input": { "n": 4 } },
                        { "name": "five", "input": { "n": 5 } }
                    ]
                })),
            ),
            (
                "disabled.test".to_string(),
                test_file(json!({
                    "contentType": "test",
                    "filePath": "Pricing Rule",
                    "disabled": true,
                    "testCases": [{ "name": "ignored", "input": {} }]
                })),
            ),
            (
                "orphan.test".to_string(),
                test_file(json!({ "contentType": "test", "testCases": [{ "input": {} }] })),
            ),
        ];

        let examples = collect_examples(files);
        assert_eq!(examples.len(), 1);

        let list = examples.get("pricing rule").unwrap();
        assert_eq!(
            list.len(),
            3,
            "capped at MAX_EXAMPLES_PER_RULE, disabled skipped"
        );
        assert_eq!(list[0].name.as_ref(), "one");
        assert_eq!(
            list[1].name.as_ref(),
            "Pricing Rule.test",
            "name falls back to test file path"
        );
        assert_eq!(list[1].input, json!({ "n": 3 }));
        assert_eq!(list[2].name.as_ref(), "four");
        assert_eq!(list[0].source_test_file.as_ref(), "Pricing Rule.test");
    }

    #[test]
    fn spec_entries_extracts_schemas() {
        let (key_b, graph_b) = graph(
            json!({
                "contentType": "graph",
                "title": "Pricing",
                "description": "Cart pricing rules",
                "meta": { "versionId": "3a5b" },
                "nodes": [
                    { "id": "in", "name": "request", "type": "inputNode",
                      "content": { "schema": "{\"type\":\"object\",\"properties\":{\"cartTotal\":{\"type\":\"number\"}}}" } },
                    { "id": "out", "name": "response", "type": "outputNode",
                      "content": { "schema": "{\"type\":\"object\"}" } }
                ],
                "edges": [{ "id": "e1", "sourceId": "in", "targetId": "out" }]
            }),
            "Pricing Rule",
        );
        let (key_a, graph_a) = graph(
            json!({ "contentType": "graph", "nodes": [], "edges": [] }),
            "alpha-rule",
        );

        let mut examples = HashMap::new();
        examples.insert(
            "pricing rule".to_string(),
            vec![SpecExample {
                name: Arc::from("one"),
                input: json!({ "n": 1 }),
                source_test_file: Arc::from("Pricing Rule.test"),
            }],
        );

        let loader = ImmutableLoader::new(
            HashMap::from([(key_a, graph_a), (key_b, graph_b)]),
            examples,
            None,
        );

        let entries = loader.spec_entries();
        assert_eq!(entries.len(), 2);

        let pricing = entries
            .iter()
            .find(|e| e.path.as_ref() == "Pricing Rule")
            .expect("expected an entry for Pricing Rule");
        assert_eq!(pricing.title.as_deref(), Some("Pricing"));
        assert_eq!(pricing.description.as_deref(), Some("Cart pricing rules"));
        assert_eq!(pricing.content_hash.as_deref(), Some("3a5b"));
        assert_eq!(
            pricing.input_schema.as_deref(),
            Some(&json!({ "type": "object", "properties": { "cartTotal": { "type": "number" } } }))
        );
        assert_eq!(
            pricing.output_schema.as_deref(),
            Some(&json!({ "type": "object" }))
        );
        assert_eq!(pricing.examples.len(), 1);

        let alpha = entries
            .iter()
            .find(|e| e.path.as_ref() == "alpha-rule")
            .expect("expected an entry for alpha-rule");
        assert_eq!(alpha.title, None);
        assert_eq!(alpha.input_schema, None);
        assert_eq!(alpha.examples.len(), 0);
    }
}
