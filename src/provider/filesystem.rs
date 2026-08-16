use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::{env, fs};

use crate::config::{FilesystemProviderConfig, GlobalAgentConfig};
use crate::data::extended_decision::{FileContent, FileDecisionGraph, FileTestContent};
use crate::data::release_data::ReleaseData;
use crate::immutable_loader::{ImmutableLoader, collect_examples};
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
                let directory = match fs::read_dir(root.clone()) {
                    Ok(dir) => dir,
                    Err(error) => {
                        println!("[FS - Skip] Failed to read directory: {}", error);
                        return DashMap::new();
                    }
                };

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
                    .filter_map(|directory| {
                        let relative_path = match directory.strip_prefix(root.clone()) {
                            Ok(ok) => ok,
                            Err(err) => {
                                tracing::error!(
                                    "[FS - Skip] failed to strip prefix on {}: {}",
                                    directory.display(),
                                    err
                                );
                                return None;
                            }
                        };

                        let project = match load_from_directory(&directory) {
                            Ok(ok) => ok,
                            Err(err) => {
                                tracing::error!(
                                    "[FS - Skip] failed to load project from directory {}: {}",
                                    directory.display(),
                                    err
                                );
                                return None;
                            }
                        };

                        Some((
                            relative_path.to_string_lossy().to_string(),
                            Arc::new(project),
                        ))
                    })
                    .collect::<DashMap<_, _>>()
            })
            .await?;

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

fn load_from_directory(root: &PathBuf) -> anyhow::Result<Project> {
    let files = WalkDir::new(root.clone())
        .into_iter()
        .filter_ok(|d| d.file_type().is_file())
        .collect::<Result<Vec<_>, _>>()
        .context("failed to load files")?;

    let project_json_path = Some(root.join(".config").join("project.json"));
    let release_data = project_json_path
        .map(|entry| {
            let file_reader = File::open(entry).ok()?;
            ReleaseData::from_json_reader(file_reader)
        })
        .flatten();

    let mut graphs: HashMap<String, FileDecisionGraph> = HashMap::new();
    let mut test_files: Vec<(String, FileTestContent)> = Vec::new();

    for entry in files.iter() {
        let Ok(relative_path) = entry.path().strip_prefix(&root) else {
            continue;
        };
        if relative_path.starts_with(".config") {
            continue;
        }

        let path = relative_path.to_string_lossy().to_string();
        let file_reader = File::open(entry.path()).context("failed to open file")?;
        let content: FileContent = serde_json::from_reader(file_reader)
            .with_context(|| format!("failed to parse decision content for file {path}"))?;

        match content {
            FileContent::Graph(mut graph) => {
                // Keep the original-cased path for display, but key the map by the
                // lowercased path — `ImmutableLoader::load` lowercases lookup keys,
                // so a mixed-case key here would be unreachable at evaluation time.
                graph.meta.display_path = Some(Arc::from(path.as_str()));
                graphs.insert(path.to_lowercase(), graph);
            }
            FileContent::Test(test) => test_files.push((path, test)),
            FileContent::Unknown => {}
        }
    }

    let examples = collect_examples(test_files);

    Ok(Project {
        engine: ImmutableLoader::new(graphs, examples, release_data).into_engine(),
        content_hash: None,
        rules_spec: OnceLock::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_ext::EngineExtension;

    /// Mixed-case file names must be reachable at evaluation time (which
    /// lowercases lookup keys via `ImmutableLoader::load`), while the
    /// original casing is preserved for display in the rules OpenAPI spec.
    #[test]
    fn load_from_directory_lowercases_keys_but_keeps_display_path() {
        let dir = env::temp_dir().join(format!(
            "gorules-agent-fs-test-{}-{}",
            std::process::id(),
            "load_from_directory_lowercases_keys_but_keeps_display_path"
        ));
        fs::create_dir_all(&dir).expect("failed to create temp test dir");

        let graph_json = r#"{
            "contentType": "graph",
            "nodes": [
                { "id": "in", "name": "request", "type": "inputNode" },
                { "id": "out", "name": "response", "type": "outputNode" }
            ],
            "edges": [{ "id": "e1", "sourceId": "in", "targetId": "out" }]
        }"#;
        fs::write(dir.join("Mixed Case Rule"), graph_json).expect("failed to write graph fixture");

        let result = load_from_directory(&dir);

        // Clean up before asserting so the temp dir is never left behind on failure.
        let _ = fs::remove_dir_all(&dir);

        let project = result.expect("failed to load project from directory");

        let keys = project.engine.decision_keys();
        assert!(
            keys.contains(&"mixed case rule".to_string()),
            "expected lowercased key in decision_keys(), got {keys:?}"
        );

        let entries = project.engine.spec_entries();
        let entry = entries
            .iter()
            .find(|e| e.path.as_ref() == "Mixed Case Rule")
            .expect("expected an entry with the original-cased display path");
        assert_eq!(entry.path.as_ref(), "Mixed Case Rule");
    }
}
