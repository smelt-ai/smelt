//! Worktree 继承算法由 GUI 与 smeltd 共用；这里只保留 GPUI Global 包装。

use std::ops::{Deref, DerefMut};

pub use smelt_core::worktree_inherit::inherit_if_enabled;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct WorktreeInheritSettings(pub smelt_core::worktree_inherit::WorktreeInheritSettings);

impl gpui::Global for WorktreeInheritSettings {}

impl Deref for WorktreeInheritSettings {
    type Target = smelt_core::worktree_inherit::WorktreeInheritSettings;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for WorktreeInheritSettings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub fn load_settings() -> WorktreeInheritSettings {
    WorktreeInheritSettings(smelt_core::worktree_inherit::load_settings())
}

pub fn save_settings(settings: &WorktreeInheritSettings) {
    smelt_core::worktree_inherit::save_settings(&settings.0);
}
