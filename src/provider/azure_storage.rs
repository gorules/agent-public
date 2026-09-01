use crate::Agent;
use crate::config::{AzureStorageProviderConfig, GlobalAgentConfig};
use crate::immutable_loader::{ImmutableLoader, ProtectedZipArchive};
use crate::provider::{
    AgentData, AgentDataProvider, FailedProjectsRegistry, Project, ProjectData, ProjectDiff,
};
use crate::util::prefix::Prefix;
use anyhow::Context;
use async_trait::async_trait;
use azure_core::credentials::{AccessToken, Secret, TokenCredential, TokenRequestOptions};
use azure_core::http::policies::{Policy, PolicyResult};
use azure_core::http::{ClientOptions, Etag, Request, Url};
use azure_identity::{
    ClientSecretCredential, DeveloperToolsCredential, ManagedIdentityCredential,
    WorkloadIdentityCredential,
};
use azure_storage_blob::models::BlobContainerClientListBlobsOptions;
use azure_storage_blob::{BlobContainerClient, BlobContainerClientOptions};
use dashmap::DashMap;
use futures::{StreamExt, TryStreamExt};
use std::collections::BTreeMap;
use std::future::Future;
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use zip::ZipArchive;

#[derive(Clone)]
pub struct AzureStorageProvider {
    client: Arc<BlobContainerClient>,
    prefix: Prefix,
    global_config: Arc<GlobalAgentConfig>,
}

impl std::fmt::Debug for AzureStorageProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureStorageProvider")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

/// How the provider authenticates, decided from config at startup.
#[derive(Debug, PartialEq)]
enum AzureAuth {
    /// Connection string carrying an AccountKey or SAS — shared-key/SAS credentials.
    ConnectionString(String),
    /// Entra ID (IAM) via a chained credential for the given account.
    Iam { account_name: String },
}

#[derive(Debug, Default)]
struct ParsedConnectionString {
    account_name: Option<String>,
    account_key: Option<String>,
    sas: Option<String>,
    blob_endpoint: Option<String>,
    protocol: Option<String>,
    endpoint_suffix: Option<String>,
}

fn parse_connection_string(raw: &str) -> anyhow::Result<ParsedConnectionString> {
    let mut parsed = ParsedConnectionString::default();
    for segment in raw.split(';') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }

        let (key, value) = segment
            .split_once('=')
            .with_context(|| format!("Invalid connection string segment: {segment}"))?;
        let value = value.trim();
        if value.is_empty() {
            continue;
        }

        match key.trim().to_ascii_lowercase().as_str() {
            "accountname" => parsed.account_name = Some(value.to_string()),
            "accountkey" => parsed.account_key = Some(value.to_string()),
            "sharedaccesssignature" => parsed.sas = Some(value.to_string()),
            "blobendpoint" => parsed.blob_endpoint = Some(value.to_string()),
            "defaultendpointsprotocol" => parsed.protocol = Some(value.to_string()),
            "endpointsuffix" => parsed.endpoint_suffix = Some(value.to_string()),
            _ => {}
        }
    }

    Ok(parsed)
}

