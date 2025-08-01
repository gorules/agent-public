use std::env;
use std::fs::File;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{GlobalAgentConfig, ZipProviderConfig};
use crate::immutable_loader::{ImmutableLoader, ProtectedZipArchive};
use crate::provider::{AgentData, AgentDataProvider, Project, ProjectDiff};
use anyhow::Context;
use dashmap::DashMap;
use itertools::Itertools;
use tokio::task;
use walkdir::WalkDir;
use zip::ZipArchive;

#[derive(Debug)]
pub struct ZipProvider {
    root_dir: PathBuf,
    global_config: Arc<GlobalAgentConfig>,
}

impl ZipProvider {
    pub fn new(config: &ZipProviderConfig, global_config: Arc<GlobalAgentConfig>) -> Self {
        let root = env::current_dir()
            .expect("Current directory is available")
            .join(config.root_dir.as_str())
            .to_path_buf();

        Self {
            root_dir: root,
            global_config,
        }
    }
}

impl AgentDataProvider for ZipProvider {
    fn load_data(
        &self,
        data: Arc<AgentData>,
    ) -> impl Future<Output = anyhow::Result<Vec<ProjectDiff>>> + Send + 'static {
        let root = self.root_dir.clone();
        let password = self.global_config.release_zip_password.clone();

        async move {
            let projects = task::spawn_blocking(|| load_from_directory(root, password)).await??;
            let diff = projects
                .iter()
                .map(|project| ProjectDiff::Created(project.key().to_string()))
                .collect();

            projects.into_iter().for_each(|(key, project)| {
                let _ = data.projects.insert(key, project);
            });

            Ok(diff)
        }
    }
}

fn load_from_directory(
    root: PathBuf,
    password: Option<Arc<str>>,
) -> anyhow::Result<DashMap<String, Arc<Project>>> {
    let files = WalkDir::new(root.clone())
        .max_depth(1)
        .into_iter()
        .filter_ok(|d| d.file_type().is_file())
        .collect::<Result<Vec<_>, _>>()
        .context("failed to load files")?;

    let projects = files
        .iter()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|s| s.ends_with(".zip"))
        })
        .map(|entry| {
            let file_reader = File::open(entry.path()).context("failed to open file")?;
            let path = entry
                .path()
                .strip_prefix(&root)
                .context("failed to strip file prefix")?
                .to_string_lossy()
                .trim_end_matches(".zip")
                .to_string();

            let archive = ProtectedZipArchive {
                archive: ZipArchive::new(file_reader).context("failed to load zip archive")?,
                password: password.clone(),
            };

            Ok((
                path,
                Arc::new(Project {
                    engine: ImmutableLoader::try_from(archive)?.into_engine(),
                    content_hash: None,
                }),
            ))
        })
        .collect::<anyhow::Result<DashMap<_, _>>>();

    projects
}
