use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// `.config/project.json` as shipped by BRMS `deployReleaseToEnv`.
///
/// Parsing is deliberately lenient: BRMS writes explicit `null`s
/// (`project.key` is nullable), pre-GRL-614 releases lack the newer fields
/// entirely, and hand-written configs may carry only a subset. A missing
/// field must degrade that field, never the whole config — a failed
/// whole-config parse disables token auth (`can_access` treats "no config"
/// as open access).
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseData {
    pub version: Option<Arc<str>>,
    pub project: Option<ReleaseDataProject>,
    #[serde(default)]
    pub access_tokens: Vec<Arc<str>>,
    pub release: Option<ReleaseDataRelease>,
    pub environment: Option<ReleaseDataEnvironment>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseDataProject {
    pub id: Option<Arc<str>>,
    pub key: Option<Arc<str>>,
    pub name: Option<Arc<str>>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseDataRelease {
    pub id: Option<Arc<str>>,
    pub version: Option<Arc<str>>,
    pub name: Option<Arc<str>>,
    /// Opaque pass-through: BRMS's ReleaseStatus has more values than the
    /// advertised `draft | published`; the agent does not validate it.
    pub status: Option<Arc<str>>,
    pub commit_id: Option<Arc<str>>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseDataEnvironment {
    pub id: Option<Arc<str>>,
    pub key: Option<Arc<str>>,
    pub name: Option<Arc<str>>,
}

/// Which `meta.type` the /rules surface can truthfully claim for a config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MetaKind {
    Environment,
    Release,
}

impl ReleaseData {
    pub fn project_id(&self) -> Option<&Arc<str>> {
        self.project.as_ref()?.id.as_ref()
    }

    pub fn project_key(&self) -> Option<&Arc<str>> {
        self.project.as_ref()?.key.as_ref()
    }

    pub fn release_id(&self) -> Option<&Arc<str>> {
        self.release.as_ref()?.id.as_ref()
    }

    pub fn release_version(&self) -> Option<&Arc<str>> {
        self.release.as_ref()?.version.as_ref()
    }

    pub fn environment_id(&self) -> Option<&Arc<str>> {
        self.environment.as_ref()?.id.as_ref()
    }

    /// The meta emission decision shared by the evaluate response and the
    /// OpenAPI document: `None` without both identities (no meta at all),
    /// `Environment` when the deployed environment is known, `Release`
    /// otherwise.
    pub fn meta_kind(&self) -> Option<MetaKind> {
        self.project_id()?;
        self.release_id()?;
        Some(match self.environment_id() {
            Some(_) => MetaKind::Environment,
            None => MetaKind::Release,
        })
    }

    /// Lenient-parse `.config/project.json`. `None` (with a warning) only
    /// for unusable input — non-JSON or wrong-typed fields. A `None` here
    /// disables token auth, so it must never pass silently.
    pub fn from_json_reader(reader: impl std::io::Read) -> Option<Self> {
        match serde_json::from_reader::<_, ReleaseData>(reader) {
            Ok(data) => Some(data),
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "Failed to parse .config/project.json; release metadata and access tokens will be ignored"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: serde_json::Value) -> ReleaseData {
        serde_json::from_value(value).expect("config must parse")
    }

    #[test]
    fn parses_new_brms_config() {
        let rd = parse(json!({
            "version": "1",
            "project": { "id": "p-1", "key": "shipping", "name": "Shipping Rules" },
            "accessTokens": ["tok"],
            "release": { "id": "r-1", "version": "1.4.0", "name": "Q3",
                         "status": "published", "commitId": "c-1" },
            "environment": { "id": "e-1", "key": "production", "name": "Production" },
            "deployment": { "deployedBy": "x@y.z", "createdAt": "2026-08-06T00:00:00Z" }
        }));

        assert_eq!(rd.project_key().map(AsRef::as_ref), Some("shipping"));
        let release = rd.release.as_ref().unwrap();
        assert_eq!(release.status.as_deref(), Some("published"));
        assert_eq!(release.commit_id.as_deref(), Some("c-1"));
        assert_eq!(rd.environment_id().map(AsRef::as_ref), Some("e-1"));
        assert_eq!(rd.meta_kind(), Some(MetaKind::Environment));
    }

    #[test]
    fn parses_old_brms_config() {
        let rd = parse(json!({
            "version": "1",
            "project": { "id": "p-1", "key": "shipping" },
            "accessTokens": ["tok"],
            "release": { "id": "r-1", "version": "1.4.0" }
        }));

        assert_eq!(rd.meta_kind(), Some(MetaKind::Release));
        assert_eq!(rd.release_version().map(AsRef::as_ref), Some("1.4.0"));
        assert!(rd.environment.is_none());
        assert!(rd.project.as_ref().unwrap().name.is_none());
    }

    #[test]
    fn tolerates_explicit_nulls() {
        // BRMS writes explicit nulls (key: project?.key ?? null etc.).
        let rd = parse(json!({
            "project": { "id": "p-1", "key": null, "name": null },
            "release": { "id": "r-1" },
            "environment": null
        }));

        assert_eq!(rd.project_id().map(AsRef::as_ref), Some("p-1"));
        assert!(rd.project_key().is_none());
        assert_eq!(rd.meta_kind(), Some(MetaKind::Release));
    }

    #[test]
    fn partial_config_parses_without_meta_identity() {
        let rd = parse(json!({ "accessTokens": ["tok"] }));

        assert_eq!(rd.access_tokens.len(), 1);
        assert!(
            rd.meta_kind().is_none(),
            "no project/release ids -> no meta"
        );
    }

    #[test]
    fn environment_without_id_degrades_to_release_kind() {
        let rd = parse(json!({
            "project": { "id": "p-1" },
            "release": { "id": "r-1" },
            "environment": { "key": "production" }
        }));

        assert_eq!(rd.meta_kind(), Some(MetaKind::Release));
    }

    #[test]
    fn missing_release_id_means_no_meta() {
        let rd = parse(json!({
            "project": { "id": "p-1", "key": "k" },
            "release": { "version": "1.0.0" }
        }));

        assert!(rd.meta_kind().is_none());
    }

    #[test]
    fn unusable_input_parses_to_none() {
        assert!(ReleaseData::from_json_reader("not json".as_bytes()).is_none());
        assert!(
            ReleaseData::from_json_reader(r#"{"accessTokens": "not-an-array"}"#.as_bytes())
                .is_none(),
            "wrong-typed field still fails the whole parse"
        );
        assert!(ReleaseData::from_json_reader("{}".as_bytes()).is_some());
    }
}
