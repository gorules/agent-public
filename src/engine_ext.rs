use crate::data::release_data::ReleaseData;
use crate::immutable_loader::ImmutableLoader;
use std::sync::Arc;
use zen_engine::DecisionEngine;

pub trait EngineExtension {
    fn release_data(&self) -> Option<ReleaseData>;
    fn get_version(&self, path: &str) -> Option<Arc<str>>;

    fn can_access(&self, token: &str) -> bool;
}

impl EngineExtension for DecisionEngine {
    fn release_data(&self) -> Option<ReleaseData> {
        self.loader()
            .downcast_arc::<ImmutableLoader>()
            .ok()?
            .release_data()
            .map(|rd| rd.clone())
    }

    fn get_version(&self, path: &str) -> Option<Arc<str>> {
        self.loader()
            .downcast_arc::<ImmutableLoader>()
            .ok()?
            .get_version(path)
    }

    fn can_access(&self, token: &str) -> bool {
        self.loader()
            .downcast_arc::<ImmutableLoader>()
            .ok()
            .map_or(false, |loader| loader.can_access(token))
    }
}
