//! 改键：出厂键位、覆盖怎么生效、键表里怎么换段、录到的键怎么判。不拉窗口。

use gpui::{KeyBinding, KeyContext, Keymap, Keystroke, OperatingSystem, TestAppContext};
use smelt_client_store::KeyBindingRecord;

use super::{
    APP_KEYMAP, Overrides, PLUGIN_KEYMAP, Recorded, SHORTCUTS, app_key_bindings,
    overrides_from_records, plan_plugin_keys, recorded, splice_app_bindings,
};
use crate::plugins::{DeclaredCommand, PluginCommand};
use crate::{OpenDocs, ReportIssue, RunPluginCommand, SplitBeside, ToggleCommandPalette};

/// 测试 App 的本机系统，录键判定和解析都按它来，跟 `app_key_bindings` 一致。
fn os() -> OperatingSystem {
    TestAppContext::single().update(|cx| cx.platform_info().os)
}

fn stroke(spec: &str) -> Keystroke {
    Keystroke::parse_with_os(spec, os()).expect("keystroke")
}

/// `KeyBinding::load` 要 App，用一个不开窗口的测试 App 算键位。
fn app_bindings(overrides: &Overrides) -> Vec<KeyBinding> {
    TestAppContext::single().update(|cx| app_key_bindings(overrides, cx))
}

fn overrides(pairs: &[(&str, Option<&str>)]) -> Overrides {
    pairs
        .iter()
        .map(|(id, key)| (id.to_string(), key.map(str::to_string)))
        .collect()
}

fn workspace() -> Vec<KeyContext> {
    vec![KeyContext::parse("Workspace").expect("context")]
}

