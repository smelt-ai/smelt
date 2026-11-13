//! 工作区键位解析：context 深度、条件标识，以及「不绑 None」。
//!
//! 不拉窗口。`Keymap::bindings_for_input` 就是分发时用的那套优先级。

use gpui::{Action, KeyBinding, KeyContext, Keymap, Keystroke, TestAppContext};
use smelt_plugin_api::{self as api, RunCommand};

use crate::keymap::{Overrides, app_key_bindings, bind_commands};
use crate::{CloseSettingsWindow, DismissWorkspace, SidebarOpen, workspace_commands};

fn keymap() -> Keymap {
    let bindings = TestAppContext::single().update(|cx| {
        workspace_commands::register_inert(cx);
        let bound = bind_commands(api::commands(cx), &Overrides::new(), cx.platform_info().os);
        app_key_bindings(&bound, cx)
    });
    let mut keymap = Keymap::new(bindings);
    // 用 SidebarOpen 站在 Input 上，模拟编辑器更深的 Outdent / Replace / Escape。
    keymap.add_bindings([
        KeyBinding::new("cmd-[", SidebarOpen, Some("Input")),
        KeyBinding::new("cmd-shift-f", SidebarOpen, Some("Input")),
        KeyBinding::new("escape", SidebarOpen, Some("Input")),
    ]);
    keymap
}

fn contexts(stack: &[&str]) -> Vec<KeyContext> {
    stack
        .iter()
        .map(|entry| KeyContext::parse(entry).expect("context"))
        .collect()
}

fn stroke(spec: &'static str) -> Keystroke {
    Keystroke::parse(spec).expect("keystroke")
}

fn matched(spec: &'static str, stack: &[&str]) -> Vec<KeyBinding> {
    let (bindings, pending) = keymap().bindings_for_input(&[stroke(spec)], &contexts(stack));
    assert!(!pending, "{spec} 不该停在半截组合键上");
    bindings.into_vec()
}

fn command_of(binding: &KeyBinding) -> Option<String> {
    binding
        .action()
        .as_any()
        .downcast_ref::<RunCommand>()
        .map(|run| run.command.to_string())
}

fn first_is<T: Action>(spec: &'static str, stack: &[&str]) -> bool {
    matched(spec, stack)
        .first()
        .is_some_and(|binding| binding.action().as_any().is::<T>())
}

/// 命中的第一条绑的是哪条命令。
fn first_command(spec: &'static str, stack: &[&str]) -> Option<String> {
    matched(spec, stack).first().and_then(command_of)
}

#[test]
fn workspace_chords_stay_shallower_than_terminal_and_input() {
    assert_eq!(
        first_command("cmd-k", &["Workspace", "Terminal"]).as_deref(),
        Some("smelt.palette/toggle")
    );
    assert!(
        matched("cmd-s", &["Workspace", "Terminal"]).is_empty(),
        "保存归文件视图自己，工作台不绑 Cmd+S"
    );
    assert!(
        matched("up", &["Workspace", "Terminal"]).is_empty(),
        "工作台不该抢终端的方向键"
    );
    assert!(
        matched("enter", &["Workspace", "Terminal"]).is_empty(),
        "不能从 Workspace 抢走终端的回车"
    );

    let cycle = matched("cmd-[", &["Workspace", "Input"]);
    assert!(cycle[0].action().as_any().is::<SidebarOpen>());
    assert!(
        cycle
            .iter()
            .any(|binding| command_of(binding).as_deref() == Some("smelt.stage/prev-pane"))
    );

    let hud = matched("cmd-shift-f", &["Workspace", "Input"]);
    assert!(hud[0].action().as_any().is::<SidebarOpen>());

    assert!(first_is::<SidebarOpen>("escape", &["Workspace", "Input"]));
    assert!(first_is::<DismissWorkspace>(
        "escape",
        &["Workspace", "Terminal"]
    ));
}

#[test]
fn session_and_split_chords_carry_their_commands() {
    assert_eq!(
        first_command("cmd-1", &["Workspace"]).as_deref(),
        Some("smelt.sessions/activate-1")
    );
    assert_eq!(
        first_command("cmd-9", &["Workspace"]).as_deref(),
        Some("smelt.sessions/activate-9")
    );
    assert_eq!(
        first_command("cmd-d", &["Workspace"]).as_deref(),
        Some("smelt.stage/split-right")
    );
    assert_eq!(
        first_command("shift-cmd-d", &["Workspace"]).as_deref(),
        Some("smelt.stage/split-down")
    );
    assert_eq!(
        first_command("cmd-w", &["Workspace", "Terminal"]).as_deref(),
        Some("smelt.stage/close-pane")
    );
}

/// 设置和退出在设置窗口里也能按，设置窗口把命令转给主窗口。
#[test]
fn app_chords_also_work_in_the_settings_window() {
    for stack in [&["Workspace", "Terminal"][..], &["SettingsWindow"][..]] {
        assert_eq!(
            first_command("cmd-q", stack).as_deref(),
            Some("smelt.app/quit")
        );
        assert_eq!(
            first_command("cmd-,", stack).as_deref(),
            Some("smelt.app/settings")
        );
    }
    assert!(
        matched("cmd-k", &["SettingsWindow"]).is_empty(),
        "工作台的键不该在设置窗口里命中"
    );
}

#[test]
fn workspace_chords_are_not_bound_at_the_root_context() {
    for spec in [
        "cmd-k",
        "cmd-q",
        "cmd-[",
        "cmd-]",
        "cmd-1",
        "cmd-d",
        "shift-cmd-d",
        "cmd-w",
        "cmd-s",
        "cmd-shift-f",
        "escape",
        "up",
        "enter",
    ] {
        let (bindings, _) = keymap().bindings_for_input(&[stroke(spec)], &contexts(&["Terminal"]));
        assert!(bindings.is_empty(), "{spec} 不该在没有 Workspace 时命中");
    }
}

/// 与 `app_key_bindings` 里关设置窗口的键一致。`Keystroke::parse` 不接受 `secondary-`。
#[cfg(target_os = "macos")]
const CLOSE_SETTINGS_CHORD: &str = "cmd-w";
#[cfg(not(target_os = "macos"))]
const CLOSE_SETTINGS_CHORD: &str = "ctrl-w";

/// 设置窗口 Cmd+W（Windows / Linux 是 Ctrl+W）关窗；它是普通窗口，Esc 不关它。
#[test]
fn settings_window_closes_on_secondary_w_but_not_escape() {
    assert!(first_is::<CloseSettingsWindow>(
        CLOSE_SETTINGS_CHORD,
        &["SettingsWindow"]
    ));
    assert!(matched("escape", &["SettingsWindow"]).is_empty());
}
