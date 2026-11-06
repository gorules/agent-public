use crate::data::evaluation_meta::EvaluationMeta;
use crate::engine_ext::EngineExtension;
use crate::rules_spec::{RulesDocumentSource, build_rules_openapi};
use crate::{Agent, Project};
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::task::LocalPoolHandle;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use zen_engine::loader::LoaderError;
use zen_engine::{EvaluationError, EvaluationOptions};

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RulesEvaluateRequest {
    context: Value,
    trace: Option<bool>,
}

/// Uniform error body of the /rules surface: `{ code, details?, key? }`,
/// mirroring the BRMS error keys (rules.handler.ts `throwEngineError`).
pub struct RulesApiError {
    status: StatusCode,
    body: Value,
}

impl RulesApiError {
    fn new(status: StatusCode, code: &str) -> Self {
        Self {
            status,
            body: json!({ "code": code }),
        }
    }

    fn project_not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "project.notFound")
    }

    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized")
    }

    fn from_engine_error(error: &EvaluationError) -> Self {
        match error {
            EvaluationError::LoaderError(LoaderError::NotFound(key)) => Self {
                status: StatusCode::BAD_REQUEST,
                body: json!({ "code": "documents.invalid", "details": "File not found", "key": key }),
            },
            EvaluationError::NodeError { source, .. }
                if source
                    .to_string()
                    .contains("Custom node handler not provided") =>
            {
                Self {
                    status: StatusCode::BAD_REQUEST,
                    body: json!({
                        "code": "evaluate.integrationsNotSupported",
                        "details": "This endpoint does not evaluate graphs containing integration (custom) nodes."
                    }),
                }
            }
            other => Self {
                status: StatusCode::BAD_REQUEST,
                body: json!({
                    "code": "evaluate.failed",
                    "details": serde_json::to_value(other).unwrap_or_default()
                }),
            },
        }
    }
}

impl IntoResponse for RulesApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// Project lookup + flat token check shared by both /rules handlers.
fn authorized_project(
    agent: &Agent,
    project: &str,
    headers: &HeaderMap,
) -> Result<Arc<Project>, RulesApiError> {
    let Some(project_data) = agent.project(project) else {
        return Err(RulesApiError::project_not_found());
    };

    let access_token = headers
        .get("X-Access-Token")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();

    if !project_data.engine.can_access(access_token) {
        return Err(RulesApiError::unauthorized());
    }

    Ok(project_data)
}

#[utoipa::path(
    post,
    path = "/api/rules/{project}/evaluate/{*key}",
    params(
        ("project" = String, Path, description = "Project slug or id"),
        ("key" = String, Path, description = "Key (path) of decision model")
    ),
    request_body = RulesEvaluateRequest,
    responses(
        (status = OK, description = "Raw engine evaluation response: { performance, result, trace?, meta? }"),
        (status = BAD_REQUEST, description = "Evaluation error: { code, details?, key? }")
    )
)]
pub async fn rules_evaluate(
    headers: HeaderMap,
    Extension(local_pool): Extension<LocalPoolHandle>,
    Extension(agent): Extension<Agent>,
    Path((project, key)): Path<(Arc<str>, Arc<str>)>,
    Json(payload): Json<RulesEvaluateRequest>,
) -> Result<Response, RulesApiError> {
    let span = Span::current();
    span.set_attribute("params.project", project.clone());
    span.set_attribute("params.key", key.clone());

    let project_data = authorized_project(&agent, &project, &headers)?;

    let release_data = project_data.engine.release_data();
    if let Some(release_data) = &release_data {
        if let Some(id) = release_data.release_id() {
            span.set_attribute("release.id", id.clone());
        }
        if let Some(version) = release_data.release_version() {
            span.set_attribute("release.version", version.clone());
        }
        if let Some(id) = release_data.project_id() {
            span.set_attribute("project.id", id.clone());
        }
        if let Some(key) = release_data.project_key() {
            span.set_attribute("project.key", key.clone());
        }
    }

    let cloned_project_data = project_data.clone();
    let cloned_key = key.clone();
    // EvaluationError is not Send — classify and serialize it inside the
    // pinned task, so only Send data crosses back.
    let result = local_pool
        .spawn_pinned(move || async move {
            cloned_project_data
                .engine
                .evaluate_with_opts(
                    &cloned_key,
                    payload.context.into(),
                    EvaluationOptions {
                        trace: payload.trace.unwrap_or(false),
                        max_depth: 10,
                    },
                )
                .await
                .map_err(|error| RulesApiError::from_engine_error(error.as_ref()))
                .and_then(|response| {
                    serde_json::to_value(response)
                        .map_err(|_| RulesApiError::new(StatusCode::BAD_REQUEST, "evaluate.failed"))
                })
        })
        .await
        .expect("Thread failed to join");

    let mut body = match result {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(error = ?error.body, "Failed to evaluate decision model");
            return Err(error);
        }
    };

    // BRMS parity: `Object.assign(result.data, { meta })` — meta rides on
    // 200s only.
    if let Some(meta) = EvaluationMeta::from_release_data(release_data.as_ref(), &key)
        && let Ok(meta_value) = serde_json::to_value(&meta)
        && let Some(map) = body.as_object_mut()
    {
        map.insert("meta".to_string(), meta_value);
    }

    let mut response = Json(body).into_response();
    if let Some(release_id) = release_data.as_ref().and_then(|rd| rd.release_id())
        && let Ok(header_value) = HeaderValue::from_str(release_id)
    {
        response.headers_mut().insert("X-Release-Id", header_value);
    }

    Ok(response)
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct RulesListQuery {
    /// When true, adds a Content-Disposition attachment header.
    download: Option<bool>,
}

