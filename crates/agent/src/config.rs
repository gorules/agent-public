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

    #[serde(default)]
    pub tsgo: TsgoConfig,
}

fn default_refresh_interval() -> Duration {
    Duration::from_millis(5_000)
}

/// Type resolution for function nodes and policies, which runs the TypeScript
/// compiler as a WASI module under wasmtime.
#[derive(Debug, Clone, Deserialize)]
pub struct TsgoConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_tsgo_memory_bytes")]
    pub memory_bytes: usize,

    #[serde(
        deserialize_with = "deserialize_millis",
        default = "default_tsgo_timeout"
    )]
    pub timeout: Duration,

    /// Resolved function types held per process, keyed by source and input
    /// type. Cleared wholesale when full — the working set is one release's
    /// functions, so this is a ceiling, not an eviction policy.
    #[serde(default = "default_tsgo_cache_capacity")]
    pub cache_capacity: usize,
}

fn default_true() -> bool {
    true
}

fn default_tsgo_memory_bytes() -> usize {
    2 << 30
}

fn default_tsgo_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_tsgo_cache_capacity() -> usize {
    4_096
}

impl Default for TsgoConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            memory_bytes: default_tsgo_memory_bytes(),
            timeout: default_tsgo_timeout(),
            cache_capacity: default_tsgo_cache_capacity(),
        }
    }
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
            tsgo: TsgoConfig::default(),
        }
    }
}

pub fn deserialize_millis<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Duration::from_millis(<u64>::deserialize(deserializer)?))
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
    pub connection_string: Option<String>,
    pub account_name: Option<String>,
    pub container: String,
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GcsProviderConfig {
    pub base64_contents: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use config::{Config, Environment};

    /// The README documents these names, so they are worth pinning: the
    /// `config` crate maps `TSGO__TIMEOUT` onto `tsgo.timeout` through the
    /// `__` separator, and a missing section still has to default cleanly.
    fn parse(vars: &[(&str, &str)]) -> EnvironmentConfig {
        let source = Environment::default()
            .separator("__")
            .try_parsing(true)
            .source(Some(
                vars.iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            ));

        Config::builder()
            .add_source(source)
            .set_default("provider.type", "Zip")
            .unwrap()
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }

    #[test]
    fn tsgo_defaults_when_unset() {
        let config = parse(&[]);

        assert!(config.tsgo.enabled);
        assert_eq!(config.tsgo.memory_bytes, 2 << 30);
        assert_eq!(config.tsgo.timeout, Duration::from_secs(30));
        assert_eq!(config.tsgo.cache_capacity, 4_096);
    }

    #[test]
    fn tsgo_reads_the_documented_env_vars() {
        let config = parse(&[
            ("TSGO__ENABLED", "false"),
            ("TSGO__MEMORY_BYTES", "1073741824"),
            ("TSGO__TIMEOUT", "5000"),
            ("TSGO__CACHE_CAPACITY", "16"),
        ]);

        assert!(!config.tsgo.enabled);
        assert_eq!(config.tsgo.memory_bytes, 1024 * 1024 * 1024);
        assert_eq!(config.tsgo.timeout, Duration::from_millis(5_000));
        assert_eq!(config.tsgo.cache_capacity, 16);
    }

    #[test]
    fn a_partial_tsgo_section_keeps_the_other_defaults() {
        let config = parse(&[("TSGO__ENABLED", "false")]);

        assert!(!config.tsgo.enabled);
        assert_eq!(config.tsgo.timeout, Duration::from_secs(30));
    }
}
