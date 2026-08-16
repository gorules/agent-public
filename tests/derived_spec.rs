use agent::app;
use agent::config::{EnvironmentConfig, ProviderConfig, ZipProviderConfig};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn derived_router() -> Router {
    let config = EnvironmentConfig {
        provider: ProviderConfig::Zip(ZipProviderConfig {
            root_dir: "tests/data/derived".to_string(),
        }),
        ..Default::default()
    };

    let agent = app::create_agent(config.clone(), Default::default()).await;
    app::create_app(agent, config).await
}

async fn document() -> Value {
    let router = derived_router().await;
    let request = Request::get("/api/rules/derived-project")
        .header("X-Access-Token", "derived-token")
        .body(Body::empty())
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// The whole point of wiring tsgo: `Scoring` declares no output schema, so the
/// only way `score`/`tier` can appear is if the TypeScript compiler resolved
/// the function node's return type.
#[tokio::test]
async fn function_node_return_type_reaches_the_response_schema() {
    let document = document().await;
    let post = &document["paths"]["/evaluate/Scoring"]["post"];
    let result =
        &post["responses"]["200"]["content"]["application/json"]["schema"]["properties"]["result"];

    assert_eq!(result["properties"]["score"], json!({ "type": "number" }));
    assert_eq!(
        result["properties"]["tier"],
        json!({ "const": "gold" }),
        "`as const` survives the round trip through tsgo"
    );
    assert_eq!(post["x-gorules"]["hasOutputSchema"], json!(true));
}

#[tokio::test]
async fn declared_input_schema_still_wins() {
    let document = document().await;
    let context = &document["paths"]["/evaluate/Scoring"]["post"]["requestBody"]["content"]["application/json"]
        ["schema"]["properties"]["context"];

    assert_eq!(context["properties"]["amount"], json!({ "type": "number" }));
    assert_eq!(context["required"], json!(["amount"]));
}

/// `Inferred` declares nothing at all — both the request schema and the
/// skeleton come from what the engine saw the expression read.
#[tokio::test]
async fn undeclared_inputs_are_inferred_from_expressions() {
    let document = document().await;
    let post = &document["paths"]["/evaluate/Inferred"]["post"];

    let context =
        &post["requestBody"]["content"]["application/json"]["schema"]["properties"]["context"];
    assert_eq!(context["required"], json!(["cart"]));
    assert!(
        context["properties"]["cart"]["properties"]
            .get("price")
            .is_some(),
        "dotted reference nests into an object: {context}"
    );

    assert_eq!(
        post["x-gorules"]["skeleton"],
        json!({ "cart": { "price": null, "quantity": null } })
    );
    assert_eq!(post["x-gorules"]["hasInputSchema"], json!(true));
}

#[tokio::test]
async fn policies_are_published_alongside_graphs() {
    let document = document().await;
    let paths = document["paths"].as_object().expect("paths object");

    assert_eq!(
        paths.keys().collect::<Vec<_>>(),
        vec![
            "/evaluate/Eligibility",
            "/evaluate/Inferred",
            "/evaluate/Scoring"
        ]
    );

    let policy = &document["paths"]["/evaluate/Eligibility"]["post"];
    assert_eq!(policy["tags"], json!(["policy"]));
    assert_eq!(policy["x-gorules"]["kind"], json!("policy"));
    assert_eq!(policy["summary"], json!("Eligibility"));

    let graph = &document["paths"]["/evaluate/Scoring"]["post"];
    assert_eq!(graph["tags"], json!(["graph"]));
    assert_eq!(graph["x-gorules"]["kind"], json!("graph"));
}

#[tokio::test]
async fn schemas_omit_json_schema_metadata_keys() {
    let document = document().await;
    let context = &document["paths"]["/evaluate/Inferred"]["post"]["requestBody"]["content"]["application/json"]
        ["schema"]["properties"]["context"];

    assert_eq!(context.get("$schema"), None, "OpenAPI 3.0 rejects $schema");
    assert_eq!(context.get("$id"), None, "OpenAPI 3.0 rejects $id");
}

/// Derivation is memoized per project, so the document is byte-identical
/// across requests and the second one never re-enters the compiler.
#[tokio::test]
async fn repeated_requests_return_the_cached_document() {
    let router = derived_router().await;

    let fetch = || async {
        let request = Request::get("/api/rules/derived-project")
            .header("X-Access-Token", "derived-token")
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        to_bytes(response.into_body(), usize::MAX).await.unwrap()
    };

    assert_eq!(fetch().await, fetch().await);
}