fn resolve_auth(config: &AzureStorageProviderConfig) -> anyhow::Result<AzureAuth> {
    // The config crate surfaces empty env vars as Some(""), so treat blank as unset.
    let non_empty = |value: &Option<String>| -> Option<String> {
        value
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    match (
        non_empty(&config.connection_string),
        non_empty(&config.account_name),
    ) {
        (Some(_), Some(_)) => anyhow::bail!(
            "Both PROVIDER__CONNECTION_STRING and PROVIDER__ACCOUNT_NAME are set; configure exactly one"
        ),
        (None, None) => anyhow::bail!(
            "Azure provider requires either PROVIDER__CONNECTION_STRING (key or SAS) or PROVIDER__ACCOUNT_NAME (IAM/Entra ID)"
        ),
        (None, Some(account_name)) => Ok(AzureAuth::Iam { account_name }),
        (Some(raw), None) => {
            let parsed = parse_connection_string(&raw).context("Invalid connection string")?;
            if parsed.account_key.is_some() || parsed.sas.is_some() {
                return Ok(AzureAuth::ConnectionString(raw));
            }
            let account_name = parsed.account_name.context(
                "Connection string has no AccountKey/SharedAccessSignature and no AccountName",
            )?;
            tracing::warn!(
                "Keyless connection string detected; using IAM/Entra ID auth. Prefer PROVIDER__ACCOUNT_NAME."
            );
            Ok(AzureAuth::Iam { account_name })
        }
    }
}

#[derive(Debug)]
pub struct AzureSharedKeyPolicy {
    account: String,
    key: Secret,
}

impl AzureSharedKeyPolicy {
    pub fn new(account: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            account: account.into(),
            key: Secret::new(key.into()),
        }
    }

    fn string_to_sign(&self, request: &Request) -> String {
        let headers = request.headers();
        let header = |name: &str| -> &str {
            headers
                .iter()
                .find(|(key, _)| key.as_str().eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str().trim())
                .unwrap_or_default()
        };

        let content_length = match request.body().len() {
            Some(len) if len > 0 => len.to_string(),
            _ => String::new(),
        };

        let mut ms_headers: Vec<(String, &str)> = headers
            .iter()
            .map(|(key, value)| (key.as_str().to_ascii_lowercase(), value.as_str().trim()))
            .filter(|(key, _)| key.starts_with("x-ms-"))
            .collect();
        ms_headers.sort();
        let canonicalized_headers: String = ms_headers
            .iter()
            .map(|(key, value)| format!("{key}:{value}\n"))
            .collect();

        let url = request.url();
        let mut canonicalized_resource = format!("/{}{}", self.account, url.path());
        let mut params: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (key, value) in url.query_pairs() {
            params
                .entry(key.to_ascii_lowercase())
                .or_default()
                .push(value.into_owned());
        }
        for (key, mut values) in params {
            values.sort();
            canonicalized_resource.push_str(&format!("\n{key}:{}", values.join(",")));
        }

        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n\n{}\n{}\n{}\n{}\n{}\n{}{}",
            request.method(),
            header("content-encoding"),
            header("content-language"),
            content_length,
            header("content-md5"),
            header("content-type"),
            header("if-modified-since"),
            header("if-match"),
            header("if-none-match"),
            header("if-unmodified-since"),
            header("range"),
            canonicalized_headers,
            canonicalized_resource,
        )
    }
}

#[async_trait]
impl Policy for AzureSharedKeyPolicy {
    async fn send(
        &self,
        ctx: &azure_core::http::Context,
        request: &mut Request,
        next: &[Arc<dyn Policy>],
    ) -> PolicyResult {
        let date = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        request.insert_header("x-ms-date", date);

        let signature = azure_core::hmac::hmac_sha256(&self.string_to_sign(request), &self.key)?;
        request.insert_header(
            "authorization",
            format!("SharedKey {}:{}", self.account, signature),
        );

        next[0].send(ctx, request, &next[1..]).await
    }
}

#[derive(Debug)]
struct ChainedTokenCredential {
    sources: Vec<Arc<dyn TokenCredential>>,
    selected: AtomicUsize,
}

#[async_trait]
impl TokenCredential for ChainedTokenCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        let selected = self.selected.load(Ordering::Relaxed);
        if selected != usize::MAX {
            return self.sources[selected].get_token(scopes, options).await;
        }

        let mut last_error = None;
        for (index, source) in self.sources.iter().enumerate() {
            match source.get_token(scopes, options.clone()).await {
                Ok(token) => {
                    self.selected.store(index, Ordering::Relaxed);
                    return Ok(token);
                }
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            azure_core::Error::with_message(
                azure_core::error::ErrorKind::Credential,
                "No Azure credential source is available",
            )
        }))
    }
}

