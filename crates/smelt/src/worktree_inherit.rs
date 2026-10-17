//! Worktree 继承由运行时在建 worktree 时执行；这里只保留设置页用的 GPUI Global 包装。

use std::ops::{Deref, DerefMut};

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
