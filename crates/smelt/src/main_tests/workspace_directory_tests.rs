//! 工作区目录镜像：收到的和手上的一样、或不比手上的新，不叫醒观察者（插件据此整份重算会话
//! 列表）。

use std::cell::Cell;
use std::rc::Rc;

use gpui::{App, TestAppContext};
use smelt_client::runtime::RuntimeKey;
use smelt_model::workspace_directory::WorkspaceDirectory;

use crate::WorkspaceDirectoriesGlobal;

fn directory(revision: u64) -> WorkspaceDirectory {
    WorkspaceDirectory {
        epoch: "e1".into(),
        revision,
        ..Default::default()
    }
}

#[gpui::test]
fn only_newer_directories_wake_observers(cx: &mut TestAppContext) {
    let signals = Rc::new(Cell::new(0));
    cx.update(|cx| {
        cx.set_global(WorkspaceDirectoriesGlobal::default());
        let signals = signals.clone();
        cx.observe_global::<WorkspaceDirectoriesGlobal>(move |_| signals.set(signals.get() + 1))
            .detach();
    });
    let local = RuntimeKey::Local;
    let receive = |directory: WorkspaceDirectory, snapshot: bool, cx: &mut App| {
        WorkspaceDirectoriesGlobal::receive(&local, directory, snapshot, cx)
    };
    cx.update(|cx| receive(directory(2), true, cx));
    assert_eq!(signals.get(), 1);
    cx.update(|cx| receive(directory(2), true, cx));
    cx.update(|cx| receive(directory(1), false, cx));
    cx.update(|cx| receive(directory(2), false, cx));
    assert_eq!(signals.get(), 1, "重连的一样快照、旧的增量都不收");
    cx.update(|cx| receive(directory(3), false, cx));
    assert_eq!(signals.get(), 2);
    cx.update(|cx| receive(directory(1), true, cx));
    assert_eq!(signals.get(), 3, "快照不一样就整份换（守护可能回退过）");
}