fn default_credential_chain() -> anyhow::Result<Arc<dyn TokenCredential>> {
    let mut sources: Vec<Arc<dyn TokenCredential>> = Vec::new();

    if let (Ok(tenant_id), Ok(client_id), Ok(client_secret)) = (
        std::env::var("AZURE_TENANT_ID"),
        std::env::var("AZURE_CLIENT_ID"),
        std::env::var("AZURE_CLIENT_SECRET"),
    ) && let Ok(credential) =
        ClientSecretCredential::new(&tenant_id, client_id, client_secret.into(), None)
    {
        sources.push(credential);
    }

    if let Ok(credential) = WorkloadIdentityCredential::new(None) {
        sources.push(credential);
    }
    if let Ok(credential) = ManagedIdentityCredential::new(None) {
        sources.push(credential);
    }
    if let Ok(credential) = DeveloperToolsCredential::new(None) {
        sources.push(credential);
    }

    anyhow::ensure!(!sources.is_empty(), "Invalid credential");
    Ok(Arc::new(ChainedTokenCredential {
        sources,
        selected: AtomicUsize::new(usize::MAX),
    }))
}

impl AzureStorageProvider {
    pub fn new(
        config: &AzureStorageProviderConfig,
        global_config: Arc<GlobalAgentConfig>,
    ) -> anyhow::Result<Self> {
        let client = match resolve_auth(config)? {
            AzureAuth::ConnectionString(raw) => {
                let parsed = parse_connection_string(&raw).context("Invalid connection string")?;
                let endpoint = match &parsed.blob_endpoint {
                    Some(endpoint) => endpoint.trim_end_matches('/').to_string(),
                    None => {
                        let account_name = parsed
                            .account_name
                            .as_deref()
                            .context("Invalid account name")?;
                        let protocol = parsed.protocol.as_deref().unwrap_or("https");
                        let suffix = parsed
                            .endpoint_suffix
                            .as_deref()
                            .unwrap_or("core.windows.net");
                        format!("{protocol}://{account_name}.blob.{suffix}")
                    }
                };

                let mut url = Url::parse(&format!("{endpoint}/{}", config.container))
                    .context("Invalid blob endpoint")?;

                if let Some(account_key) = &parsed.account_key {
                    let account_name = parsed
                        .account_name
                        .as_deref()
                        .context("Invalid account name")?;
                    let options = BlobContainerClientOptions {
                        client_options: ClientOptions {
                            per_try_policies: vec![Arc::new(AzureSharedKeyPolicy::new(
                                account_name,
                                account_key.as_str(),
                            ))
                                as Arc<dyn Policy>],
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    BlobContainerClient::new(url, None, Some(options))
                        .context("Invalid storage credentials")?
                } else {
                    let sas = parsed
                        .sas
                        .as_deref()
                        .context("Invalid storage credentials")?;
                    url.set_query(Some(sas.trim_start_matches('?')));
                    BlobContainerClient::new(url, None, None)
                        .context("Invalid storage credentials")?
                }
            }
            AzureAuth::Iam { account_name } => {
                let url = Url::parse(&format!(
                    "https://{account_name}.blob.core.windows.net/{}",
                    config.container
                ))
                .context("Invalid blob endpoint")?;
                let credential = default_credential_chain().context("Invalid credential")?;
                BlobContainerClient::new(url, Some(credential), None)
                    .context("Invalid storage credentials")?
            }
        };

        Ok(AzureStorageProvider {
            client: Arc::new(client),
            prefix: Prefix::from(config.prefix.clone()),
            global_config,
        })
    }

    async fn generate_projects(&self, keys: Vec<String>) -> DashMap<String, Arc<Project>> {
        let array = futures::stream::iter(keys.into_iter())
            .map(|key| {
                let blob_client = self
                    .client
                    .blob_client(self.prefix.prepend(key.as_str().into()).as_ref());

                async move {
                    let result = match blob_client.download(None).await {
                        Ok(result) => result,
                        Err(e) => {
                            tracing::error!("[AZURE - SKIP] Failed to get blob {}: {}", key, e);
                            return None;
                        }
                    };

                    let content_hash = etag_hash(result.properties.etag.as_ref());
                    let data = match result.body.collect().await {
                        Ok(data) => data,
                        Err(e) => {
                            tracing::error!(
                                "[AZURE - SKIP] Failed to collect blob data {}: {}",
                                key,
                                e
                            );
                            return None;
                        }
                    };

                    let cursor = Cursor::new(data);
                    let archive = ProtectedZipArchive {
                        archive: match ZipArchive::new(cursor) {
                            Ok(archive) => archive,
                            Err(err) => {
                                tracing::error!(
                                    "[AZURE - SKIP] failed unpack zip archive {}: {}",
                                    key,
                                    err
                                );
                                return None;
                            }
                        },
                        password: self.global_config.release_zip_password.clone(),
                    };

                    let engine = match ImmutableLoader::try_from(archive) {
                        Ok(loader) => loader.into_engine(),
                        Err(err) => {
                            tracing::error!(
                                "[AZURE - SKIP] failed load into engine {}: {}",
                                key,
                                err
                            );
                            match content_hash {
                                Some(etag) => FailedProjectsRegistry::insert(etag),
                                None => (),
                            }
                            return None;
                        }
                    };

                    Some((
                        key,
                        Arc::new(Project {
                            engine,
                            content_hash,
                            rules_spec: OnceLock::new(),
                        }),
                    ))
                }
            })
            .buffered(100)
            .filter_map(|result| async { result })
            .collect::<Vec<(String, Arc<Project>)>>()
            .await;

        array.into_iter().collect::<DashMap<String, Arc<Project>>>()
    }
}

impl AgentDataProvider for AzureStorageProvider {
    fn load_data(
        &self,
        data: Arc<AgentData>,
    ) -> impl Future<Output = anyhow::Result<Vec<ProjectDiff>>> + Send + 'static {
        let this = self.clone();

        async move {
            let options = BlobContainerClientListBlobsOptions {
                prefix: this.prefix.to_string(),
                maxresults: Some(1_000),
                ..Default::default()
            };

            let mut pager = this
                .client
                .list_blobs(Some(options))
                .context("Failed to list blobs")?;

            let mut project_datum: Vec<ProjectData> = Vec::new();
            while let Some(item) = pager.try_next().await.context(
                "Failed to list blobs — if authentication succeeded, ensure the identity \
                 has the 'Storage Blob Data Reader' role on the storage account or container",
            )? {
                let Some(name) = item.name else { continue };

                let key = this.prefix.strip(name.as_str().into()).into_owned();
                if key.contains('/') {
                    continue;
                }

                let project_data = ProjectData {
                    key,
                    content_hash: etag_hash(item.properties.as_ref().and_then(|p| p.etag.as_ref())),
                };
                if FailedProjectsRegistry::has_failed(project_data.content_hash.as_deref()) {
                    continue;
                }

                project_datum.push(project_data);
            }

            let diff = data.calculate_diff(project_datum);

            let to_refresh = Agent::get_refresh_list(&diff);

            let refreshed_projects = this.generate_projects(to_refresh).await;
            let diff = Agent::get_diff_result(data, diff, refreshed_projects);

            Ok(diff)
        }
    }
}

fn etag_hash(etag: Option<&Etag>) -> Option<Vec<u8>> {
    etag.map(|etag| etag.as_ref().trim_matches('"').as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(
        connection_string: Option<&str>,
        account_name: Option<&str>,
    ) -> AzureStorageProviderConfig {
        AzureStorageProviderConfig {
            connection_string: connection_string.map(str::to_string),
            account_name: account_name.map(str::to_string),
            container: "releases".to_string(),
            prefix: None,
        }
    }

    #[test]
    fn key_bearing_connection_string_uses_connection_string_auth() {
        let raw = "DefaultEndpointsProtocol=https;AccountName=acc;AccountKey=a2V5;EndpointSuffix=core.windows.net";
        let auth = resolve_auth(&config(Some(raw), None)).unwrap();
        assert_eq!(auth, AzureAuth::ConnectionString(raw.to_string()));
    }

    #[test]
    fn sas_connection_string_uses_connection_string_auth() {
        let raw = "BlobEndpoint=https://acc.blob.core.windows.net;AccountName=acc;SharedAccessSignature=sv=2022-11-02&sig=abc";
        let auth = resolve_auth(&config(Some(raw), None)).unwrap();
        assert_eq!(auth, AzureAuth::ConnectionString(raw.to_string()));
    }

    #[test]
    fn account_name_uses_iam() {
        let auth = resolve_auth(&config(None, Some("myaccount"))).unwrap();
        assert_eq!(
            auth,
            AzureAuth::Iam {
                account_name: "myaccount".to_string()
            }
        );
    }

    #[test]
    fn keyless_connection_string_uses_iam_compat() {
        let auth = resolve_auth(&config(Some("AccountName=myaccount"), None)).unwrap();
        assert_eq!(
            auth,
            AzureAuth::Iam {
                account_name: "myaccount".to_string()
            }
        );
    }

    #[test]
    fn keyless_connection_string_without_account_name_errors() {
        let err = resolve_auth(&config(Some("EndpointSuffix=core.windows.net"), None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("AccountName"), "unexpected error: {err}");
    }

    #[test]
    fn both_settings_error() {
        let err = resolve_auth(&config(
            Some("AccountName=acc;AccountKey=a2V5"),
            Some("other"),
        ))
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("PROVIDER__CONNECTION_STRING") && err.contains("PROVIDER__ACCOUNT_NAME"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn neither_setting_errors() {
        let err = resolve_auth(&config(None, None)).unwrap_err().to_string();
        assert!(
            err.contains("PROVIDER__CONNECTION_STRING") && err.contains("PROVIDER__ACCOUNT_NAME"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn empty_connection_string_with_account_name_uses_iam() {
        let auth = resolve_auth(&config(Some("  "), Some("myaccount"))).unwrap();
        assert_eq!(
            auth,
            AzureAuth::Iam {
                account_name: "myaccount".to_string()
            }
        );
    }

    #[test]
    fn empty_account_name_with_key_string_uses_connection_string_auth() {
        let raw = "AccountName=acc;AccountKey=a2V5";
        let auth = resolve_auth(&config(Some(raw), Some(""))).unwrap();
        assert_eq!(auth, AzureAuth::ConnectionString(raw.to_string()));
    }

    #[test]
    fn shared_key_string_to_sign_canonicalizes_headers_and_query() {
        let policy = AzureSharedKeyPolicy::new("devstoreaccount1", "a2V5");
        let url = Url::parse(
            "http://127.0.0.1:10000/devstoreaccount1/releases?restype=container&comp=list&prefix=some/prefix/",
        )
        .unwrap();
        let mut request = Request::new(url, azure_core::http::Method::Get);
        request.insert_header("x-ms-version", "2025-01-05");
        request.insert_header("x-ms-date", "Sun, 17 Aug 2026 10:00:00 GMT");
        request.insert_header("accept", "application/xml");

        let string_to_sign = policy.string_to_sign(&request);
        assert_eq!(
            string_to_sign,
            "GET\n\n\n\n\n\n\n\n\n\n\n\n\
             x-ms-date:Sun, 17 Aug 2026 10:00:00 GMT\n\
             x-ms-version:2025-01-05\n\
             /devstoreaccount1/devstoreaccount1/releases\
             \ncomp:list\nprefix:some/prefix/\nrestype:container"
        );
    }
}
