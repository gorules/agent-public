use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::io::Cursor;
use std::sync::Arc;

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

use crate::config::{GcsProviderConfig, GlobalAgentConfig};
use crate::immutable_loader::{ImmutableLoader, ProtectedZipArchive};
use crate::provider::{AgentData, AgentDataProvider, Project, ProjectData, ProjectDiff};
use crate::util::prefix::Prefix;

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
    pub async fn new(config: &GcsProviderConfig, global_config: Arc<GlobalAgentConfig>) -> Self {
        let credentials_contents = BASE64_STANDARD
            .decode(config.base64_contents.as_str())
            .expect("Invalid base64 contents");

        let credentials_file: CredentialsFile =
            serde_json::from_slice(credentials_contents.as_slice()).expect("Invalid credentials");

        let client_config = ClientConfig::default()
            .with_credentials(credentials_file)
            .await
            .expect("Invalid credentials");

        GcsProvider {
            client: Arc::new(Client::new(client_config)),
            bucket: Arc::new(config.bucket.to_string()),
            prefix: Prefix::from(config.prefix.clone()),
            global_config,
        }
    }

    async fn generate_projects(
        &self,
        keys: Vec<String>,
    ) -> anyhow::Result<DashMap<String, Arc<Project>>> {
        let array = futures::stream::iter(keys.into_iter())
            .map(|key| {
                let client = self.client.clone();
                let bucket = self.bucket.clone();
                let object_key = self.prefix.prepend(key.clone().into()).into_owned();

                async move {
                    let object_request = GetObjectRequest {
                        bucket: bucket.to_string(),
                        object: object_key,
                        ..Default::default()
                    };

                    let object = client
                        .get_object(&object_request)
                        .await
                        .context("failed to get object")?;

                    let data = client
                        .download_object(&object_request, &Default::default())
                        .await
                        .context("failed to get download object")?;

                    let cursor = Cursor::new(data);
                    let archive = ProtectedZipArchive {
                        archive: ZipArchive::new(cursor).unwrap(),
                        password: self.global_config.release_zip_password.clone(),
                    };

                    let engine = ImmutableLoader::try_from(archive).unwrap().into_engine();

                    Ok((
                        key,
                        Arc::new(Project {
                            engine,
                            content_hash: Some(object.etag.into_bytes()),
                        }),
                    ))
                }
            })
            .buffered(100)
            .collect::<Vec<anyhow::Result<(String, Arc<Project>)>>>()
            .await;

        array
            .into_iter()
            .collect::<anyhow::Result<DashMap<String, Arc<Project>>>>()
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
                .collect::<Vec<_>>();

            let diff = data.calculate_diff(project_datum);
            let to_refresh = diff
                .iter()
                .filter_map(|c| match c {
                    ProjectDiff::Created(key) | ProjectDiff::Updated(key) => Some(key.to_string()),
                    ProjectDiff::Removed(_) => None,
                })
                .collect::<Vec<String>>();

            let refreshed_projects = this.generate_projects(to_refresh).await?;
            diff.iter()
                .try_for_each::<_, anyhow::Result<()>>(|change| match change {
                    ProjectDiff::Created(key) | ProjectDiff::Updated(key) => {
                        data.projects.insert(
                            key.to_string(),
                            refreshed_projects
                                .get(key)
                                .context("key should be fetched")?
                                .clone(),
                        );

                        Ok(())
                    }
                    ProjectDiff::Removed(key) => {
                        data.projects.remove(key);
                        Ok(())
                    }
                })?;

            Ok(diff)
        }
    }
}
