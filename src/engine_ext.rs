use crate::data::release_data::ReleaseData;
use crate::immutable_loader::ImmutableLoader;
use std::sync::Arc;
use zen_engine::DecisionEngine;
use zen_engine::handler::custom_node_adapter::NoopCustomNode;

pub trait EngineExtension {
    fn release_data(&self) -> Option<&ReleaseData>;
    fn get_version(&self, path: &str) -> Option<Arc<str>>;

    fn can_access(&self, token: &str) -> bool;
}

impl EngineExtension for DecisionEngine<ImmutableLoader, NoopCustomNode> {
    fn release_data(&self) -> Option<&ReleaseData> {
        self.loader().release_data()
    }

    fn get_version(&self, path: &str) -> Option<Arc<str>> {
        self.loader().get_version(path)
    }

    fn can_access(&self, token: &str) -> bool {
        self.loader().can_access(token)
    }
}
