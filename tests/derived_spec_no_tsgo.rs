use agent::app;
use agent::config::{EnvironmentConfig, ProviderConfig, TsgoConfig, ZipProviderConfig};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The analyzer is a process-wide singleton, so the disabled path needs its own
/// test binary — it cannot share one with the tests that expect tsgo to run.
async fn document() -> Value {
    let config = EnvironmentConfig {
        provider: ProviderConfig::Zip(ZipProviderConfig {
            root_dir: "tests/data/derived".to_string(),
        }),
        tsgo: TsgoConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let agent = app::create_agent(config.clone(), Default::default()).await;
    let router = app::create_app(agent, config).await;

    let request = Request::get("/api/rules/derived-project")
        .header("X-Access-Token", "derived-token")
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Without tsgo the function node is opaque, so `Scoring` has no output schema
/// to publish — which is also what proves the schema in the enabled case came
/// from the TypeScript compiler and nowhere else.
#[tokio::test]
async fn function_node_output_is_unresolved_without_tsgo() {
    let document = document().await;
    let post = &document["paths"]["/evaluate/Scoring"]["post"];

    assert_eq!(post["x-gorules"]["hasOutputSchema"], json!(false));
    assert_eq!(
        post["responses"]["200"]["content"]["application/json"]["schema"]["properties"]["result"],
        json!({ "type": "object" }),
        "falls back to the generic object"
    );
}

/// Everything that does not depend on TypeScript still resolves, so disabling
/// tsgo degrades the document rather than emptying it.
#[tokio::test]
async fn engine_derived_schemas_survive_without_tsgo() {
    let document = document().await;

    let scoring = &document["paths"]["/evaluate/Scoring"]["post"];
    assert_eq!(scoring["x-gorules"]["hasInputSchema"], json!(true));

    let inferred = &document["paths"]["/evaluate/Inferred"]["post"];
    assert_eq!(inferred["x-gorules"]["hasInputSchema"], json!(true));
    assert_eq!(
        inferred["x-gorules"]["skeleton"],
        json!({ "cart": { "price": null, "quantity": null } })
    );

    assert_eq!(document["paths"].as_object().unwrap().len(), 3);
}
