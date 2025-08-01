use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseData {
    pub version: Option<Arc<str>>,
    pub project: ReleaseDataProject,
    #[serde(default)]
    pub access_tokens: Vec<Arc<str>>,
    pub release: ReleaseDataRelease,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseDataProject {
    pub id: Arc<str>,
    pub key: Arc<str>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseDataRelease {
    pub id: Arc<str>,
    pub version: Arc<str>,
}
