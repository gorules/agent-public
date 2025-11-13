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

impl AzureStorageProvider {
    pub fn new(
        config: &AzureStorageProviderConfig,
        global_config: Arc<GlobalAgentConfig>,
    ) -> anyhow::Result<Self> {
        let connection_string = ConnectionString::new(&config.connection_string)
            .context("Invalid connection string")?;

        let credentials = match connection_string.account_key {
            Some(_) => connection_string
                .storage_credentials()
                .context("Invalid storage credentials")?,
            None => {
                let credential = DefaultAzureCredential::create(TokenCredentialOptions::default())
                    .context("Invalid credential")?;

                StorageCredentials::token_credential(Arc::new(credential))
            }
        };

        let blob_service = BlobServiceClient::new(
            connection_string
                .account_name
                .context("Invalid account name")?,
            credentials,
        );

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
                let items = response?.blobs.items;
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
