mod support;

use agent::app;
use agent::config::{EnvironmentConfig, ProviderConfig, ZipProviderConfig};
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

#[tokio::test]
async fn health_test() {
    let config = EnvironmentConfig {
        provider: ProviderConfig::Zip(ZipProviderConfig {
            root_dir: "tests/data".to_string(),
        }),
        ..Default::default()
    };

    let agent = app::create_agent(config.clone(), Default::default()).await;
    let app = app::create_app(agent, config).await;

    let request = Request::get("/api/health").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200, "Response should be 200.");
}