#[utoipa::path(
    get,
    path = "/api/rules/{project}",
    params(
        ("project" = String, Path, description = "Project slug or id"),
        RulesListQuery
    ),
    responses(
        (status = OK, description = "OpenAPI 3 document describing the deployed release's evaluable rules")
    )
)]
pub async fn rules_openapi(
    headers: HeaderMap,
    Extension(agent): Extension<Agent>,
    Path(project): Path<Arc<str>>,
    Query(query): Query<RulesListQuery>,
) -> Result<Response, RulesApiError> {
    let project_data = authorized_project(&agent, &project, &headers)?;

    let release_data = project_data.engine.release_data();
    let project_ref = release_data
        .as_ref()
        .and_then(|rd| rd.project_key().cloned())
        .unwrap_or_else(|| project.clone());

    let document = match project_data.rules_spec.get() {
        Some(document) => document.clone(),
        None => {
            let cloned_project_data = project_data.clone();
            let cloned_project_ref = project_ref.clone();
            // Derivation runs the TypeScript compiler and can take seconds, so
            // it goes to the blocking pool rather than the pinned pool that
            // serves evaluations — a cold spec request must not stall them.
            // The zen Workspace it builds is !Send, but it is created and
            // dropped inside this closure, so only the document crosses back.
            let built = tokio::task::spawn_blocking(move || {
                let mut entries = cloned_project_data.engine.spec_entries();
                crate::spec_derive::enrich(&mut entries, cloned_project_data.engine.documents());

                Arc::new(build_rules_openapi(RulesDocumentSource {
                    project_ref: cloned_project_ref.as_ref(),
                    release: cloned_project_data.engine.release_data().as_ref(),
                    entries: &entries,
                }))
            })
            .await
            .expect("Thread failed to join");

            project_data.rules_spec.get_or_init(|| built).clone()
        }
    };

    let mut response = Json(document.as_ref().clone()).into_response();
    if query.download.unwrap_or(false)
        && let Ok(value) = HeaderValue::from_str(&format!(
            "attachment; filename=\"{}\"",
            sanitize_filename(&format!("{project_ref}-openapi.json"))
        ))
    {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }

    Ok(response)
}

/// Same character policy as BRMS: runs of anything outside
/// `[a-zA-Z0-9._-]` collapse to a single `-`.
fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filename_collapses_disallowed_runs() {
        // '(' is disallowed right after a replacement dash for the preceding
        // space, so it's swallowed (consecutive-dash suppression). ')' then
        // becomes its own replacement dash. The literal '-' that follows is
        // an *allowed* character, not a replacement dash, so it is pushed
        // even though the previous char was a (replacement) dash — hence
        // the double dash before "openapi".
        assert_eq!(
            sanitize_filename("my project (v2)-openapi.json"),
            "my-project-v2--openapi.json"
        );

        // A run of disallowed characters collapses to a single dash.
        assert_eq!(sanitize_filename("a !?b"), "a-b");
    }
}
