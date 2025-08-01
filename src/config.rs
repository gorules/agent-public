use anyhow::Context;
use axum_server::tls_rustls::RustlsConfig;
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use serde::de::Error;
use serde::{Deserialize, Deserializer};
use std::sync::Arc;
use std::time::Duration;
use strum_macros::AsRefStr;

#[derive(Debug, Clone, Deserialize)]
pub struct EnvironmentConfig {
    #[serde(default)]
    pub cors_permissive: bool,
    pub provider: ProviderConfig,

    #[serde(default)]
    pub release_zip_password: Option<Arc<str>>,

    #[serde(
        deserialize_with = "deserialize_duration",
        default = "default_refresh_interval"
    )]
    pub poll_interval: Duration,

    #[serde(default)]
    pub otel_enabled: bool,

    #[serde(default)]
    pub http_ssl: Option<HttpSslConfig>,
}

fn default_refresh_interval() -> Duration {
    Duration::from_millis(5_000)
}

impl Default for EnvironmentConfig {
    fn default() -> Self {
        Self {
            cors_permissive: true,
            release_zip_password: None,
            provider: ProviderConfig::default(),
            poll_interval: Duration::from_millis(5_000),
            otel_enabled: false,
            http_ssl: None,
        }
    }
}

pub fn deserialize_duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let millis = <u64>::deserialize(deserializer)?;
    if millis < 1_000 {
        return Err(Error::custom(format!(
            "poll interval must be at least 1000 milliseconds ({millis} given)"
        )));
    }

    Ok(Duration::from_millis(millis))
}

#[derive(Debug, Clone, Deserialize, AsRefStr)]
#[serde(tag = "type")]
pub enum ProviderConfig {
    Zip(ZipProviderConfig),
    Filesystem(FilesystemProviderConfig),
    S3(S3ProviderConfig),
    AzureStorage(AzureStorageProviderConfig),
    GCS(GcsProviderConfig),
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self::Zip(ZipProviderConfig::default())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ZipProviderConfig {
    #[serde(default = "default_root")]
    pub root_dir: String,
}

impl Default for ZipProviderConfig {
    fn default() -> Self {
        Self {
            root_dir: default_root(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilesystemProviderConfig {
    #[serde(default = "default_root")]
    pub root_dir: String,
}

impl Default for FilesystemProviderConfig {
    fn default() -> Self {
        Self {
            root_dir: default_root(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct S3ProviderConfig {
    pub bucket: String,
    #[serde(default)]
    pub force_path_style: bool,
    pub endpoint: Option<String>,
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AzureStorageProviderConfig {
    pub connection_string: String,
    pub container: String,
    pub prefix: Option<String>,
    #[allow(dead_code)]
    pub sas_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GcsProviderConfig {
    pub base64_contents: String,
    pub bucket: String,
    pub prefix: Option<String>,
}

pub fn default_root() -> String {
    "data".to_string()
}

#[derive(Debug, Clone, Default)]
pub struct GlobalAgentConfig {
    pub release_zip_password: Option<Arc<str>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpSslConfig {
    pub key: String,
    pub cert: String,
}

impl HttpSslConfig {
    pub async fn to_rustls_config(&self) -> anyhow::Result<RustlsConfig> {
        let cert = BASE64_STANDARD
            .decode(&self.cert)
            .context("Failed to decode SSL certificate")?;
        let key = BASE64_STANDARD
            .decode(&self.key)
            .context("Failed to decode SSL key")?;

        Ok(RustlsConfig::from_pem(cert, key).await?)
    }
}
