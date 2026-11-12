//! 改键：命令的键怎么算、覆盖怎么生效、键表里怎么换段、录到的键怎么判。不拉窗口。

use gpui::{App, KeyBinding, KeyContext, Keymap, Keystroke, OperatingSystem, TestAppContext};
use smelt_client_store::KeyBindingRecord;
use smelt_plugin_api::{self as api, CommandInfo, RunCommand};

use super::{
    APP_KEYMAP, Bound, Overrides, Recorded, app_key_bindings, bind_commands,
    overrides_from_records, recorded, splice_app_bindings,
};
use crate::workspace_commands::{self, COMMANDS};
use crate::{DismissWorkspace, SidebarOpen};

/// 测试 App 的本机系统，录键判定和解析都按它来，跟装键表时一致。
fn os() -> OperatingSystem {
    TestAppContext::single().update(|cx| cx.platform_info().os)
}

fn stroke(spec: &str) -> Keystroke {
    Keystroke::parse_with_os(spec, os()).expect("keystroke")
}

/// 在一个不开窗口的测试 App 里登记自带命令，按覆盖算键。
fn with_builtin<R>(overrides: &Overrides, f: impl FnOnce(Vec<Bound>, &App) -> R) -> R {
    TestAppContext::single().update(|cx| {
        workspace_commands::register_inert(cx);
        let bound = bind_commands(api::commands(cx), overrides, cx.platform_info().os);
        f(bound, cx)
    })
}

fn bound(overrides: &Overrides) -> Vec<Bound> {
    with_builtin(overrides, |bound, _| bound)
}

fn app_bindings(overrides: &Overrides) -> Vec<KeyBinding> {
    with_builtin(overrides, |bound, cx| app_key_bindings(&bound, cx))
}

fn overrides(pairs: &[(&str, Option<&str>)]) -> Overrides {
    pairs
        .iter()
        .map(|(id, key)| (id.to_string(), key.map(str::to_string)))
        .collect()
}

fn info(id: &str, key: Option<&str>) -> CommandInfo {
    CommandInfo {
        id: id.to_string().into(),
        group: "测试".into(),
        title: id.to_string().into(),
        key: key.map(|key| key.to_string().into()),
        context: "Workspace".into(),
        palette: true,
    }
}

fn keys(bound: &[Bound]) -> Vec<(String, Option<String>)> {
    bound
        .iter()
        .map(|command| (command.info.id.to_string(), command.key.clone()))
        .collect()
}

fn workspace() -> Vec<KeyContext> {
    vec![KeyContext::parse("Workspace").expect("context")]
}

const PALETTE: &str = "smelt.palette/toggle";

fn palette_keys(overrides: &Overrides) -> Vec<Keystroke> {
    let keymap = Keymap::new(app_bindings(overrides));
    keymap
        .bindings_for_action(&RunCommand::new(PALETTE))
        .map(|binding| binding.keystrokes()[0].inner().clone())
        .collect()
}

/// 存下来的按键按平台写（macOS `cmd-`、Linux `super-`），比的是解析后的按键。
fn assert_set(outcome: Recorded, spec: &str) {
    match outcome {
        Recorded::Set(Some(key)) => assert_eq!(stroke(&key), stroke(spec)),
        other => panic!("应该改成 {spec}，实际是 {other:?}"),
    }
}

fn judge_with(id: &str, spec: &str, overrides: &Overrides, extra: &[KeyBinding]) -> Recorded {
    let (bound, mut keymap) = with_builtin(overrides, |bound, cx| {
        let keymap = app_key_bindings(&bound, cx);
        (bound, keymap)
    });
    keymap.extend(extra.iter().cloned());
    recorded(id, &stroke(spec), &bound, &keymap, os())
}

fn judge(id: &str, spec: &str, overrides: &Overrides) -> Recorded {
    judge_with(id, spec, overrides, &[])
}

/// 自带命令的表登记得上（id 不重、键和 context 写得对，`register_inert` 失败会 debug 断言），
/// 而且每个出厂键都绑上了：不合规或撞车会被 `bind_commands` 丢掉。
#[test]
fn every_builtin_default_key_is_bound() {
    let bound = bound(&Overrides::new());
    assert_eq!(bound.len(), COMMANDS.len());
    for def in COMMANDS {
        let id = format!("{}/{}", def.owner, def.name);
        let command = bound
            .iter()
            .find(|command| command.info.id.as_ref() == id)
            .unwrap_or_else(|| panic!("{id} 没登记上"));
        assert_eq!(command.key.as_deref(), def.key, "{id} 的出厂键没绑上");
        assert!(!command.overridden);
    }
}

