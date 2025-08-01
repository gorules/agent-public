use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::{env, fs};

use crate::config::{FilesystemProviderConfig, GlobalAgentConfig};
use crate::data::extended_decision::ExtendedDecisionContent;
use crate::data::release_data::ReleaseData;
use crate::immutable_loader::ImmutableLoader;
use crate::provider::{AgentData, AgentDataProvider, Project, ProjectDiff};
use anyhow::Context;
use dashmap::DashMap;
use itertools::Itertools;
use tokio::task;
use walkdir::WalkDir;

#[derive(Debug)]
pub struct FilesystemProvider {
    root_dir: PathBuf,
}

impl FilesystemProvider {
    pub fn new(config: &FilesystemProviderConfig, _: Arc<GlobalAgentConfig>) -> Self {
        let root = env::current_dir()
            .expect("Current directory is available")
            .join(config.root_dir.as_str())
            .to_path_buf();

        Self { root_dir: root }
    }
}

impl AgentDataProvider for FilesystemProvider {
    fn load_data(
        &self,
        data: Arc<AgentData>,
    ) -> impl Future<Output = anyhow::Result<Vec<ProjectDiff>>> + Send + 'static {
        let root = self.root_dir.clone();

        async move {
            let projects = task::spawn_blocking(move || {
                let directory = fs::read_dir(root.clone()).context("failed to read directory")?;
                let paths = directory
                    .into_iter()
                    .filter_map(|d| {
                        let Ok(entry) = d else {
                            return None;
                        };

                        let Ok(meta) = entry.metadata() else {
                            return None;
                        };

                        meta.is_dir().then_some(entry.path().to_path_buf())
                    })
                    .collect::<Vec<PathBuf>>();

                paths
                    .into_iter()
                    .map(|directory| {
                        let relative_path = directory
                            .strip_prefix(root.clone())
                            .context("failed to extract prefix from project")?;

                        Ok((
                            relative_path.to_string_lossy().to_string(),
                            Arc::new(load_from_directory(directory)?),
                        ))
                    })
                    .collect::<anyhow::Result<DashMap<_, _>>>()
            })
            .await??;

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

fn load_from_directory(root: PathBuf) -> anyhow::Result<Project> {
    let files = WalkDir::new(root.clone())
        .into_iter()
        .filter_ok(|d| d.file_type().is_file())
        .collect::<Result<Vec<_>, _>>()
        .context("failed to load files")?;

    let project_json_path = Some(root.join(".config").join("project.json"));
    let release_data = project_json_path
        .map(|entry| {
            let file_reader = File::open(entry).ok()?;
            let content: ReleaseData = serde_json::from_reader(file_reader).ok()?;

            Some(content)
        })
        .flatten();

    let projects = files
        .iter()
        .filter(|entry| {
            let Ok(relative_path) = entry.path().strip_prefix(&root) else {
                return false;
            };

            !relative_path.starts_with(".config")
        })
        .map(|entry| {
            let file_reader = File::open(entry.path()).context("failed to open file")?;
            let content: ExtendedDecisionContent = serde_json::from_reader(file_reader)?;

            let relative_path = entry
                .path()
                .strip_prefix(&root)
                .context("failed to extract relative path")?;

            Ok((relative_path.to_string_lossy().to_string(), content))
        })
        .collect::<anyhow::Result<HashMap<String, ExtendedDecisionContent>>>();

    Ok(Project {
        engine: ImmutableLoader::new(projects?, release_data).into_engine(),
        content_hash: None,
    })
}
