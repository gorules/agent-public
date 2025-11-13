use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::io::Cursor;
use std::sync::Arc;

use crate::Agent;
use crate::config::{GcsProviderConfig, GlobalAgentConfig};
use crate::immutable_loader::{ImmutableLoader, ProtectedZipArchive};
use crate::provider::{
    AgentData, AgentDataProvider, FailedProjectsRegistry, Project, ProjectData, ProjectDiff,
};
use crate::util::prefix::Prefix;
use anyhow::Context;
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use dashmap::DashMap;
use futures::StreamExt;
use google_cloud_storage::client::google_cloud_auth::credentials::CredentialsFile;
use google_cloud_storage::client::{Client, ClientConfig};
use google_cloud_storage::http::objects::get::GetObjectRequest;
use google_cloud_storage::http::objects::list::ListObjectsRequest;
use zip::ZipArchive;

#[derive(Clone)]
pub struct GcsProvider {
    client: Arc<Client>,
    bucket: Arc<String>,
    global_config: Arc<GlobalAgentConfig>,
    prefix: Prefix,
}

impl Debug for GcsProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "GcsProvider")
    }
}

impl GcsProvider {
    pub async fn new(
        config: &GcsProviderConfig,
        global_config: Arc<GlobalAgentConfig>,
    ) -> anyhow::Result<Self> {
        let client_config = match &config.base64_contents {
            Some(base64_contents) => {
                let credentials_contents = BASE64_STANDARD
                    .decode(base64_contents)
                    .context("Invalid base64 contents")?;

                let credentials_file: CredentialsFile =
                    serde_json::from_slice(credentials_contents.as_slice())
                        .context("Invalid credentials")?;

                ClientConfig::default()
                    .with_credentials(credentials_file)
                    .await
                    .context("Invalid credentials")?
            }
            None => ClientConfig::default()
                .with_auth()
                .await
                .context("Invalid credentials")?,
        };

        Ok(GcsProvider {
            client: Arc::new(Client::new(client_config)),
            bucket: Arc::new(config.bucket.to_string()),
            prefix: Prefix::from(config.prefix.clone()),
            global_config,
        })
    }

    async fn generate_projects(&self, keys: Vec<String>) -> DashMap<String, Arc<Project>> {
        let array = futures::stream::iter(keys.into_iter())
            .map(|key| {
                let client = self.client.clone();
                let bucket = self.bucket.clone();
                let object_key = self.prefix.prepend(key.clone().into()).into_owned();

                async move {
                    let object_request = GetObjectRequest {
                        bucket: bucket.to_string(),
                        object: object_key.clone(),
                        ..Default::default()
                    };

                    let object = match client.get_object(&object_request).await {
                        Ok(object) => object,
                        Err(e) => {
                            tracing::error!(
                                "[GCS - SKIP] Failed to get object {}: {}",
                                object_key,
                                e
                            );
                            return None;
                        }
                    };

                    let data = match client
                        .download_object(&object_request, &Default::default())
                        .await
                    {
                        Ok(downloaded_object) => downloaded_object,
                        Err(e) => {
                            tracing::error!(
                                "[GCS - SKIP] Failed to download object {}: {}",
                                object_key,
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
                                    "[GCS - SKIP] failed unpack zip archive {}: {}",
                                    object_key,
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
                                "[GCS - SKIP] failed load into engine {}: {}",
                                object_key,
                                err
                            );
                            FailedProjectsRegistry::insert(object.etag.into_bytes());
                            return None;
                        }
                    };

                    Some((
                        key,
                        Arc::new(Project {
                            engine,
                            content_hash: Some(object.etag.into_bytes()),
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

impl AgentDataProvider for GcsProvider {
    fn load_data(
        &self,
        data: Arc<AgentData>,
    ) -> impl Future<Output = anyhow::Result<Vec<ProjectDiff>>> + Send + 'static {
        let this = self.clone();

        async move {
            let object_list = this
                .client
                .list_objects(&ListObjectsRequest {
                    bucket: this.bucket.to_string(),
                    max_results: Some(1_000),
                    prefix: this.prefix.to_string(),
                    delimiter: Some(String::from("/")),
                    ..Default::default()
                })
                .await
                .context("failed to list objects")?;

            let objects = object_list.items.unwrap_or_default();
            let project_datum = objects
                .iter()
                .map(|obj| ProjectData {
                    key: this.prefix.strip(obj.name.as_str().into()).into_owned(),
                    content_hash: Some(obj.etag.clone().into_bytes()),
                })
                .filter_map(|proj_data| {
                    if FailedProjectsRegistry::has_failed(proj_data.content_hash.as_deref()) {
                        return None;
                    }
                    if proj_data.key.is_empty() {
                        return None;
                    }
                    Some(proj_data)
                })
                .collect::<Vec<_>>();

            let diff = data.calculate_diff(project_datum);
            let to_refresh = Agent::get_refresh_list(&diff);

            let refreshed_projects = this.generate_projects(to_refresh).await;

            let diff = Agent::get_diff_result(data, diff, refreshed_projects);

            Ok(diff)
        }
    }
}
