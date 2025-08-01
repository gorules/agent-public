use std::collections::HashMap;
use std::ffi::OsStr;
use std::future::Future;
use std::io::{Read, Seek};
use std::ops::{Deref, DerefMut};
use std::os::unix::ffi::OsStrExt;
use std::path::Component;
use std::sync::Arc;

use crate::data::extended_decision::ExtendedDecisionContent;
use crate::data::release_data::ReleaseData;
use anyhow::anyhow;
use zen_engine::DecisionEngine;
use zen_engine::handler::custom_node_adapter::NoopCustomNode;
use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zip::ZipArchive;
use zip::read::ZipFile;
use zip::result::ZipResult;

#[derive(Default, Debug)]
pub struct ImmutableLoader {
    release_data: Option<ReleaseData>,
    content: HashMap<String, ExtendedDecisionContent>,
}

impl ImmutableLoader {
    pub fn new(
        content: HashMap<String, ExtendedDecisionContent>,
        release_data: Option<ReleaseData>,
    ) -> Self {
        Self {
            content,
            release_data,
        }
    }

    pub fn into_engine(self) -> DecisionEngine<ImmutableLoader, NoopCustomNode> {
        DecisionEngine::default().with_loader(Arc::new(self))
    }

    pub fn release_data(&self) -> Option<&ReleaseData> {
        self.release_data.as_ref()
    }

    pub fn get_version(&self, path: &str) -> Option<Arc<str>> {
        self.content.get(path)?.meta.version_id.clone()
    }

    pub fn can_access(&self, token: &str) -> bool {
        self.release_data()
            .map(|rd| rd.access_tokens.iter().any(|at| at.deref().eq(token)))
            .unwrap_or(true)
    }
}

impl DecisionLoader for ImmutableLoader {
    fn load<'a>(&'a self, key: &'a str) -> impl Future<Output = LoaderResponse> + 'a {
        async move {
            let lower_key = key.to_lowercase();
            let Some(data) = self.content.get(lower_key.as_str()) else {
                return Err(Box::new(LoaderError::NotFound(lower_key)));
            };

            Ok(data.content.clone())
        }
    }
}

pub struct ProtectedZipArchive<R> {
    pub password: Option<Arc<str>>,
    pub archive: ZipArchive<R>,
}

impl<R> Deref for ProtectedZipArchive<R> {
    type Target = ZipArchive<R>;

    fn deref(&self) -> &Self::Target {
        &self.archive
    }
}

impl<R> DerefMut for ProtectedZipArchive<R> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.archive
    }
}

impl<R> ProtectedZipArchive<R>
where
    R: Read + Seek,
{
    pub fn by_index_try_decrypt(&mut self, file_number: usize) -> ZipResult<ZipFile<'_, R>> {
        let Some(password) = self.password.clone() else {
            return self.by_index(file_number);
        };

        let is_ok = self
            .by_index_decrypt(file_number, password.as_bytes())
            .is_ok();
        if is_ok {
            self.by_index_decrypt(file_number, password.as_bytes())
        } else {
            self.by_index(file_number)
        }
    }
}

// Sync
impl<R> TryFrom<ProtectedZipArchive<R>> for ImmutableLoader
where
    R: Read + Seek,
{
    type Error = anyhow::Error;

    fn try_from(mut archive: ProtectedZipArchive<R>) -> Result<Self, Self::Error> {
        let config_prefix = ".config";

        let release_data = archive
            .index_for_name(".config/project.json")
            .map(|index| archive.by_index_try_decrypt(index).ok())
            .flatten()
            .map(|f| {
                if !f.is_file() {
                    return None;
                }

                serde_json::from_reader::<_, ReleaseData>(f).ok()
            })
            .flatten();

        let contents = (0..archive.len())
            .filter_map(move |i| {
                let Ok(file_reader) = archive.by_index_try_decrypt(i) else {
                    return Some(Err(anyhow!("failed to load file on index {i}")));
                };

                if !file_reader.is_file() {
                    return None;
                }

                let Some(enclosed_name) = file_reader.enclosed_name() else {
                    return None;
                };

                if enclosed_name.components().nth(0)
                    == Some(Component::Normal(OsStr::from_bytes(
                        config_prefix.as_bytes(),
                    )))
                {
                    return None;
                }

                let name = file_reader.name().to_lowercase();
                let Ok(content) =
                    serde_json::from_reader::<_, ExtendedDecisionContent>(file_reader)
                else {
                    return Some(Err(anyhow!(
                        "failed to parse decision content for file {name}",
                    )));
                };

                Some(Ok((name, content)))
            })
            .collect::<Result<HashMap<_, _>, _>>();

        Ok(Self::new(contents?, release_data))
    }
}
