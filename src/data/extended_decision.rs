use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zen_engine::model::DecisionContent;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtendedDecisionContent {
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
