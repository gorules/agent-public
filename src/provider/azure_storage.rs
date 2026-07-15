use crate::Agent;
use crate::config::{AzureStorageProviderConfig, GlobalAgentConfig};
use crate::immutable_loader::{ImmutableLoader, ProtectedZipArchive};
use crate::provider::{
    AgentData, AgentDataProvider, FailedProjectsRegistry, Project, ProjectData, ProjectDiff,
};
use crate::util::prefix::Prefix;
use anyhow::Context;
use azure_core::prelude::MaxResults;
use azure_identity::{DefaultAzureCredential, TokenCredentialOptions};
use azure_storage::{ConnectionString, StorageCredentials};
use azure_storage_blobs::blob::BlobProperties;
use azure_storage_blobs::container::operations::BlobItem;
use azure_storage_blobs::prelude::{BlobServiceClient, ContainerClient};
use dashmap::DashMap;
use futures::StreamExt;
use std::future::Future;
use std::io::Cursor;
use std::num::NonZeroU32;
use std::sync::Arc;
use zip::ZipArchive;

#[derive(Clone, Debug)]
pub struct AzureStorageProvider {
    client: ContainerClient,
    prefix: Prefix,
    global_config: Arc<GlobalAgentConfig>,
}

/// How the provider authenticates, decided from config at startup.
#[derive(Debug, PartialEq)]
enum AzureAuth {
    /// Connection string carrying an AccountKey or SAS — shared-key/SAS credentials.
    ConnectionString(String),
    /// Entra ID (IAM) via `DefaultAzureCredential` for the given account.
    Iam { account_name: String },
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
            let parsed = ConnectionString::new(&raw).context("Invalid connection string")?;
            if parsed.account_key.is_some() || parsed.sas.is_some() {
                return Ok(AzureAuth::ConnectionString(raw));
            }
            let account_name = parsed.account_name.context(
                "Connection string has no AccountKey/SharedAccessSignature and no AccountName",
            )?;
            tracing::warn!(
                "Keyless connection string detected; using IAM/Entra ID auth. Prefer PROVIDER__ACCOUNT_NAME."
            );
            Ok(AzureAuth::Iam {
                account_name: account_name.to_string(),
            })
        }
    }
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
}

impl AzureStorageProvider {
    pub fn new(
        config: &AzureStorageProviderConfig,
        global_config: Arc<GlobalAgentConfig>,
    ) -> anyhow::Result<Self> {
        let (account_name, credentials) = match resolve_auth(config)? {
            AzureAuth::ConnectionString(raw) => {
                let connection_string =
                    ConnectionString::new(&raw).context("Invalid connection string")?;
                let credentials = connection_string
                    .storage_credentials()
                    .context("Invalid storage credentials")?;
                let account_name = connection_string
                    .account_name
                    .context("Invalid account name")?
                    .to_string();
                (account_name, credentials)
            }
            AzureAuth::Iam { account_name } => {
                let credential = DefaultAzureCredential::create(TokenCredentialOptions::default())
                    .context("Invalid credential")?;
                (
                    account_name,
                    StorageCredentials::token_credential(Arc::new(credential)),
                )
            }
        };

        let blob_service = BlobServiceClient::new(account_name, credentials);

        let container_client = blob_service.container_client(&config.container);

        Ok(AzureStorageProvider {
            client: container_client,
            prefix: Prefix::from(config.prefix.clone()),
            global_config,
        })
    }

    async fn generate_projects(&self, keys: Vec<String>) -> DashMap<String, Arc<Project>> {
        let array = futures::stream::iter(keys.into_iter())
            .map(|key| {
                let client = self.client.clone();
                let blob_client = client.blob_client(self.prefix.prepend(key.as_str().into()));

                async move {
                    let mut complete_response = vec![];
                    let mut stream = blob_client.get().chunk_size(0x2000u64).into_stream();
                    let mut content_hash = None;
                    while let Some(maybe_value) = stream.next().await {
                        let value = match maybe_value {
                            Ok(val) => val,
                            Err(e) => {
                                tracing::error!(
                                    "[AZURE - SKIP] Failed to get blob chunk {}: {}",
                                    key,
                                    e
                                );
                                return None;
                            }
                        };
                        if content_hash.is_none() {
                            content_hash = extract_hash(&value.blob.properties);
                        }

                        let data = match value.data.collect().await {
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
                        complete_response.extend(&data);
                    }

                    let cursor = Cursor::new(complete_response);
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
            let mut request_builder = this
                .client
                .list_blobs()
                .delimiter("/")
                .max_results(MaxResults::new(NonZeroU32::new(1_000u32).unwrap()));
            if let Some(prefix) = this.prefix.to_string() {
                request_builder = request_builder.prefix(prefix);
            }

            let mut stream = request_builder.into_stream();

            let mut project_datum: Vec<ProjectData> = Vec::new();
            while let Some(response) = stream.next().await {
                let items = response
                    .context(
                        "Failed to list blobs — if authentication succeeded, ensure the identity \
                         has the 'Storage Blob Data Reader' role on the storage account or container",
                    )?
                    .blobs
                    .items;
                let blobs = items
                    .iter()
                    .filter_map(|blob_item| match blob_item {
                        BlobItem::Blob(blob) => Some(ProjectData {
                            key: this.prefix.strip(blob.name.as_str().into()).into_owned(),
                            content_hash: extract_hash(&blob.properties),
                        }),
                        BlobItem::BlobPrefix(_) => None,
                    })
                    .filter_map(|proj_data| {
                        if FailedProjectsRegistry::has_failed(proj_data.content_hash.as_deref()) {
                            return None;
                        }
                        Some(proj_data)
                    });

                project_datum.extend(blobs);
            }

            let diff = data.calculate_diff(project_datum);

            let to_refresh = Agent::get_refresh_list(&diff);

            let refreshed_projects = this.generate_projects(to_refresh).await;
            let diff = Agent::get_diff_result(data, diff, refreshed_projects);

            Ok(diff)
        }
    }
}

fn extract_hash(properties: &BlobProperties) -> Option<Vec<u8>> {
    // if let Some(crc64) = &properties.content_crc64 {
    //     return Some(crc64.bytes().to_vec());
    // }
    //
    // if let Some(md5) = &properties.content_md5 {
    //     return Some(md5.bytes().to_vec());
    // }

    Some(
        properties
            .etag
            .to_string()
            .trim_matches('"')
            .as_bytes()
            .to_vec(),
    )
}
