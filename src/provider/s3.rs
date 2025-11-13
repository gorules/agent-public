use std::future::Future;
use std::io::Cursor;
use std::sync::Arc;

use crate::Agent;
use crate::config::{GlobalAgentConfig, S3ProviderConfig};
use crate::immutable_loader::{ImmutableLoader, ProtectedZipArchive};
use crate::provider::{
    AgentData, AgentDataProvider, FailedProjectsRegistry, Project, ProjectData, ProjectDiff,
};
use crate::util::prefix::Prefix;
use aws_config::default_provider::credentials::DefaultCredentialsChain;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_s3::Client;
use dashmap::DashMap;
use futures::StreamExt;
use zip::ZipArchive;

#[derive(Clone, Debug)]
pub struct S3Provider {
    client: Client,
    bucket: Arc<String>,
    prefix: Prefix,
    global_config: Arc<GlobalAgentConfig>,
}

impl S3Provider {
    pub async fn new(config: &S3ProviderConfig, global_config: Arc<GlobalAgentConfig>) -> Self {
        let region_provider = RegionProviderChain::default_provider().or_else("us-east-1");
        let region = region_provider.region().await;

        let credentials = DefaultCredentialsChain::builder()
            .region(region_provider)
            .build()
            .await;

        let mut config_builder = aws_sdk_s3::config::Builder::new()
            .behavior_version_latest()
            .force_path_style(config.force_path_style)
            .region(region)
            .credentials_provider(credentials);
        if let Some(endpoint) = &config.endpoint {
            config_builder = config_builder.endpoint_url(endpoint.clone());
        }

        let client = Client::from_conf(config_builder.build());

        S3Provider {
            client,
            global_config,
            bucket: Arc::new(config.bucket.clone()),
            prefix: Prefix::from(config.prefix.clone()),
        }
    }

    async fn generate_projects(&self, keys: Vec<String>) -> DashMap<String, Arc<Project>> {
        let array = futures::stream::iter(keys.into_iter())
            .map(|key| {
                let client = self.client.clone();
                let bucket = self.bucket.clone();

                async move {
                    let object = match client
                        .clone()
                        .get_object()
                        .bucket(bucket.as_str())
                        .key(self.prefix.prepend(key.as_str().into()))
                        .send()
                        .await
                    {
                        Ok(object) => object,
                        Err(e) => {
                            tracing::error!("[S3 - SKIP] Failed to get object {}: {}", key, e);
                            return None;
                        }
                    };

                    let bdy = match object.body.collect().await {
                        Ok(bdy) => bdy,
                        Err(e) => {
                            tracing::error!("[S3 - SKIP] Failed to get object body {}: {}", key, e);
                            return None;
                        }
                    };

                    let cursor = Cursor::new(bdy.to_vec());
                    let archive = ProtectedZipArchive {
                        archive: match ZipArchive::new(cursor) {
                            Ok(archive) => archive,
                            Err(err) => {
                                tracing::error!(
                                    "[S3 - SKIP] failed unpack zip archive {}: {}",
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
                            tracing::error!("[S3 - SKIP] failed load into engine {}: {}", key, err);
                            match object.e_tag.map(|t| t.into_bytes()) {
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
                            content_hash: object.e_tag.map(|t| t.into_bytes()),
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

impl AgentDataProvider for S3Provider {
    fn load_data(
        &self,
        data: Arc<AgentData>,
    ) -> impl Future<Output = anyhow::Result<Vec<ProjectDiff>>> + Send + 'static {
        let this = self.clone();

        async move {
            let mut request_builder = this
                .client
                .list_objects_v2()
                .delimiter("/")
                .max_keys(1_000)
                .bucket(this.bucket.as_str());
            if let Some(prefix) = this.prefix.as_str() {
                request_builder = request_builder.prefix(prefix)
            }

            let response = request_builder.send().await?;

            let objects = response.contents.unwrap_or_default();
            let project_datum = objects
                .into_iter()
                .filter_map(|obj| {
                    let key = this.prefix.strip(obj.key?.into()).into_owned();
                    if key.is_empty() {
                        return None;
                    }

                    let content_hash = obj.e_tag.map(|t| t.into_bytes());
                    if FailedProjectsRegistry::has_failed(content_hash.as_deref()) {
                        return None;
                    }
                    Some(ProjectData { key, content_hash })
                })
                .collect();

            let diff = data.calculate_diff(project_datum);

            let to_refresh = Agent::get_refresh_list(&diff);

            let refreshed_projects = this.generate_projects(to_refresh).await;

            let diff = Agent::get_diff_result(data, diff, refreshed_projects);

            Ok(diff)
        }
    }
}
