use crate::data::release_data::{MetaKind, ReleaseData};
use serde::Serialize;
use std::sync::Arc;

/// The `meta` object appended to successful /rules evaluate responses — the
/// agent-side subset of BRMS `EvaluateMeta` (rules.handler.ts). An agent
/// serves a release deployed to an environment, so the only kinds it can
/// truthfully claim are `environment` (configs carrying the environment
/// block) and `release` (older configs without it).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvaluationMeta {
    #[serde(rename = "type")]
    pub kind: MetaKind,
    pub path: Arc<str>,
    pub project: MetaProject,
    pub release: MetaRelease,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<MetaEnvironment>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetaProject {
    pub id: Arc<str>,
    pub key: Option<Arc<str>>,
    pub name: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaRelease {
    pub id: Arc<str>,
    pub name: Option<Arc<str>>,
    pub version: Option<Arc<str>>,
    pub status: Option<Arc<str>>,
    pub commit_id: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetaEnvironment {
    pub id: Arc<str>,
    pub key: Option<Arc<str>>,
    pub name: Option<Arc<str>>,
}

impl EvaluationMeta {
    /// Unknown metadata serializes as explicit `null`s (BRMS null-coalesces
    /// every field), except `environment`, which is omitted entirely for
    /// `release` kind — the published schema requires it only for
    /// `environment` kind.
    pub fn from_release_data(release_data: Option<&ReleaseData>, path: &str) -> Option<Self> {
        let rd = release_data?;
        let kind = rd.meta_kind()?;
        let project = rd.project.as_ref()?;
        let release = rd.release.as_ref()?;

        let environment = match kind {
            MetaKind::Environment => {
                let environment = rd.environment.as_ref()?;
                Some(MetaEnvironment {
                    id: environment.id.clone()?,
                    key: environment.key.clone(),
                    name: environment.name.clone(),
                })
            }
            MetaKind::Release => None,
        };

        Some(Self {
            kind,
            path: Arc::from(path),
            project: MetaProject {
                id: project.id.clone()?,
                key: project.key.clone(),
                name: project.name.clone(),
            },
            release: MetaRelease {
                id: release.id.clone()?,
                name: release.name.clone(),
                version: release.version.clone(),
                status: release.status.clone(),
                commit_id: release.commit_id.clone(),
            },
            environment,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::release_data::ReleaseData;
    use serde_json::json;

    fn rd(value: serde_json::Value) -> ReleaseData {
        serde_json::from_value(value).unwrap()
    }

    fn new_config() -> ReleaseData {
        rd(json!({
            "version": "1",
            "project": { "id": "p-1", "key": "shipping", "name": "Shipping Rules" },
            "accessTokens": ["tok"],
            "release": { "id": "r-1", "version": "1.4.0", "name": "Q3",
                         "status": "published", "commitId": "c-1" },
            "environment": { "id": "e-1", "key": "production", "name": "Production" }
        }))
    }

    fn old_config() -> ReleaseData {
        rd(json!({
            "version": "1",
            "project": { "id": "p-1", "key": "shipping" },
            "accessTokens": ["tok"],
            "release": { "id": "r-1", "version": "1.4.0" }
        }))
    }

    #[test]
    fn new_config_builds_environment_meta() {
        let meta = EvaluationMeta::from_release_data(Some(&new_config()), "pricing/main").unwrap();
        let value = serde_json::to_value(&meta).unwrap();

        assert_eq!(
            value,
            json!({
                "type": "environment",
                "path": "pricing/main",
                "project": { "id": "p-1", "key": "shipping", "name": "Shipping Rules" },
                "release": { "id": "r-1", "name": "Q3", "version": "1.4.0",
                             "status": "published", "commitId": "c-1" },
                "environment": { "id": "e-1", "key": "production", "name": "Production" }
            })
        );
    }

    #[test]
    fn old_config_builds_release_meta_with_nulls() {
        let meta = EvaluationMeta::from_release_data(Some(&old_config()), "plain-rule").unwrap();
        let value = serde_json::to_value(&meta).unwrap();

        assert_eq!(
            value,
            json!({
                "type": "release",
                "path": "plain-rule",
                "project": { "id": "p-1", "key": "shipping", "name": null },
                "release": { "id": "r-1", "name": null, "version": "1.4.0",
                             "status": null, "commitId": null }
            })
        );
        assert!(
            value.get("environment").is_none(),
            "omitted for release kind"
        );
    }

    #[test]
    fn no_config_or_missing_ids_mean_no_meta() {
        assert!(EvaluationMeta::from_release_data(None, "x").is_none());

        let partial = rd(json!({ "accessTokens": ["tok"] }));
        assert!(EvaluationMeta::from_release_data(Some(&partial), "x").is_none());

        let no_release_id = rd(json!({
            "project": { "id": "p-1" },
            "release": { "version": "1.0.0" }
        }));
        assert!(EvaluationMeta::from_release_data(Some(&no_release_id), "x").is_none());
    }

    #[test]
    fn environment_without_id_degrades_to_release_kind() {
        let cfg = rd(json!({
            "project": { "id": "p-1" },
            "release": { "id": "r-1" },
            "environment": { "key": "production" }
        }));

        let meta = EvaluationMeta::from_release_data(Some(&cfg), "x").unwrap();
        let value = serde_json::to_value(&meta).unwrap();
        assert_eq!(value["type"], json!("release"));
        assert!(value.get("environment").is_none());
    }
}
