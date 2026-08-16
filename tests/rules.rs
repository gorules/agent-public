use agent::app;
use agent::config::{EnvironmentConfig, ProviderConfig, ZipProviderConfig};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn rules_router() -> Router {
    let config = EnvironmentConfig {
        provider: ProviderConfig::Zip(ZipProviderConfig {
            root_dir: "tests/data/rules".to_string(),
        }),
        ..Default::default()
    };

    let agent = app::create_agent(config.clone(), Default::default()).await;
    app::create_app(agent, config).await
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };

    (status, headers, body)
}

fn evaluate_request(uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::post(uri).header("Content-Type", "application/json");
    if let Some(token) = token {
        builder = builder.header("X-Access-Token", token);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn evaluate_returns_raw_engine_response_with_release_header() {
    let router = rules_router().await;
    let (status, headers, body) = send(
        &router,
        evaluate_request(
            "/api/rules/rules-project/evaluate/Pricing%20Rule",
            Some("secret-token"),
            json!({ "context": { "cartTotal": 100 } }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["cartTotal"], json!(100));
    assert!(body["performance"].is_string());
    assert!(body.get("details").is_none(), "no legacy details wrapper");
    assert_eq!(
        headers.get("X-Release-Id").map(|v| v.to_str().unwrap()),
        Some("22222222-2222-2222-2222-222222222222")
    );
    assert_eq!(body["meta"]["type"], json!("release"));
    assert_eq!(body["meta"]["path"], json!("Pricing Rule"));
    assert_eq!(
        body["meta"]["release"],
        json!({
            "id": "22222222-2222-2222-2222-222222222222",
            "name": null,
            "version": "1.2.3",
            "status": null,
            "commitId": null
        })
    );
    assert!(body["meta"].get("environment").is_none());
}

#[tokio::test]
async fn evaluate_returns_full_environment_meta() {
    let router = rules_router().await;
    let (status, headers, body) = send(
        &router,
        evaluate_request(
            "/api/rules/env-project/evaluate/plain-rule",
            Some("env-token"),
            json!({ "context": { "hello": "world" } }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["hello"], json!("world"));
    assert_eq!(
        body["meta"],
        json!({
            "type": "environment",
            "path": "plain-rule",
            "project": {
                "id": "33333333-3333-3333-3333-333333333333",
                "key": "env-project",
                "name": "Env Project"
            },
            "release": {
                "id": "44444444-4444-4444-4444-444444444444",
                "name": "Env Release",
                "version": "2.0.0",
                "status": "published",
                "commitId": "55555555-5555-5555-5555-555555555555"
            },
            "environment": {
                "id": "66666666-6666-6666-6666-666666666666",
                "key": "production",
                "name": "Production"
            }
        })
    );
    assert_eq!(
        headers.get("X-Release-Id").map(|v| v.to_str().unwrap()),
        Some("44444444-4444-4444-4444-444444444444")
    );
}

#[tokio::test]
async fn evaluate_includes_trace_when_requested() {
    let router = rules_router().await;
    let (status, _, body) = send(
        &router,
        evaluate_request(
            "/api/rules/rules-project/evaluate/plain-rule",
            Some("secret-token"),
            json!({ "context": { "hello": "world" }, "trace": true }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.get("trace").is_some());
}

#[tokio::test]
async fn evaluate_rejects_bad_token() {
    let router = rules_router().await;
    let (status, _, body) = send(
        &router,
        evaluate_request(
            "/api/rules/rules-project/evaluate/plain-rule",
            Some("wrong-token"),
            json!({ "context": {} }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], json!("unauthorized"));
}

#[tokio::test]
async fn evaluate_unknown_project_is_404() {
    let router = rules_router().await;
    let (status, _, body) = send(
        &router,
        evaluate_request(
            "/api/rules/nope/evaluate/plain-rule",
            None,
            json!({ "context": {} }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], json!("project.notFound"));
}

#[tokio::test]
async fn evaluate_unknown_key_is_documents_invalid() {
    let router = rules_router().await;
    let (status, _, body) = send(
        &router,
        evaluate_request(
            "/api/rules/rules-project/evaluate/does-not-exist",
            Some("secret-token"),
            json!({ "context": {} }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("documents.invalid"));
    assert_eq!(body["key"], json!("does-not-exist"));
}

#[tokio::test]
async fn evaluate_custom_node_is_integrations_not_supported() {
    let router = rules_router().await;
    let (status, _, body) = send(
        &router,
        evaluate_request(
            "/api/rules/rules-project/evaluate/custom-rule",
            Some("secret-token"),
            json!({ "context": {} }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("evaluate.integrationsNotSupported"));
}

#[tokio::test]
async fn evaluate_without_release_metadata_is_open_and_headerless() {
    let router = rules_router().await;
    let (status, headers, body) = send(
        &router,
        evaluate_request(
            "/api/rules/no-config-project/evaluate/plain-rule",
            None,
            json!({ "context": { "hello": "world" } }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["hello"], json!("world"));
    assert!(headers.get("X-Release-Id").is_none());
    assert!(body.get("meta").is_none(), "no meta without project.json");
}

fn list_request(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::get(uri);
    if let Some(token) = token {
        builder = builder.header("X-Access-Token", token);
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn list_returns_openapi_document() {
    let router = rules_router().await;
    let (status, _, document) = send(
        &router,
        list_request("/api/rules/rules-project", Some("secret-token")),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(document["openapi"], json!("3.0.3"));
    assert_eq!(document["info"]["title"], json!("rules-project rules API"));
    assert_eq!(document["info"]["version"], json!("1.2.3"));
    assert_eq!(
        document["servers"][0]["url"],
        json!("/api/rules/rules-project")
    );
    assert_eq!(
        document["x-gorules"]["source"],
        json!({
            "type": "release",
            "releaseId": "22222222-2222-2222-2222-222222222222",
            "version": "1.2.3"
        })
    );

    let paths = document["paths"].as_object().unwrap();
    assert_eq!(paths.len(), 3);
    assert!(
        paths.contains_key("/evaluate/Pricing Rule"),
        "original casing preserved"
    );
    assert!(paths.contains_key("/evaluate/plain-rule"));
    assert!(paths.contains_key("/evaluate/custom-rule"));
}

#[tokio::test]
async fn list_entry_carries_schemas_and_examples() {
    let router = rules_router().await;
    let (_, _, document) = send(
        &router,
        list_request("/api/rules/rules-project", Some("secret-token")),
    )
    .await;

    let post = &document["paths"]["/evaluate/Pricing Rule"]["post"];
    assert_eq!(post["summary"], json!("Pricing"));
    assert_eq!(post["operationId"], json!("evaluate_Pricing_Rule"));
    assert_eq!(post["x-gorules"]["contentHash"], json!("3a5b"));
    assert_eq!(post["x-gorules"]["hasInputSchema"], json!(true));

    let content = &post["requestBody"]["content"]["application/json"];
    assert_eq!(
        content["schema"]["properties"]["context"]["properties"]["cartTotal"],
        json!({ "type": "number" })
    );

    let examples = content["examples"].as_object().unwrap();
    assert_eq!(examples.len(), 3, "capped at 3, disabled case skipped");
    assert_eq!(examples["example_1"]["summary"], json!("small cart"));
    assert_eq!(examples["example_2"]["summary"], json!("medium cart"));
    assert_eq!(examples["example_3"]["summary"], json!("large cart"));
    assert_eq!(
        examples["example_1"]["x-source"],
        json!("Pricing Rule.test")
    );

    let plain = &document["paths"]["/evaluate/plain-rule"]["post"];
    assert_eq!(
        plain["requestBody"]["content"]["application/json"]["schema"]["properties"]["context"],
        json!({ "type": "object" }),
        "generic fallback when no schema declared"
    );
    assert_eq!(
        plain["requestBody"]["content"]["application/json"].get("examples"),
        None
    );
}

#[tokio::test]
async fn list_environment_config_document() {
    let router = rules_router().await;
    let (status, _, document) = send(
        &router,
        list_request("/api/rules/env-project", Some("env-token")),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(document["info"]["version"], json!("2.0.0"));
    assert_eq!(
        document["x-gorules"]["source"],
        json!({
            "type": "environment",
            "environmentId": "66666666-6666-6666-6666-666666666666",
            "environmentKey": "production",
            "releaseId": "44444444-4444-4444-4444-444444444444"
        })
    );

    let schema = &document["paths"]["/evaluate/plain-rule"]["post"]["responses"]["200"]["content"]
        ["application/json"]["schema"];
    assert_eq!(schema["required"], json!(["performance", "result", "meta"]));
    assert_eq!(
        schema["properties"]["meta"]["properties"]["type"]["enum"],
        json!(["environment"])
    );
}

#[tokio::test]
async fn list_requires_token_when_configured() {
    let router = rules_router().await;
    let (status, _, body) = send(&router, list_request("/api/rules/rules-project", None)).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], json!("unauthorized"));
}

#[tokio::test]
async fn list_download_sets_attachment_header() {
    let router = rules_router().await;
    let (status, headers, _) = send(
        &router,
        list_request(
            "/api/rules/rules-project?download=true",
            Some("secret-token"),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-disposition")
            .map(|v| v.to_str().unwrap()),
        Some("attachment; filename=\"rules-project-openapi.json\"")
    );
}

#[tokio::test]
async fn list_without_release_metadata_uses_fallbacks() {
    let router = rules_router().await;
    let (status, headers, document) =
        send(&router, list_request("/api/rules/no-config-project", None)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("content-disposition").is_none());
    assert_eq!(document["info"]["version"], json!("1.0.0"));
    assert_eq!(
        document["info"]["title"],
        json!("no-config-project rules API")
    );
    assert_eq!(document["x-gorules"]["source"]["releaseId"], json!(null));

    // Download filename falls back to the path parameter when release
    // metadata (and thus the project key) is absent.
    let (_, headers, _) = send(
        &router,
        list_request("/api/rules/no-config-project?download=true", None),
    )
    .await;
    assert_eq!(
        headers
            .get("content-disposition")
            .map(|v| v.to_str().unwrap()),
        Some("attachment; filename=\"no-config-project-openapi.json\"")
    );

    let schema = &document["paths"]["/evaluate/plain-rule"]["post"]["responses"]["200"]["content"]
        ["application/json"]["schema"];
    assert_eq!(schema.get("required"), None);
    assert!(schema["properties"].get("meta").is_none());
    assert_eq!(schema["properties"]["trace"]["type"], json!("object"));
}
