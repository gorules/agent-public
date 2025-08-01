use crate::config::{EnvironmentConfig, GlobalAgentConfig};
use crate::provider::Agent;
use crate::routes;
use axum::extract::DefaultBodyLimit;
use axum::{Extension, Router};
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use std::sync::Arc;
use std::thread::available_parallelism;
use tokio_util::task::LocalPoolHandle;
use tower_http::cors::CorsLayer;
use utoipa::openapi::{ContactBuilder, InfoBuilder};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_swagger_ui::SwaggerUi;

pub async fn create_agent(
    config: EnvironmentConfig,
    global_config: Arc<GlobalAgentConfig>,
) -> Agent {
    match Agent::new(config, global_config).await {
        Ok(agent) => agent,
        Err(error) => {
            tracing::error!("Failed to create agent: {error:?}");
            panic!("Failed to create agent");
        }
    }
}

pub async fn create_app(agent: Agent, config: EnvironmentConfig) -> Router<()> {
    let local_pool = LocalPoolHandle::new(available_parallelism().map(Into::into).unwrap_or(1));

    let (router, openapi) = OpenApiRouter::with_openapi(openapi())
        .routes(routes!(routes::engine::evaluate))
        .routes(routes!(routes::project_info::project_info))
        .routes(routes!(routes::infra::version))
        .routes(routes!(routes::infra::health))
        .split_for_parts();

    let mut app = router
        .merge(SwaggerUi::new("/api/docs").url("/api.json", openapi))
        .layer(Extension(agent))
        .layer(Extension(local_pool))
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024));
    if config.otel_enabled {
        app = app
            .layer(OtelInResponseLayer::default())
            .layer(OtelAxumLayer::default())
    }

    if config.cors_permissive {
        app = app.layer(CorsLayer::permissive());
    }

    app
}

fn openapi() -> utoipa::openapi::OpenApi {
    let service_version =
        std::env::var("SERVICE_VERSION").unwrap_or_else(|_| "unknown".to_string());

    let openapi_info = InfoBuilder::new()
        .title(env!("CARGO_PKG_NAME"))
        .version(service_version)
        .description(Some(env!("CARGO_PKG_DESCRIPTION").to_string()))
        .contact(Some(
            ContactBuilder::new()
                .name(Some("GoRules"))
                .email(Some("hi@gorules.io"))
                .build(),
        ))
        .build();

    utoipa::openapi::OpenApi::new(openapi_info, utoipa::openapi::Paths::new())
}
