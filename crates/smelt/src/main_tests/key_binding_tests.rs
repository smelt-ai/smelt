//! 工作区键位解析：context 深度、条件标识，以及「不绑 None」。
//!
//! 不拉窗口。`Keymap::bindings_for_input` 就是分发时用的那套优先级。

use gpui::{Action, KeyBinding, KeyContext, Keymap, Keystroke};

use crate::{
    ActivateSession, ClosePane, CyclePanePrev, DismissWorkspace, FileTreeActivate, FileTreeMoveUp,
    NextHunk, PrevHunk, Quit, SaveFile, SplitBelow, SplitBeside, ToggleCommandPalette,
    app_key_bindings, workspace_key_context,
};

fn keymap() -> Keymap {
    let mut keymap = Keymap::new(app_key_bindings());
    // 用 Quit 站在 Input 上，模拟编辑器更深的 Outdent / Replace / Escape。
    keymap.add_bindings([
        KeyBinding::new("cmd-[", Quit, Some("Input")),
        KeyBinding::new("cmd-shift-f", Quit, Some("Input")),
        KeyBinding::new("escape", Quit, Some("Input")),
    ]);
    keymap
}

fn contexts(stack: &[&str]) -> Vec<KeyContext> {
    stack
        .iter()
        .map(|entry| KeyContext::parse(entry).expect("context"))
        .collect()
}

fn stroke(spec: &str) -> Keystroke {
    Keystroke::parse(spec).expect("keystroke")
}

fn matched(spec: &str, stack: &[&str]) -> Vec<KeyBinding> {
    let (bindings, pending) = keymap().bindings_for_input(&[stroke(spec)], &contexts(stack));
    assert!(!pending, "{spec} 不该停在半截组合键上");
    bindings.into_vec()
}

fn first_is<T: Action>(spec: &str, stack: &[&str]) -> bool {
    matched(spec, stack)
        .first()
        .is_some_and(|binding| binding.action().as_any().is::<T>())
}

#[test]
fn context_string_only_adds_the_panel_that_is_showing() {
    assert_eq!(workspace_key_context(false, false), "Workspace");
    assert_eq!(workspace_key_context(true, false), "Workspace Files");
    assert_eq!(workspace_key_context(false, true), "Workspace GitChanges");
    assert_eq!(
        workspace_key_context(true, true),
        "Workspace Files GitChanges"
    );
}

#[test]
fn workspace_chords_stay_shallower_than_terminal_and_input() {
    assert!(first_is::<ToggleCommandPalette>(
        "cmd-k",
        &["Workspace", "Terminal"]
    ));
    assert!(first_is::<SaveFile>(
        "cmd-s",
        &["Workspace Files", "Terminal"]
    ));
    assert!(
        matched("cmd-s", &["Workspace", "Terminal"]).is_empty(),
        "文件页没打开时 Cmd+S 不该命中"
    );
    assert!(first_is::<NextHunk>(
        "f7",
        &["Workspace GitChanges", "Editor"]
    ));
    assert!(first_is::<PrevHunk>(
        "shift-f7",
        &["Workspace Files GitChanges", "Terminal"]
    ));
    assert!(
        matched("f7", &["Workspace", "Editor"]).is_empty(),
        "改动页没打开时 F7 不该命中"
    );
    assert!(first_is::<FileTreeMoveUp>(
        "up",
        &["Workspace Files", "FileTree"]
    ));
    assert!(first_is::<FileTreeActivate>(
        "enter",
        &["Workspace", "FileTree"]
    ));
    assert!(
        matched("up", &["Workspace Files", "Terminal"]).is_empty(),
        "文件树没聚焦时方向键不该命中"
    );
    assert!(
        matched("enter", &["Workspace", "Terminal"]).is_empty(),
        "Enter 只属于文件树，不能从 Workspace 抢走终端的回车"
    );

    let cycle = matched("cmd-[", &["Workspace", "Input"]);
    assert!(cycle[0].action().as_any().is::<Quit>());
    assert!(
        cycle
            .iter()
            .any(|binding| binding.action().as_any().is::<CyclePanePrev>())
    );

    let hud = matched("cmd-shift-f", &["Workspace", "Input"]);
    assert!(hud[0].action().as_any().is::<Quit>());

    assert!(first_is::<Quit>("escape", &["Workspace", "Input"]));
    assert!(first_is::<DismissWorkspace>(
        "escape",
        &["Workspace", "Terminal"]
    ));
}

#[test]
fn session_and_split_chords_carry_their_actions() {
    let first = &matched("cmd-1", &["Workspace"])[0];
    let ActivateSession { index } = first
        .action()
        .as_any()
        .downcast_ref::<ActivateSession>()
        .expect("cmd-1");
    assert_eq!(*index, 0);

    let last = &matched("cmd-9", &["Workspace Files GitChanges"])[0];
    let ActivateSession { index } = last
        .action()
        .as_any()
        .downcast_ref::<ActivateSession>()
        .expect("cmd-9");
    assert_eq!(*index, 8);

    assert!(first_is::<SplitBeside>("cmd-d", &["Workspace"]));
    assert!(first_is::<SplitBelow>("shift-cmd-d", &["Workspace"]));
    assert!(first_is::<ClosePane>("cmd-w", &["Workspace", "Terminal"]));
}

#[test]
fn migrated_chords_are_not_bound_at_the_root_context() {
    for spec in [
        "cmd-k",
        "cmd-[",
        "cmd-]",
        "cmd-1",
        "cmd-d",
        "shift-cmd-d",
        "cmd-w",
        "cmd-s",
        "cmd-shift-f",
        "f7",
        "shift-f7",
        "escape",
        "up",
        "enter",
    ] {
        let (bindings, _) = keymap().bindings_for_input(&[stroke(spec)], &contexts(&["Terminal"]));
        assert!(
            bindings.is_empty(),
            "{spec} 不该在没有 Workspace / Files / GitChanges / FileTree 时命中"
        );
    }
}
