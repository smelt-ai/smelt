//! `<SmeltConversation>` 跨行/跨段/跨气泡框选与 `Cmd+C` 复制端到端回归测试
//! （独立进程，避开 `nsh_platform_gpui::rt::JOBS` 单进程 `OnceLock`）：
//! 1. 外层模拟 `Workspace` 持有 `outer_focus`（挂在 `AppView` 之外）时，拖拽选中助手多段
//!    Markdown 正文后按 `Cmd+C`，剪贴板必须包含跨段选中的全部文字；
//! 2. 跨消息从用户气泡拖选到助手回复末尾再按 `Cmd+C`，剪贴板必须同时包含用户消息和助手正文。

use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, Modifiers, MouseButton, ParentElement as _, Pixels, Render, Styled as _,
    TestAppContext, VisualTestContext, Window, div, point, px, size,
};
use nsh_core::element::description::SetDescription;
use nsh_platform_gpui::{AppOptions, AppView, Limits, Theme};
use serde_json::{Value, json};
use smelt_nsh::plugins::{PluginHost, Plugins};

const SHELL: &str = r#"
import { View } from "nsh";
import { Slot } from "./slots.tsx";

export function Shell() {
  return (
    <View style={{ flex: 1, width: "100%", height: "100%" }}>
      <Slot id="probe" style={{ flex: 1, width: "100%", height: "100%" }} />
    </View>
  );
}
"#;

const PLUGIN: &str = r#"
import { View } from "nsh";
import { SmeltConversation } from "smelt/conversation";

export default function Probe() {
  return (
    <View style={{ flex: 1, width: 900, height: 700 }}>
      <SmeltConversation
        machine={null}
        sessionId="acp-sel-1"
        spec={{
          agent: "pi",
          cwd: "/repo",
          binding: { type: "direct" },
        }}
        embedded={false}
        title="选区测试"
      />
    </View>
  );
}
"#;

/// 模拟主窗口外层 `Workspace`：外层 `div` 跟踪 `outer_focus`，内层挂载 `AppView`。
struct HostRoot {
    outer_focus: FocusHandle,
    app: Entity<AppView>,
}

impl Render for HostRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .track_focus(&self.outer_focus)
            .child(self.app.clone())
    }
}

fn a11y_nodes(cx: &mut VisualTestContext) -> Vec<Value> {
    let Some(raw) = cx.update(|window, _| window.debug_a11y_tree_json()) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    parsed
        .get("nodes")
        .and_then(Value::as_object)
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default()
}

fn node_text(node: &Value) -> Option<&str> {
    let aria = node.get("aria")?;
    aria.get("value")
        .and_then(Value::as_str)
        .or_else(|| aria.get("label").and_then(Value::as_str))
}

fn has_a11y_text(cx: &mut VisualTestContext, needle: &str) -> bool {
    a11y_nodes(cx)
        .into_iter()
        .any(|node| node_text(&node).is_some_and(|t| t.contains(needle)))
}

/// 从已绘制的 quad 中定位底部 Composer 卡片（宽 ~768px、高 >= 80px），消息区就在其正上方。
fn composer_top(cx: &mut VisualTestContext) -> Result<Pixels, String> {
    cx.update(|window, _| {
        let scale = window.scale_factor();
        let quads: Vec<(f32, f32, f32, f32, f32)> = window
            .painted_quads()
            .iter()
            .map(|q| {
                let b = q.bounds;
                (
                    b.origin.x.0 / scale,
                    b.origin.y.0 / scale,
                    b.size.width.0 / scale,
                    b.size.height.0 / scale,
                    q.corner_radii.top_left.0 / scale,
                )
            })
            .collect();
        quads
            .iter()
            .copied()
            .find(|&(_, y, w, h, _)| y > 100. && (w - 768.).abs() <= 4. && h >= 80.)
            .map(|(_, y, _, _, _)| px(y))
            .ok_or_else(|| format!("未找到 Composer 卡片 quad；quads={quads:?}"))
    })
}

