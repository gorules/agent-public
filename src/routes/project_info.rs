use crate::Agent;
use crate::engine_ext::EngineExtension;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::Serialize;
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/api/projects/{project}",
    params(
        ("project" = String, Path, description = "Project slug or id")
    ),
    responses(
        (status = OK, body = ProjectInfo)
    )
)]
pub async fn project_info(
    Extension(agent): Extension<Agent>,
    Path(project): Path<String>,
) -> Result<Json<ProjectInfo>, (StatusCode, String)> {
    let Some(p) = agent.project(project.as_str()) else {
        return Err((StatusCode::NOT_FOUND, "Project not found".to_string()));
    };

    let Some(release_data) = p.engine.release_data() else {
        return Err((
            StatusCode::BAD_REQUEST,
            "Project data not available".to_string(),
        ));
    };

    Ok(Json(ProjectInfo {
        project_id: release_data.project.id.clone(),
        project_key: release_data.project.key.clone(),
        release_id: release_data.release.id.clone(),
        release_version: release_data.release.version.clone(),
    }))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectInfo {
    pub project_id: Arc<str>,
    pub project_key: Arc<str>,
    pub release_id: Arc<str>,
    pub release_version: Arc<str>,
}
