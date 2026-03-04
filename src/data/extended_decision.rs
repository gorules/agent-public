use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;
use zen_engine::model::DecisionContent;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDecisionGraph {
    #[serde(default)]
    pub meta: DecisionContentMeta,

    #[serde(flatten)]
    pub content: Arc<DecisionContent>,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionContentMeta {
    pub version_id: Option<Arc<str>>,
}

/// New file types need to be also added below in TaggedFileContent
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "contentType")]
pub enum FileContent {
    Graph(FileDecisionGraph),
    Unknown,
}

impl<'de> Deserialize<'de> for FileContent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// As serde doesn't support tagged union with default - we need to duplicate the content here
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", tag = "contentType")]
        enum TaggedFileContent {
            Graph(FileDecisionGraph),
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
                Either::Untagged(g) => Ok(FileContent::Graph(g)),
            },
            Err(_) => Ok(FileContent::Unknown),
        }
    }
}