#[test]
fn every_app_binding_carries_the_app_marker() {
    assert!(
        app_bindings(&Overrides::new())
            .iter()
            .all(|binding| binding.meta() == Some(APP_KEYMAP))
    );
}

#[test]
fn an_override_replaces_the_default_and_none_unbinds() {
    assert_eq!(palette_keys(&Overrides::new()), vec![stroke("cmd-k")]);
    assert_eq!(
        palette_keys(&overrides(&[(PALETTE, Some("cmd-shift-p"))])),
        vec![stroke("cmd-shift-p")]
    );
    assert!(palette_keys(&overrides(&[(PALETTE, None)])).is_empty());

    let keymap = Keymap::new(app_bindings(&overrides(&[(PALETTE, Some("cmd-shift-p"))])));
    let (old, _) = keymap.bindings_for_input(&[stroke("cmd-k")], &workspace());
    assert!(old.is_empty(), "改走了以后旧键不能还在");
}

/// 用户改的键先占，出厂键按登记顺序排；撞了的、不合规的出厂键不绑。
#[test]
fn overrides_win_and_bad_or_clashing_defaults_stay_unbound() {
    let commands = vec![
        info("a/first", Some("cmd-j")),
        info("b/second", Some("cmd-j")),
        info("b/terminal", Some("ctrl-r")),
        info("b/copy", Some("cmd-c")),
        info("b/f-key", Some("f8")),
        info("c/moved", Some("cmd-l")),
    ];
    let bound = bind_commands(
        commands.clone(),
        &overrides(&[("c/moved", Some("cmd-j")), ("b/second", None)]),
        os(),
    );
    let keys: Vec<_> = keys(&bound)
        .into_iter()
        .map(|(id, key)| (id, key.map(|key| stroke(&key))))
        .collect();
    assert_eq!(
        keys,
        vec![
            ("a/first".to_string(), None),
            ("b/second".to_string(), None),
            ("b/terminal".to_string(), None),
            ("b/copy".to_string(), None),
            ("b/f-key".to_string(), Some(stroke("f8"))),
            ("c/moved".to_string(), Some(stroke("cmd-j"))),
        ]
    );
    assert!(bound[1].overridden && bound[5].overridden && !bound[0].overridden);

    // 没有覆盖时先登记的拿到撞车的键。
    let bound = bind_commands(commands, &Overrides::new(), os());
    assert_eq!(bound[0].key.as_deref(), Some("cmd-j"));
    assert_eq!(bound[1].key, None);
}

/// 还没登记的命令（插件没加载）的覆盖留着，解析不了的按键丢掉。
#[test]
fn unknown_commands_are_kept_and_broken_keystrokes_dropped() {
    let loaded = overrides_from_records(
        vec![
            KeyBindingRecord {
                command: PALETTE.to_string(),
                keystroke: Some("cmd-j".to_string()),
            },
            KeyBindingRecord {
                command: "smelt.sessions/new-conversation".to_string(),
                keystroke: None,
            },
            KeyBindingRecord {
                command: "trader/buy".to_string(),
                keystroke: Some("cmd-l".to_string()),
            },
            // 不认识的修饰键解析不了。`cmd-` 倒是合法的：⌘ 加减号键。
            KeyBindingRecord {
                command: "smelt.stage/split-right".to_string(),
                keystroke: Some("hyper-k".to_string()),
            },
        ],
        os(),
    );
    assert_eq!(
        loaded,
        overrides(&[
            (PALETTE, Some("cmd-j")),
            ("smelt.sessions/new-conversation", None),
            ("trader/buy", Some("cmd-l")),
        ])
    );
    // 没登记的不绑，也不占键。
    assert!(
        keys(&bound(&loaded))
            .iter()
            .all(|(id, _)| id != "trader/buy")
    );
}

