use crate::support::minio::MinioContainer;
use crate::support::path::decision_paths;
use agent::app;
use agent::config::{EnvironmentConfig, ProviderConfig, S3ProviderConfig};
use axum::body::{Body, to_bytes};
use axum::http::{Request, Response};
use serde::Deserialize;
use serde_json::json;
use std::env;
use tower::ServiceExt;

mod support;

#[tokio::test]
async fn s3_engine() {
    let minio = MinioContainer::start()
        .await
        .expect("Minio container is available");
    let host_port = minio
        .container
        .get_host_port_ipv4(9000)
        .await
        .expect("Minio port 9000 is available");
    let minio_image = minio.container.image();

    unsafe { env::set_var("AWS_ACCESS_KEY_ID", minio_image.username.clone()) };
    unsafe { env::set_var("AWS_SECRET_ACCESS_KEY", minio_image.password.clone()) };

    let config = EnvironmentConfig {
        provider: ProviderConfig::S3(S3ProviderConfig {
            bucket: minio_image.bucket_name.to_string(),
            endpoint: Some(minio_image.endpoint(host_port)),
            prefix: None,
            force_path_style: true,
        }),
        ..Default::default()
    };

    run_engine_test(config, "sample-project").await;
}

#[tokio::test]
async fn s3_engine_prefix() {
    let minio = MinioContainer::start()
        .await
        .expect("Minio container is available");
    let host_port = minio
        .container
        .get_host_port_ipv4(9000)
        .await
        .expect("Minio port 9000 is available");
    let minio_image = minio.container.image();

    unsafe { env::set_var("AWS_ACCESS_KEY_ID", minio_image.username.clone()) };
    unsafe { env::set_var("AWS_SECRET_ACCESS_KEY", minio_image.password.clone()) };

    let config = EnvironmentConfig {
        provider: ProviderConfig::S3(S3ProviderConfig {
            bucket: minio_image.bucket_name.to_string(),
            endpoint: Some(minio_image.endpoint(host_port)),
            prefix: Some("nested".to_string()),
            force_path_style: true,
        }),
        ..Default::default()
    };

    run_engine_test(config, "nested-project").await;
}

async fn run_engine_test(config: EnvironmentConfig, project_name: &str) {
    let agent = app::create_agent(config.clone(), Default::default()).await;
    let router = app::create_app(agent, config).await;

    let req_json = json!({ "context": { "hello": "world" }});

    for key in decision_paths() {
        let request_uri = format!("/api/projects/{project_name}/evaluate/{key}");
        let request_uri = request_uri.replace(" ", "%20");

        let mut request = Request::post(request_uri.as_str())
            .body(Body::from(req_json.to_string()))
            .unwrap();
        let h = request.headers_mut();
        h.insert("Content-Type", "application/json".parse().unwrap());

        let r = router.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response_body_result(r).await,
            Some("world".to_string()),
            "Correct response body on {key} key"
        );
    }
}

#[derive(Debug, Deserialize)]
struct ResponseBody {
    pub result: ResponseBodyResult,
}

#[derive(Debug, Deserialize)]
struct ResponseBodyResult {
    pub hello: String,
}

async fn response_body_result(r: Response<Body>) -> Option<String> {
    let byte_data = to_bytes(r.into_body(), usize::MAX).await.ok()?;
    let r = serde_json::from_slice::<ResponseBody>(&byte_data).ok()?;

    Some(r.result.hello)
}
