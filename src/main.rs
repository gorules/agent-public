use agent::config::{EnvironmentConfig, GlobalAgentConfig};
use agent::{app, telemetry};
use config::{Config, Environment};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const IS_DEVELOPMENT: bool = cfg!(debug_assertions);

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let settings = Config::builder()
        .add_source(Environment::default().separator("__").try_parsing(true))
        .set_default("provider.type", "Zip")
        .expect("Failed to set provider.type as Filesystem")
        .build()
        .expect("Failed to build settings");

    let cfg: EnvironmentConfig = settings
        .try_deserialize()
        .expect("Invalid environment variables");

    telemetry::setup(cfg.otel_enabled).ok();

    let rustls_config = match &cfg.http_ssl {
        None => None,
        Some(s) => {
            rustls::crypto::aws_lc_rs::default_provider()
                .install_default()
                .expect("Failed to install rustls crypto provider");
            Some(s.to_rustls_config().await.expect("Valid SSL Config"))
        }
    };

    let global_config = Arc::new(GlobalAgentConfig {
        release_zip_password: cfg.release_zip_password.clone(),
    });

    let agent = app::create_agent(cfg.clone(), global_config).await;
    let app = app::create_app(agent, cfg).await;

    let listener_address_str = IS_DEVELOPMENT
        .then_some("127.0.0.1:3000")
        .unwrap_or("0.0.0.0:8080");
    let listener_address =
        SocketAddr::from_str(listener_address_str).expect("Valid socket address");

    let server_result = match rustls_config {
        None => {
            tracing::info!("🚀 Listening on http://{listener_address}");
            axum_server::bind(listener_address)
                .serve(app.into_make_service())
                .await
        }
        Some(rustls_config) => {
            tracing::info!("🚀 Listening on https://{listener_address}");
            axum_server::bind_rustls(listener_address, rustls_config)
                .serve(app.into_make_service())
                .await
        }
    };

    if let Err(error) = server_result {
        tracing::error!("Server exited with an error: {error:?}");
    }
}