fn palette_keys(overrides: &Overrides) -> Vec<Keystroke> {
    let keymap = Keymap::new(app_bindings(overrides));
    keymap
        .bindings_for_action(&ToggleCommandPalette)
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

fn judge(id: &str, spec: &str, overrides: &Overrides) -> Recorded {
    let keymap = app_bindings(overrides);
    recorded(id, &stroke(spec), overrides, &keymap, os())
}

#[test]
fn shortcut_ids_are_unique_and_defaults_do_not_collide() {
    for (ix, shortcut) in SHORTCUTS.iter().enumerate() {
        Keystroke::parse(shortcut.default).expect(shortcut.id);
        for other in &SHORTCUTS[ix + 1..] {
            assert_ne!(shortcut.id, other.id, "动作名重复");
            assert_ne!(
                stroke(shortcut.default),
                stroke(other.default),
                "{} 和 {} 的出厂键位撞了",
                shortcut.id,
                other.id
            );
        }
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
        palette_keys(&overrides(&[("command_palette", Some("cmd-shift-p"))])),
        vec![stroke("cmd-shift-p")]
    );
    assert!(palette_keys(&overrides(&[("command_palette", None)])).is_empty());

    let keymap = Keymap::new(app_bindings(&overrides(&[(
        "command_palette",
        Some("cmd-shift-p"),
    )])));
    let (old, _) = keymap.bindings_for_input(&[stroke("cmd-k")], &workspace());
    assert!(old.is_empty(), "改走了以后旧键不能还在");
}

#[test]
fn unknown_actions_and_broken_keystrokes_fall_back_to_defaults() {
    let loaded = overrides_from_records(
        vec![
            KeyBindingRecord {
                action_id: "command_palette".to_string(),
                keystroke: Some("cmd-j".to_string()),
            },
            KeyBindingRecord {
                action_id: "new_conversation".to_string(),
                keystroke: None,
            },
            KeyBindingRecord {
                action_id: "no_such_action".to_string(),
                keystroke: Some("cmd-l".to_string()),
            },
            // 不认识的修饰键解析不了。`cmd-` 倒是合法的：⌘ 加减号键。
            KeyBindingRecord {
                action_id: "split_beside".to_string(),
                keystroke: Some("hyper-k".to_string()),
            },
        ],
        os(),
    );
    assert_eq!(
        loaded,
        overrides(&[
            ("command_palette", Some("cmd-j")),
            ("new_conversation", None)
        ])
    );
}

/// 插件后来加的键、组件自带的键都原地留着，smelt 那一段换到原来的位置上。
#[test]
fn rebuilding_swaps_only_the_app_segment_in_place() {
    let before = KeyBinding::new("cmd-shift-x", OpenDocs, Some("Component"));
    let after = KeyBinding::new("cmd-shift-y", ReportIssue, Some("Plugin"));
    let mut existing = vec![before];
    existing.extend(app_bindings(&Overrides::new()));
    existing.push(after);

    let app = app_bindings(&overrides(&[("split_beside", Some("cmd-shift-b"))]));
    let app_len = app.len();
    let rebuilt = splice_app_bindings(existing, app);

    assert_eq!(rebuilt.len(), app_len + 2);
    assert!(rebuilt[0].action().as_any().is::<OpenDocs>());
    assert!(rebuilt[1..=app_len].iter().all(|b| b.meta() == Some(APP_KEYMAP)));
    assert!(rebuilt[app_len + 1].action().as_any().is::<ReportIssue>());
    let split: Vec<_> = rebuilt
        .iter()
        .filter(|binding| binding.action().as_any().is::<SplitBeside>())
        .collect();
    assert_eq!(split.len(), 1, "旧的那段不能留下来");
    assert_eq!(split[0].keystrokes()[0].inner(), &stroke("cmd-shift-b"));
}

#[test]
fn escape_cancels_and_backspace_clears() {
    let none = Overrides::new();
    assert_eq!(judge("command_palette", "escape", &none), Recorded::Cancel);
    assert_eq!(judge("command_palette", "backspace", &none), Recorded::Set(None));
    assert_set(judge("command_palette", "cmd-shift-p", &none), "cmd-shift-p");
    assert_set(judge("next_hunk", "f8", &none), "f8");
}

#[test]
fn recording_the_default_again_is_a_reset() {
    let moved = overrides(&[("command_palette", Some("cmd-j"))]);
    assert_eq!(judge("command_palette", "cmd-k", &moved), Recorded::Reset);
}

#[test]
fn keys_without_command_are_left_to_the_terminal_and_inputs() {
    let none = Overrides::new();
    for spec in ["ctrl-r", "alt-p", "shift-a", "a"] {
        assert!(
            matches!(judge("command_palette", spec, &none), Recorded::Rejected(_)),
            "{spec} 不该收"
        );
    }
}

/// 撞上另一条能改的快捷键不直接抢，也不直接拒：交给用户确认改绑。
#[test]
fn a_key_used_by_another_shortcut_asks_to_reassign() {
    let none = Overrides::new();
    match judge("command_palette", "cmd-d", &none) {
        Recorded::Taken { other, keystroke } => {
            assert_eq!(other, "split_beside");
            assert_eq!(stroke(&keystroke), stroke("cmd-d"));
        }
        other => panic!("应该问要不要改绑，实际是 {other:?}"),
    }
    // 改走以后，原来的键就空出来了。
    let moved = overrides(&[("split_beside", Some("cmd-shift-b"))]);
    assert_set(judge("command_palette", "cmd-d", &moved), "cmd-d");
    // 自己现在的键不算冲突。
    assert_eq!(judge("command_palette", "cmd-k", &none), Recorded::Reset);
}

/// 改不了的键、编辑键、终端自己处理的键和系统用的键直接拒。
#[test]
fn fixed_reserved_and_system_keys_are_refused() {
    let none = Overrides::new();
    for spec in ["cmd-q", "cmd-1", "cmd-c", "cmd-z", "cmd-f", "cmd-h", "cmd-m"] {
        assert!(
            matches!(judge("command_palette", spec, &none), Recorded::Rejected(_)),
            "{spec} 不该收"
        );
    }
    assert!(matches!(
        judge("command_palette", "cmd-c", &none),
        Recorded::Rejected(reason) if reason.contains("复制")
    ));
}

fn plugin_command(plugin: &str, name: &str, key: Option<&str>) -> PluginCommand {
    PluginCommand {
        plugin: plugin.to_string(),
        plugin_name: format!("{plugin} 插件"),
        command: DeclaredCommand {
            name: name.to_string(),
            title: format!("{name} 命令"),
            key: key.map(str::to_string),
        },
    }
}

/// 按 smelt 现在的键（出厂或改过的）算插件命令的键：装上的给下标和按键，装不上的给原因。
fn plan(
    commands: &[PluginCommand],
    overrides: &Overrides,
    extra: &[KeyBinding],
) -> (Vec<(usize, Keystroke)>, Vec<(String, String)>) {
    let app = app_bindings(overrides);
    let fixed: Vec<&KeyBinding> = extra.iter().chain(&app).collect();
    plan_plugin_keys(commands, overrides, &fixed, os())
}

#[test]
fn plugin_keys_that_follow_the_rules_are_planned() {
    let commands = [
        plugin_command("party", "jump", Some("cmd-shift-j")),
        plugin_command("party", "quiet", None),
        plugin_command("board", "refresh", Some("f9")),
    ];
    let (planned, problems) = plan(&commands, &Overrides::new(), &[]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        planned,
        [(0, stroke("cmd-shift-j")), (2, stroke("f9"))],
        "没声明键的命令不装键"
    );
}

/// 和用户录键同一套规矩，外加只收一个按键、写法要对。
#[test]
fn plugin_keys_that_break_the_rules_are_reported() {
    for (key, needle) in [
        ("ctrl-b", "要带 ⌘"),
        ("cmd-c", "复制"),
        ("cmd-k cmd-s", "连按"),
        ("cmd-hyper-j", "写法不对"),
        ("cmd-d", "左右分屏"),
        ("cmd-q", "已被别的功能占用"),
    ] {
        let commands = [plugin_command("party", "jump", Some(key))];
        let (planned, problems) = plan(&commands, &Overrides::new(), &[]);
        assert!(planned.is_empty(), "{key} 不该装上");
        let [(plugin, reason)] = problems.as_slice() else {
            panic!("{key} 应该报一条，实际是 {problems:?}");
        };
        assert_eq!(plugin, "party");
        assert!(
            reason.contains(needle),
            "{key}：「{reason}」里没有「{needle}」"
        );
    }
}

/// smelt 的快捷键按用户当前的键位算：改走了，原来的键插件就能用。
#[test]
fn plugin_keys_follow_the_users_current_shortcuts() {
    let commands = [plugin_command("party", "jump", Some("cmd-d"))];
    let moved = overrides(&[("split_beside", Some("cmd-shift-b"))]);
    let (planned, problems) = plan(&commands, &moved, &[]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(planned, [(0, stroke("cmd-d"))]);

    let commands = [plugin_command("party", "jump", Some("cmd-shift-b"))];
    let (planned, problems) = plan(&commands, &moved, &[]);
    assert!(planned.is_empty());
    assert!(problems[0].1.contains("左右分屏"), "{problems:?}");
}

/// 插件之间先到先得；脚本自己绑的、组件的键也不抢。
#[test]
fn plugin_keys_do_not_take_keys_already_held() {
    let commands = [
        plugin_command("party", "jump", Some("cmd-shift-j")),
        plugin_command("board", "jump", Some("cmd-shift-j")),
        plugin_command("board", "open", Some("cmd-shift-o")),
    ];
    let held = [KeyBinding::new("cmd-shift-o", OpenDocs, Some("Component"))];
    let (planned, problems) = plan(&commands, &Overrides::new(), &held);
    assert_eq!(planned, [(0, stroke("cmd-shift-j"))]);
    assert_eq!(problems.len(), 2, "{problems:?}");
    let (_, first) = &problems[0];
    assert!(
        first.contains("「party 插件」的「jump 命令」先用了"),
        "{first}"
    );
    assert!(problems[1].1.contains("已被别的功能占用"));
}

/// 插件命令的键和 smelt 自己的键是同一段，一起换；用户录键撞上它时说是哪个插件的。
#[test]
fn plugin_bindings_live_in_the_app_segment() {
    let plugin = KeyBinding::new(
        "cmd-shift-j",
        RunPluginCommand {
            plugin: "party".to_string(),
            command: "jump".to_string(),
        },
        Some("Workspace"),
    )
    .with_meta(PLUGIN_KEYMAP);
    let other = KeyBinding::new("cmd-shift-y", ReportIssue, Some("Plugin"));
    let mut existing = app_bindings(&Overrides::new());
    existing.push(plugin.clone());
    existing.push(other);

    let none = Overrides::new();
    let keystroke = stroke("cmd-shift-j");
    match recorded("command_palette", &keystroke, &none, &existing, os()) {
        Recorded::Rejected(reason) => {
            assert!(reason.contains("插件命令 party/jump"), "{reason}")
        }
        other => panic!("应该拒，实际是 {other:?}"),
    }

    let rebuilt = splice_app_bindings(existing, app_bindings(&none));
    assert!(
        !rebuilt
            .iter()
            .any(|binding| binding.action().as_any().is::<RunPluginCommand>()),
        "旧的插件键跟着那一段换掉"
    );
    assert!(
        rebuilt
            .last()
            .is_some_and(|binding| binding.action().as_any().is::<ReportIssue>())
    );
}
