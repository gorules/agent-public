use crate::Agent;
use crate::engine_ext::EngineExtension;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::{Extension, Json};
use serde::Serialize;

#[utoipa::path(
    get,
    path = "/api/projects/{project}/entrypoints",
    params(
        ("project" = String, Path, description = "Project slug or id")
    ),
    responses(
        (status = OK, body = DecisionPointsResponse)
    )
)]
pub async fn decision_points(
    headers: HeaderMap,
    Extension(agent): Extension<Agent>,
    Path(project): Path<String>,
) -> Result<Json<DecisionPointsResponse>, (StatusCode, String)> {
    let Some(p) = agent.project(project.as_str()) else {
        return Err((StatusCode::NOT_FOUND, "Project not found".to_string()));
    };

    let access_token = headers
        .get("X-Access-Token")
        .map(|h| h.to_str().unwrap_or(""))
        .unwrap_or_default();

    if !p.engine.can_access(access_token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Invalid X-Access-Token Header".to_string(),
        ));
    }

    let entrypoints = p
        .engine
        .decision_keys()
        .into_iter()
        .map(|path| Entrypoint {
            path,
            r#type: "graph".to_string(),
        })
        .collect();

    Ok(Json(DecisionPointsResponse { entrypoints }))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DecisionPointsResponse {
    pub entrypoints: Vec<Entrypoint>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct Entrypoint {
    pub path: String,
    pub r#type: String,
}