/// 插件后来加的键、组件自带的键都原地留着，smelt 那一段换到原来的位置上。
#[test]
fn rebuilding_swaps_only_the_app_segment_in_place() {
    let before = KeyBinding::new("cmd-shift-x", DismissWorkspace, Some("Component"));
    let after = KeyBinding::new("cmd-shift-y", SidebarOpen, Some("Plugin"));
    let mut existing = vec![before];
    existing.extend(app_bindings(&Overrides::new()));
    existing.push(after);

    let app = app_bindings(&overrides(&[(
        "smelt.sessions/new-conversation",
        Some("cmd-shift-m"),
    )]));
    let app_len = app.len();
    let rebuilt = splice_app_bindings(existing, app);

    assert_eq!(rebuilt.len(), app_len + 2);
    assert_eq!(rebuilt[0].keystrokes()[0].inner(), &stroke("cmd-shift-x"));
    assert_eq!(rebuilt[0].meta(), None);
    assert!(
        rebuilt[1..=app_len]
            .iter()
            .all(|b| b.meta() == Some(APP_KEYMAP))
    );
    assert_eq!(
        rebuilt[app_len + 1].keystrokes()[0].inner(),
        &stroke("cmd-shift-y")
    );
    let new_conversation = RunCommand::new("smelt.sessions/new-conversation");
    let moved: Vec<_> = rebuilt
        .iter()
        .filter(|binding| binding.action().partial_eq(&new_conversation))
        .collect();
    assert_eq!(moved.len(), 1, "旧的那段不能留下来");
    assert_eq!(moved[0].keystrokes()[0].inner(), &stroke("cmd-shift-m"));
}

#[test]
fn escape_cancels_and_backspace_clears() {
    let none = Overrides::new();
    assert_eq!(judge(PALETTE, "escape", &none), Recorded::Cancel);
    assert_eq!(judge(PALETTE, "backspace", &none), Recorded::Set(None));
    assert_set(judge(PALETTE, "cmd-shift-p", &none), "cmd-shift-p");
    assert_set(judge("smelt.git/next-hunk", "f8", &none), "f8");
}

#[test]
fn recording_the_default_again_is_a_reset() {
    let moved = overrides(&[(PALETTE, Some("cmd-j"))]);
    assert_eq!(judge(PALETTE, "cmd-k", &moved), Recorded::Reset);
}

#[test]
fn keys_without_command_are_left_to_the_terminal_and_inputs() {
    let none = Overrides::new();
    for spec in ["ctrl-r", "alt-p", "shift-a", "a"] {
        assert!(
            matches!(judge(PALETTE, spec, &none), Recorded::Rejected(_)),
            "{spec} 不该收"
        );
    }
}

/// 撞上另一条命令的键不直接抢，也不直接拒：交给用户确认改绑。
#[test]
fn a_key_used_by_another_command_asks_to_reassign() {
    let none = Overrides::new();
    for (spec, other) in [
        ("cmd-n", "smelt.sessions/new-conversation"),
        ("cmd-q", "smelt.app/quit"),
        ("cmd-1", "smelt.sessions/activate-1"),
    ] {
        match judge(PALETTE, spec, &none) {
            Recorded::Taken {
                other: taken,
                keystroke,
            } => {
                assert_eq!(taken.as_ref(), other);
                assert_eq!(stroke(&keystroke), stroke(spec));
            }
            outcome => panic!("{spec} 应该问要不要改绑，实际是 {outcome:?}"),
        }
    }
    // 改走以后，原来的键就空出来了。
    let moved = overrides(&[("smelt.sessions/new-conversation", Some("cmd-shift-m"))]);
    assert_set(judge(PALETTE, "cmd-n", &moved), "cmd-n");
    // 自己现在的键不算冲突。
    assert_eq!(judge(PALETTE, "cmd-k", &none), Recorded::Reset);
}

/// 编辑键、终端自己处理的键和系统用的键直接拒；键表里改不了的键也拒，设置窗口的关窗键
/// 不算。
#[test]
fn reserved_and_fixed_keys_are_refused() {
    let none = Overrides::new();
    for spec in ["cmd-c", "cmd-z", "cmd-f", "cmd-h", "cmd-m"] {
        assert!(
            matches!(judge(PALETTE, spec, &none), Recorded::Rejected(_)),
            "{spec} 不该收"
        );
    }
    assert!(matches!(
        judge(PALETTE, "cmd-c", &none),
        Recorded::Rejected(reason) if reason.contains("复制")
    ));

    let component = KeyBinding::new("cmd-shift-x", DismissWorkspace, Some("Input"));
    assert!(matches!(
        judge_with(PALETTE, "cmd-shift-x", &none, &[component]),
        Recorded::Rejected(_)
    ));

    #[cfg(target_os = "macos")]
    {
        let cleared = overrides(&[("smelt.stage/close-pane", None)]);
        assert_set(judge(PALETTE, "cmd-w", &cleared), "cmd-w");
    }
}
