use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, SecondsFormat, Utc};
use dashmap::{DashMap, DashSet};
use std::sync::OnceLock;
use strum_macros::AsRefStr;
use tokio::time::Instant;
use tokio::{task, time};
use zen_engine::DecisionEngine;

use crate::config::{EnvironmentConfig, GlobalAgentConfig, ProviderConfig};
use crate::engine_ext::EngineExtension;
use crate::provider::azure_storage::AzureStorageProvider;
use crate::provider::filesystem::FilesystemProvider;
use crate::provider::gcs::GcsProvider;
use crate::provider::s3::S3Provider;
use crate::provider::zip::ZipProvider;

mod azure_storage;
mod filesystem;
mod gcs;
mod s3;
mod zip;

#[derive(Debug, AsRefStr)]
enum AgentProvider {
    Zip(ZipProvider),
    Filesystem(FilesystemProvider),
    S3(S3Provider),
    AzureStorage(AzureStorageProvider),
    GCS(GcsProvider),
}

impl AgentProvider {
    async fn load_data(&self, data: Arc<AgentData>) -> anyhow::Result<Vec<ProjectDiff>> {
        match self {
            AgentProvider::Zip(zip) => zip.load_data(data).await,
            AgentProvider::Filesystem(fs) => fs.load_data(data).await,
            AgentProvider::S3(s3) => s3.load_data(data).await,
            AgentProvider::AzureStorage(storage) => storage.load_data(data).await,
            AgentProvider::GCS(gcs) => gcs.load_data(data).await,
        }
    }

    fn should_refresh(&self) -> bool {
        match self {
            AgentProvider::Zip(_) => false,
            AgentProvider::Filesystem(_) => false,
            AgentProvider::S3(_) => true,
            AgentProvider::AzureStorage(_) => true,
            AgentProvider::GCS(_) => true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Agent {
    data: Arc<AgentData>,
    provider: Arc<AgentProvider>,
    config: Arc<EnvironmentConfig>,
}

impl Agent {
    #[tracing::instrument(
        skip_all,
        name = "agent.create",
        fields(
            provider.kind = config.provider.as_ref(),
            provider.password_protected = global_config.release_zip_password.is_some()
        )
    )]
    pub async fn new(
        config: EnvironmentConfig,
        global_config: Arc<GlobalAgentConfig>,
    ) -> anyhow::Result<Self> {
        tracing::info!("Creating agent provider");
        let provider = match &config.provider {
            ProviderConfig::Zip(config) => {
                AgentProvider::Zip(ZipProvider::new(config, global_config))
            }
            ProviderConfig::Filesystem(config) => {
                AgentProvider::Filesystem(FilesystemProvider::new(config, global_config))
            }
            ProviderConfig::S3(config) => {
                AgentProvider::S3(S3Provider::new(config, global_config).await)
            }
            ProviderConfig::AzureStorage(config) => {
                AgentProvider::AzureStorage(AzureStorageProvider::new(config, global_config)?)
            }
            ProviderConfig::GCS(config) => {
                AgentProvider::GCS(GcsProvider::new(config, global_config).await?)
            }
        };

        tracing::info!("Created agent provider");
        let agent = Self {
            data: Arc::new(Default::default()),
            provider: Arc::new(provider),
            config: Arc::new(config),
        };

        tracing::info!("Loading agent initial data");
        let start = Instant::now();
        agent.refresh_data().await?;

        tracing::info!(duration = ?start.elapsed(), "Loaded agent initial data");

        agent.register_refresh_data();

        Ok(agent)
    }

    pub fn get_refresh_list(diff: &Vec<ProjectDiff>) -> Vec<String> {
        diff.iter()
            .filter_map(|c| match c {
                ProjectDiff::Created(key) | ProjectDiff::Updated(key) => Some(key.to_string()),
                ProjectDiff::Removed(_) => None,
            })
            .collect::<Vec<String>>()
    }

    pub fn get_diff_result(
        data: Arc<AgentData>,
        diff: Vec<ProjectDiff>,
        refreshed_projects: DashMap<String, Arc<Project>>,
    ) -> Vec<ProjectDiff> {
        let diff = diff
            .into_iter()
            .filter_map(|change| match &change {
                ProjectDiff::Created(key) | ProjectDiff::Updated(key) => {
                    match refreshed_projects.get(key) {
                        Some(project) => {
                            data.projects.insert(key.to_string(), project.clone());
                            Some(change)
                        }
                        None => {
                            data.projects.remove(key);
                            None
                        }
                    }
                }
                ProjectDiff::Removed(key) => {
                    data.projects.remove(key);
                    Some(change)
                }
            })
            .collect::<Vec<_>>();
        diff
    }

    pub fn project(&self, project: &str) -> Option<Arc<Project>> {
        if let Some(p) = self.data.projects.get(project) {
            return Some(p.clone());
        };

        self.data.projects.iter().find_map(|p| {
            let Some(rd) = p.engine.release_data() else {
                return None;
            };

            (rd.project.id.deref() == project).then_some(p.to_owned())
        })
    }

    #[tracing::instrument(
        skip_all,
        name = "agent.refresh_data",
        level = "debug",
        fields(
            provider.kind = self.provider.as_ref().as_ref(),
            provider.password_protected = self.config.release_zip_password.is_some()
        )
    )]
    async fn refresh_data(&self) -> anyhow::Result<Vec<ProjectDiff>> {
        tracing::debug!("Refreshing agent data");
        let diff = self.provider.load_data(self.data.clone()).await;
        if diff.as_ref().is_ok_and(|d| d.is_empty()) {
            tracing::debug!("No changes found during agent data refresh");
            return Ok(Default::default());
        }

        match &diff {
            Ok(data) => data.iter().for_each(|diff| match diff {
                ProjectDiff::Created(project) => {
                    tracing::info!("Project created '{project}'.")
                }
                ProjectDiff::Removed(project) => {
                    tracing::info!("Project removed '{project}'.")
                }
                ProjectDiff::Updated(project) => {
                    tracing::info!("Project updated '{project}'.")
                }
            }),
            Err(error) => {
                tracing::error!("Failed to refresh the agent data. Error: {error:?}.");
            }
        }

        diff
    }

    pub fn register_refresh_data(&self) {
        if !self.provider.should_refresh() {
            return;
        }

        let this = self.clone();
        task::spawn(async move {
            let duration = this.config.poll_interval.clone();
            let (system_time, instant) = rounded_instant(duration);
            let mut interval = time::interval_at(instant, duration);

            tracing::info!(
                job.started = format_system_time(system_time + duration),
                job.interval = ?duration,
                "Registered agent data refresh job"
            );

            interval.tick().await;

            loop {
                interval.tick().await;
                let _ = this.refresh_data().await;
            }
        });
    }
}

