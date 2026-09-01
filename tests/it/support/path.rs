use agent::Project;
use std::env::current_dir;
use std::fs;
use std::future::Future;
use walkdir::WalkDir;

pub fn data_path() -> String {
    current_dir()
        .unwrap()
        .join("tests")
        .join("data")
        .to_str()
        .unwrap()
        .to_string()
}

pub fn data_path_files() -> Vec<PathFile> {
    let dp = data_path();
    WalkDir::new(dp.clone())
        .into_iter()
        .filter_map(|m_entry| {
            let Ok(entry) = m_entry else {
                return None;
            };

            if !entry.file_type().is_file() {
                return None;
            }

            Some(PathFile {
                absolute_path: entry.path().to_str().unwrap().to_string(),
                relative_path: entry
                    .path()
                    .strip_prefix(dp.as_str())
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string(),
            })
        })
        .collect()
}

#[derive(Debug)]
pub struct PathFile {
    pub relative_path: String,
    pub absolute_path: String,
}

impl PathFile {
    pub fn read(&self) -> Vec<u8> {
        fs::read(&self.absolute_path).unwrap()
    }
}

pub trait ValidateProject {
    fn validate_project(&self) -> impl Future<Output = ()>;
}

impl ValidateProject for Project {
    fn validate_project(&self) -> impl Future<Output = ()> {
        async {
            for l in decision_paths() {
                let decision = self.engine.get_decision(l).await;
                assert!(decision.is_ok(), "{l} decision was not found");
            }

            ()
        }
    }
}

pub fn decision_paths() -> Vec<&'static str> {
    vec![
        "sample-small",
        "copy of sample-small",
        "first level/nested-sample",
    ]
}
