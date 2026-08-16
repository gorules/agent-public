use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use zen_engine::model::DecisionContent;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDecisionGraph {
    #[serde(default)]
    pub meta: DecisionContentMeta,

    #[serde(default)]
    pub title: Option<Arc<str>>,

    #[serde(default)]
    pub description: Option<Arc<str>>,

    #[serde(flatten)]
    pub content: Arc<DecisionContent>,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionContentMeta {
    pub version_id: Option<Arc<str>>,

    /// Original-cased path from the source (zip entry / relative file path);
    /// set by the loader after parsing, never read from the file itself.
    #[serde(skip)]
    pub display_path: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTestContent {
    pub file_path: Option<Arc<str>>,

    #[serde(default)]
    pub disabled: bool,

    #[serde(default)]
    pub test_cases: Vec<FileTestCase>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTestCase {
    #[serde(default)]
    pub name: Option<Arc<str>>,

    #[serde(default)]
    pub input: Value,

    #[serde(default)]
    pub disabled: bool,
}

/// New file types need to be also added below in TaggedFileContent
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "contentType")]
pub enum FileContent {
    Graph(FileDecisionGraph),
    Test(FileTestContent),
    Unknown,
}

impl<'de> Deserialize<'de> for FileContent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// As serde doesn't support tagged union with default - we need to duplicate the content here
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", tag = "contentType")]
        enum TaggedFileContent {
            Graph(FileDecisionGraph),
            Test(FileTestContent),
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Tagged(TaggedFileContent),
            Untagged(FileDecisionGraph),
        }

        match Either::deserialize(deserializer) {
            Ok(file) => match file {
                Either::Tagged(TaggedFileContent::Graph(g)) => Ok(FileContent::Graph(g)),
                Either::Tagged(TaggedFileContent::Test(t)) => Ok(FileContent::Test(t)),
                Either::Untagged(g) => Ok(FileContent::Graph(g)),
            },
            Err(_) => Ok(FileContent::Unknown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn graph_with_metadata_is_parsed() {
        let value = json!({
            "contentType": "graph",
            "title": "Pricing",
            "description": "Cart pricing rules",
            "meta": { "versionId": "3a5b" },
            "nodes": [], "edges": []
        });

        let FileContent::Graph(graph) = serde_json::from_value::<FileContent>(value).unwrap()
        else {
            panic!("expected graph");
        };
        assert_eq!(graph.title.as_deref(), Some("Pricing"));
        assert_eq!(graph.description.as_deref(), Some("Cart pricing rules"));
        assert_eq!(graph.meta.version_id.as_deref(), Some("3a5b"));
        assert_eq!(graph.meta.display_path, None);
    }

    #[test]
    fn legacy_content_type_is_still_graph() {
        let value = json!({
            "contentType": "application/vnd.gorules.decision",
            "nodes": [], "edges": []
        });
        assert!(matches!(
            serde_json::from_value::<FileContent>(value).unwrap(),
            FileContent::Graph(_)
        ));
    }

    #[test]
    fn test_file_is_parsed() {
        let value = json!({
            "contentType": "test",
            "filePath": "Pricing Rule",
            "testCases": [
                { "id": "t1", "name": "small cart", "input": { "cartTotal": 10 } },
                { "name": "off", "input": {}, "disabled": true }
            ]
        });

        let FileContent::Test(test) = serde_json::from_value::<FileContent>(value).unwrap() else {
            panic!("expected test file");
        };
        assert_eq!(test.file_path.as_deref(), Some("Pricing Rule"));
        assert!(!test.disabled);
        assert_eq!(test.test_cases.len(), 2);
        assert_eq!(test.test_cases[0].name.as_deref(), Some("small cart"));
        assert_eq!(test.test_cases[0].input, json!({ "cartTotal": 10 }));
        assert!(!test.test_cases[0].disabled);
        assert!(test.test_cases[1].disabled);
    }

    #[test]
    fn unparseable_content_is_unknown() {
        let value = json!({ "foo": "bar" });
        assert!(matches!(
            serde_json::from_value::<FileContent>(value).unwrap(),
            FileContent::Unknown
        ));
    }
}