fn drag_and_copy(
    cx: &mut VisualTestContext,
    outer_focus: &FocusHandle,
    app_focus: &FocusHandle,
    from: gpui::Point<Pixels>,
    to: gpui::Point<Pixels>,
) -> String {
    cx.update(|window, cx| {
        window.focus(outer_focus, cx);
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());

    let mid = point((from.x + to.x) * 0.5, (from.y + to.y) * 0.5);
    cx.simulate_mouse_move(from, None, Modifiers::default());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(mid, Some(MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let (app_focused, outer_focused) =
        cx.update(|window, _| (app_focus.is_focused(window), outer_focus.is_focused(window)));
    assert!(
        app_focused && !outer_focused,
        "在消息区拖拽选区应把 GPUI 焦点从外层移到 AppView（app_focused={app_focused}, outer_focused={outer_focused}）"
    );

    #[cfg(target_os = "macos")]
    let copy_stroke = "cmd-c";
    #[cfg(not(target_os = "macos"))]
    let copy_stroke = "ctrl-c";

    cx.simulate_keystrokes(copy_stroke);
    for _ in 0..8 {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        // 轮询例外：set_clipboard_text 经异步队列回主线程写入测试剪贴板；共 40ms。
        std::thread::sleep(Duration::from_millis(5));
    }

    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default()
}

#[gpui::test]
fn conversation_multiline_and_cross_message_selection_copies_to_clipboard(cx: &mut TestAppContext) {
    // 独立测试进程单测试，开启 GPUI a11y 树捕获以直接定位各段渲染边界。
    unsafe {
        std::env::set_var("GPUI_A11Y_ACTIVE", "1");
    }

    cx.executor().allow_parking();
    nsh_platform_gpui::set_ui_sets(vec![
        nsh_base::registry(),
        smelt_nsh::testing::host_registry(),
    ]);

    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("conversation_selection");
    let _ = fs::remove_dir_all(&root);

    let host = PluginHost::new();
    host.set_plugin_limits(Limits {
        memory_bytes: 32 << 20,
        load_budget: Duration::from_millis(250),
        call_budget: Duration::from_millis(120),
        ..Limits::default()
    });

    let sub_id_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let app_errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    host.handle("state", |_plugin, call| {
        call.resolve(json!({
            "revision": 1,
            "locale": "zh-CN",
            "theme": smelt_nsh::testing::theme_state(16.),
            "machines_meta": [{ "machine": null, "online": true }],
            "sessions_data": [],
        }));
    });
    host.handle("call", |_plugin, call| {
        call.resolve(Value::Null);
    });
    host.handle("context_menu_items", |_plugin, call| {
        call.resolve(json!([]));
    });
    {
        let sub_id_slot = Arc::clone(&sub_id_slot);
        host.handle("acp.subscribe", move |_plugin, call| {
            let sub_id = call.data()["sub_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            *sub_id_slot.lock().unwrap() = Some(sub_id);
            call.resolve(Value::Null);
        });
    }
    host.handle("acp.unsubscribe", |_plugin, call| {
        call.resolve(Value::Null);
    });

    let (host_root, cx) = cx.add_window_view(|window, cx| {
        let app = cx.new(|cx| {
            let v = AppView::new(
                AppOptions {
                    inline_root: true,
                    ..AppOptions::default()
                },
                window,
                cx,
            );
            v.set_theme(Some(Theme::default()), cx);
            v
        });
        let outer_focus = cx.focus_handle();
        HostRoot { outer_focus, app }
    });
    cx.simulate_resize(size(px(900.), px(700.)));

    let (outer_focus, app_view) =
        host_root.read_with(cx, |r, _| (r.outer_focus.clone(), r.app.clone()));
    {
        let app_errors = Arc::clone(&app_errors);
        let err_host = host.clone();
        app_view.update(cx, |view, _| {
            view.on_app_error(move |error| {
                app_errors.lock().unwrap().push(error.to_string());
                err_host.on_app_error(error);
            });
        });
    }

    let ui_sets = vec![
        SetDescription::from(nsh_base::registry()),
        SetDescription::from(smelt_nsh::testing::host_registry()),
    ];
    let mut plugins = Plugins::new(&root.join("stage"), SHELL, ui_sets).unwrap();
    let repo_plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    plugins.set_modules(smelt_nsh::discover_module_packages(&[
        smelt_nsh::SourceDir {
            path: repo_plugins,
            source: smelt_nsh::PluginSource::Builtin,
            watch: false,
        },
    ]));
    let plugin_dir = root.join("sources/probe");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(plugin_dir.join("main.tsx"), PLUGIN).unwrap();
    plugins
        .install_entry(
            "probe",
            &plugin_dir,
            "main.tsx",
            &["smelt/conversation".to_owned()],
        )
        .unwrap();

    let start = plugins.start();
    app_view.update(cx, |view, _| host.apply(view, start));

    let wait_until = |cx: &mut VisualTestContext,
                      what: &str,
                      done: &mut dyn FnMut(&mut VisualTestContext) -> bool| {
        let begin = Instant::now();
        loop {
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            assert!(
                begin.elapsed() < Duration::from_secs(10),
                "等待 {what} 超时；errors={:?}; a11y={:?}",
                app_errors.lock().unwrap(),
                a11y_nodes(cx),
            );
            // 轮询例外：测试平台等待引擎线程提交帧并完成重绘；周期 15ms，上限 10s。
            std::thread::sleep(Duration::from_millis(15));
        }
    };

    wait_until(cx, "acp.subscribe", &mut |_| {
        sub_id_slot.lock().unwrap().is_some()
    });
    let sub_id = sub_id_slot.lock().unwrap().clone().unwrap();

    host.send_to(
        "probe",
        "smelt.acp.event",
        json!({
            "sub_id": sub_id,
            "kind": "snapshot",
            "snapshot": {
                "phase": "ready",
                "replaying_history": false,
                "entries": [
                    { "User": "第一条用户消息 ALPHA_USER" },
                    {
                        "Assistant": {
                            "text": "助手第一段 BETA_ONE\n\n助手第二段 GAMMA_TWO\n\n助手第三段 DELTA_THREE",
                            "thought": false
                        }
                    }
                ],
                "entries_offset": 0,
                "entries_total": 2,
                "snapshot_revision": 1,
                "config_options": [],
                "model": null,
            },
        }),
    );

    // 等欢迎态过渡动画结束、底部 Composer 卡片落稳（连续 3 帧位置不变）。
    let mut stable_top: Option<(Pixels, u8)> = None;
    wait_until(
        cx,
        "用户消息与助手三段正文渲染且输入栏落稳",
        &mut |cx| {
            if !(has_a11y_text(cx, "ALPHA_USER")
                && has_a11y_text(cx, "BETA_ONE")
                && has_a11y_text(cx, "DELTA_THREE"))
            {
                return false;
            }
            let Ok(top) = composer_top(cx) else {
                return false;
            };
            if top <= px(500.) {
                stable_top = None;
                return false;
            }
            match stable_top {
                Some((prev, n)) if (prev - top).abs() < px(0.5) => {
                    stable_top = Some((top, n + 1));
                    n + 1 >= 3
                }
                _ => {
                    stable_top = Some((top, 1));
                    false
                }
            }
        },
    );

    let app_focus = app_view.read_with(cx, |v, cx| gpui::Focusable::focus_handle(v, cx));

    let comp_top = composer_top(cx).unwrap();
    // 窗口宽 900、高 700，消息列 maxWidth=768 居中（x=66..834），List 采用 alignment="bottom" + paddingBottom=20：
    // 从落稳后的 comp_top（~582px）往上推算：
    // - 底部边距 + 助手动作槽之上是三段 Markdown（行高 23px + 段间距 ~12px）：
    //   DELTA_THREE ≈ comp_top - 64、GAMMA_TWO ≈ comp_top - 100、BETA_ONE ≈ comp_top - 134；
    // - 列表间距 12px 之上是右对齐用户气泡（x≈640..834），中心在 comp_top - 205 附近。
    let assistant_start = point(px(68.), comp_top - px(134.));
    let assistant_end = point(px(520.), comp_top - px(40.));
    let user_start = point(px(620.), comp_top - px(205.));

    // 1. 助手回复内跨三段拖拽选择 + Cmd+C（外层焦点初始在 outer_focus）
    let assistant_copied =
        drag_and_copy(cx, &outer_focus, &app_focus, assistant_start, assistant_end);
    assert!(
        !assistant_copied.contains("ALPHA_USER")
            && assistant_copied.contains("BETA_ONE")
            && assistant_copied.contains("GAMMA_TWO")
            && assistant_copied.contains("DELTA_THREE"),
        "助手多段框选 Cmd+C 应精准复制助手三段（不含用户消息），实际剪贴板：{assistant_copied:?}（comp_top={comp_top:?}）"
    );

    // 2. 跨气泡从用户消息拖拽到助手最后一段 + Cmd+C
    let cross_copied = drag_and_copy(cx, &outer_focus, &app_focus, user_start, assistant_end);
    assert!(
        cross_copied.contains("ALPHA_USER")
            && cross_copied.contains("BETA_ONE")
            && cross_copied.contains("GAMMA_TWO")
            && cross_copied.contains("DELTA_THREE"),
        "跨消息框选 Cmd+C 应同时包含用户与助手全部三段正文，实际剪贴板：{cross_copied:?}（comp_top={comp_top:?}）"
    );

    // 3. 右键消息不应清除已有选区，Esc 收起右键菜单后 Cmd+C 仍能复制选区
    cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string(String::new())));
    cx.simulate_mouse_move(assistant_start, None, Modifiers::default());
    cx.simulate_mouse_down(assistant_start, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(assistant_start, MouseButton::Right, Modifiers::default());
    for _ in 0..4 {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        // 轮询例外：右键菜单经异步 contextMenuItems 后弹出；共 20ms。
        std::thread::sleep(Duration::from_millis(5));
    }
    cx.simulate_keystrokes("escape");
    #[cfg(target_os = "macos")]
    let copy_only = "cmd-c";
    #[cfg(not(target_os = "macos"))]
    let copy_only = "ctrl-c";
    cx.simulate_keystrokes(copy_only);
    for _ in 0..8 {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        // 轮询例外：set_clipboard_text 经异步队列回主线程写入测试剪贴板；共 40ms。
        std::thread::sleep(Duration::from_millis(5));
    }
    let after_right_click = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert!(
        after_right_click.contains("ALPHA_USER") && after_right_click.contains("DELTA_THREE"),
        "右键消息不应清空已有跨消息选区，实际剪贴板：{after_right_click:?}"
    );

    // 4. 点击消息区后 Cmd+A + Cmd+C 应选中并复制全部对话内容
    cx.update(|window, cx| {
        window.focus(&outer_focus, cx);
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.simulate_click(assistant_start, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    #[cfg(target_os = "macos")]
    let select_all_copy = "cmd-a cmd-c";
    #[cfg(not(target_os = "macos"))]
    let select_all_copy = "ctrl-a ctrl-c";
    cx.simulate_keystrokes(select_all_copy);
    for _ in 0..8 {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        // 轮询例外：set_clipboard_text 经异步队列回主线程写入测试剪贴板；共 40ms。
        std::thread::sleep(Duration::from_millis(5));
    }
    let all_copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert!(
        all_copied.contains("ALPHA_USER")
            && all_copied.contains("BETA_ONE")
            && all_copied.contains("GAMMA_TWO")
            && all_copied.contains("DELTA_THREE"),
        "Cmd+A + Cmd+C 应复制全部对话内容，实际剪贴板：{all_copied:?}"
    );

    let errs = app_errors.lock().unwrap();
    assert!(errs.is_empty(), "unexpected app_errors: {errs:?}");
}
