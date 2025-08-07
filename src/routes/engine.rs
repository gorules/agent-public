use crate::Agent;
use crate::engine_ext::EngineExtension;
use anyhow::{Context, anyhow};
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::task::LocalPoolHandle;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use zen_engine::EvaluationOptions;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct EvaluateRequest {
    context: Value,
    trace: Option<bool>,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateResponse {
    details: EvaluateDetailsResponse,

    #[serde(flatten)]
    graph_response: Value,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateDetailsResponse {
    release_id: Option<Arc<str>>,
    version_id: Option<Arc<str>>,
}

#[utoipa::path(
    post,
    path = "/api/projects/{project}/evaluate/{*key}",
    params(
        ("project" = String, Path, description = "Project slug or id"),
        ("key" = String, Path, description = "Key (path) of decision model")
    ),
    request_body = EvaluateRequest,
    responses(
        (status = OK, body = EvaluateResponse)
    )
)]
pub async fn evaluate(
    headers: HeaderMap,
    Extension(local_pool): Extension<LocalPoolHandle>,
    Extension(agent): Extension<Agent>,
    Path((project, key)): Path<(Arc<str>, Arc<str>)>,
    Json(payload): Json<EvaluateRequest>,
) -> Result<Json<EvaluateResponse>, EvaluateError> {
    let span = Span::current();

    span.set_attribute("params.project", project.clone());
    span.set_attribute("params.key", key.clone());

    let Some(project_data) = agent.project(&project) else {
        let error = (StatusCode::NOT_FOUND, anyhow!("Project not found"));
        return Err(error.into());
    };

    if let Some(release_data) = project_data.engine.release_data() {
        span.set_attribute("release.id", release_data.release.id.clone());
        span.set_attribute("release.version", release_data.release.version.clone());
        span.set_attribute("project.id", release_data.project.id.clone());
        span.set_attribute("project.key", release_data.project.key.clone());
    };

    let access_token = headers
        .get("X-Access-Token")
        .map(|h| h.to_str().unwrap_or("").to_string())
        .unwrap_or_default();

    if !project_data.engine.can_access(access_token.as_str()) {
        let error = (
            StatusCode::UNAUTHORIZED,
            anyhow!("Invalid X-Access-Token Header"),
        );
        return Err(error.into());
    }

    let cloned_project_data = project_data.clone();
    let cloned_key = key.clone();
    let result = local_pool
        .spawn_pinned(move || async move {
            cloned_project_data
                .engine
                .evaluate_with_opts(
                    &cloned_key,
                    payload.context.into(),
                    EvaluationOptions {
                        trace: payload.trace,
                        max_depth: Some(10),
                    },
                )
                .await
                .map(|s| serde_json::to_value(s).context("Failed to serialize value"))
        })
        .await
        .expect("Thread failed to join");
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(error = debug(&error), "Failed to evaluate decision model");
            return Err(error.into());
        }
    };

    let result = match result {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(error = debug(&error), "Failed to serialize the response.");
            return Err(error.into());
        }
    };

    let release_data = project_data.engine.release_data();

    let release_id = release_data.map(|r| r.release.id.clone());
    let version_id = project_data.engine.get_version(&key);

    Ok(Json(EvaluateResponse {
        graph_response: result,
        details: EvaluateDetailsResponse {
            version_id,
            release_id,
        },
    }))
}

pub enum EvaluateError {
    EngineError(Box<zen_engine::EvaluationError>),
    Anyhow((StatusCode, anyhow::Error)),
}

impl IntoResponse for EvaluateError {
    fn into_response(self) -> Response {
        match self {
            EvaluateError::EngineError(error) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(&error).unwrap_or_default()),
            )
                .into_response(),
            EvaluateError::Anyhow((status, error)) => (
                status,
                Json(serde_json::json!({ "message": error.to_string() })),
            )
                .into_response(),
        }
    }
}

impl From<Box<zen_engine::EvaluationError>> for EvaluateError {
    fn from(value: Box<zen_engine::EvaluationError>) -> Self {
        Self::EngineError(value)
    }
}

impl From<anyhow::Error> for EvaluateError {
    fn from(value: anyhow::Error) -> Self {
        Self::Anyhow((StatusCode::BAD_REQUEST, value))
    }
}

impl From<(StatusCode, anyhow::Error)> for EvaluateError {
    fn from(value: (StatusCode, anyhow::Error)) -> Self {
        Self::Anyhow(value)
    }
}
