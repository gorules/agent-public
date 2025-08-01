use axum::http::StatusCode;

#[utoipa::path(
    get,
    path = "/api/health",
    responses(
        (status = OK, body = String)
    )
)]
pub async fn health() -> (StatusCode, &'static str) {
    (StatusCode::OK, "healthy")
}

#[utoipa::path(
    get,
    path = "/api/version",
    responses(
        (status = OK, body = String)
    )
)]
pub async fn version() -> (StatusCode, String) {
    let service_version =
        std::env::var("SERVICE_VERSION").unwrap_or_else(|_| "unknown".to_string());

    (StatusCode::OK, service_version)
}