type AgentDecisionEngine = DecisionEngine;

#[derive(Debug)]
pub struct Project {
    pub engine: AgentDecisionEngine,
    pub content_hash: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct AgentData {
    pub projects: Arc<DashMap<String, Arc<Project>>>,
}

impl AgentData {
    pub fn calculate_diff(&self, data: Vec<ProjectData>) -> Vec<ProjectDiff> {
        let removal = self
            .projects
            .iter()
            .filter_map(|e| {
                let not_exists = data.iter().find(|o| &o.key == e.key()).is_none();

                not_exists.then_some(ProjectDiff::Removed(e.key().to_string()))
            })
            .collect::<Vec<ProjectDiff>>();

        let updates = data
            .into_iter()
            .filter_map(|obj| {
                let Some(current_value) = self.projects.get(&obj.key) else {
                    return Some(ProjectDiff::Created(obj.key));
                };

                let hash_different = current_value.content_hash != obj.content_hash;
                hash_different.then_some(ProjectDiff::Updated(obj.key))
            })
            .collect::<Vec<ProjectDiff>>();

        removal
            .into_iter()
            .chain(updates.into_iter())
            .collect::<Vec<ProjectDiff>>()
    }
}

pub trait AgentDataProvider {
    fn load_data(
        &self,
        data: Arc<AgentData>,
    ) -> impl Future<Output = anyhow::Result<Vec<ProjectDiff>>> + Send + 'static;
}

#[derive(Debug)]
pub enum ProjectDiff {
    Created(String),
    Removed(String),
    Updated(String),
}

#[derive(Debug)]
pub struct ProjectData {
    pub key: String,
    pub content_hash: Option<Vec<u8>>,
}

static FAILED_PROJECTS: OnceLock<DashSet<Vec<u8>>> = OnceLock::new();

pub struct FailedProjectsRegistry;

impl FailedProjectsRegistry {
    fn instance() -> &'static DashSet<Vec<u8>> {
        FAILED_PROJECTS.get_or_init(|| DashSet::new())
    }

    pub fn insert(project_hash: Vec<u8>) {
        Self::instance().insert(project_hash);
    }

    pub fn has_failed(project_hash: Option<&[u8]>) -> bool {
        project_hash.map_or(false, |hash| Self::instance().contains(hash))
    }
}

fn rounded_instant(target_duration: Duration) -> (SystemTime, Instant) {
    let now_system = SystemTime::now();
    let duration_since_epoch = now_system
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");

    let millis_since_epoch = duration_since_epoch.as_millis();
    let tdm = target_duration.as_millis();

    let rounded_millis = (millis_since_epoch / tdm + (millis_since_epoch % tdm > 0) as u128) * tdm;
    let rounded_system_time = UNIX_EPOCH + Duration::from_millis(rounded_millis as u64);

    let duration_diff = rounded_system_time
        .duration_since(now_system)
        .unwrap_or_else(|_| Duration::from_secs(0));

    (rounded_system_time, Instant::now() + duration_diff)
}

fn format_system_time(time: SystemTime) -> String {
    let datetime: DateTime<Utc> = time.into();
    datetime.to_rfc3339_opts(SecondsFormat::Micros, true)
}
