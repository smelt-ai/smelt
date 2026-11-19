//! 对话页出图：离屏把对话页画成 PNG，给人和 agent 对照设计稿读图。不做像素比对。
//!
//! ```sh
//! cargo test -j 2 -p smelt-conversation conversation_snapshots -- --ignored --nocapture
//! # 画一份真实会话：快照是 smeltd `acp_snapshot` 的回包（带不带外层 `snapshot` 都行）
//! SMELT_CONVERSATION_SNAPSHOT=/path/snap.json cargo test -j 2 -p smelt-conversation \
//!     conversation_snapshots -- --ignored --nocapture
//! ```
//!
//! 图在 `target/tmp/snapshots/conversation/`：每一问滚到顶各一张，段收着、全展开各一套，
//! 深浅两套主题，2 倍像素。字形走真实的 CoreText，渲染走离屏 Metal。没给真实快照时另画
//! 等人回答的提问卡（`ask-*`），一张键盘高亮在第二项、一张没有焦点。
//!
//! 默认不跑：界面语言、主题色板都是进程级全局，和并行的其它测试会互相盖掉。

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext as _, HeadlessAppContext, px, size};
use smelt_plugin_api::model::acp_session::ConversationSnapshot;
use smelt_plugin_api::model::theme_index::{DEFAULT_DARK, DEFAULT_LIGHT};

use super::*;

const WIDTH: f32 = 960.;
const HEIGHT: f32 = 1400.;

#[test]
#[ignore = "出图工具，手动跑，见模块说明"]
fn conversation_snapshots() {
    let snapshot = match std::env::var_os("SMELT_CONVERSATION_SNAPSHOT") {
        Some(path) => load_snapshot(Path::new(&path)),
        None => sample_snapshot(),
    };
    rust_i18n::set_locale(&std::env::var("SMELT_LOCALE").unwrap_or_else(|_| "zh-CN".into()));
    // 单元测试没有 `CARGO_TARGET_TMPDIR`；和 smelt-ui 的快照放在同一个 target/tmp 下。
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target"),
        std::path::PathBuf::from,
    );
    let out = target.join("tmp/snapshots/conversation");
    // 上一份会话的图留着会混进来。
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("建出图目录");
    let ask = std::env::var_os("SMELT_CONVERSATION_SNAPSHOT")
        .is_none()
        .then(ask_snapshot);
    let mut written = Vec::new();
    for (theme, suffix) in [(DEFAULT_LIGHT, "light"), (DEFAULT_DARK, "dark")] {
        for expanded in [false, true] {
            written.extend(render(&snapshot, theme, suffix, expanded, None, &out));
        }
        if let Some(ask) = &ask {
            for keyboard in [false, true] {
                let name = if keyboard { "ask-keyboard" } else { "ask" };
                written.extend(render(
                    ask,
                    theme,
                    suffix,
                    false,
                    Some((name, keyboard)),
                    &out,
                ));
            }
        }
    }
    assert!(!written.is_empty(), "一张图也没画出来");
    for path in &written {
        println!("{}", path.display());
    }
}

fn load_snapshot(path: &Path) -> ConversationSnapshot {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("读 {} 失败: {error}", path.display()));
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("快照不是 JSON");
    if let Some(inner) = value.get_mut("snapshot") {
        value = inner.take();
    }
    serde_json::from_value(value).expect("快照字段对不上")
}

/// 没给真实快照时画的样例：跑完的一轮（有说明、有改动、有回答）和正在跑的一轮。
fn sample_snapshot() -> ConversationSnapshot {
    let entries = vec![
        AcpEntry::User("先把配对和推送注册这块的问题列出来，别急着改".into()),
        thought("Looking at the pairing flow first"),
        completed_tool(
            "run-1",
            ToolKind::Execute,
            "rg -n \"push_registration\" crates/",
        ),
        completed_tool("read-1", ToolKind::Read, "crates/smelt-mobile/src/push.rs"),
        completed_tool("read-2", ToolKind::Read, "crates/smeltd/src/devices.rs"),
        AcpEntry::Assistant {
            text: "先看了配对和推送注册两条路径，接下来分头审查手机端和服务端。".into(),
            thought: false,
        },
        completed_tool(
            "run-2",
            ToolKind::Execute,
            "cargo test -j 2 -p smelt-mobile",
        ),
        completed_tool("read-3", ToolKind::Read, "crates/smeltd/src/control.rs"),
        AcpEntry::Assistant {
            text: "问题集中在三处：忘记设备时没有撤销推送注册；同一台设备重新配对时旧令牌没清；\
                   控制请求只有单次超时，没有总时限。"
                .into(),
            thought: false,
        },
        AcpEntry::User("加一下".into()),
        thought("The user wants the fix now"),
        completed_tool("read-4", ToolKind::Read, "crates/smeltd/src/devices.rs"),
        completed_tool(
            "run-3",
            ToolKind::Execute,
            "git grep -n forget_device crates/",
        ),
        thought("Checking how forget_device reaches the relay"),
        AcpEntry::ToolCall {
            id: "run-4".into(),
            title: "cargo test -j 2 -p smeltd".into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
        },
    ];
    tail_snapshot(0, 0, entries, DaemonPhase::Thinking)
}

/// 等人回答的一轮：一道带说明的单选题和一道多选题，一张卡。
fn ask_snapshot() -> ConversationSnapshot {
    use smelt_plugin_api::model::acp_session::{
        ElicitFieldKindView, ElicitFieldView, ElicitOptionView, PendingElicitation,
    };
    let option = |label: &str, description: Option<&str>| ElicitOptionView {
        label: label.into(),
        description: description.map(Into::into),
        flag: None,
    };
    let mut snapshot = tail_snapshot(
        0,
        0,
        vec![
            AcpEntry::User("插件里的图片显示不出来，看看怎么修".into()),
            AcpEntry::Assistant {
                text: "要支持插件图片，有两种实质不同的改法，需要你定一下。".into(),
                thought: false,
            },
            AcpEntry::ToolCall {
                id: "ask-1".into(),
                title: "smelt_ask_user".into(),
                kind: ToolKind::Other,
                status: ToolCallStatus::InProgress,
                output: Vec::new(),
                children: Vec::new(),
                subagents: Vec::new(),
                peer: None,
            },
        ],
        DaemonPhase::WaitingForUser,
    );
    snapshot.pending_elicitation = Some(PendingElicitation {
        id: "ask".into(),
        message: String::new(),
        fields: vec![
            ElicitFieldView {
                key: "q0".into(),
                title:
                    "按哪个方案做？推荐第一种：同时修 GPUI Shell 的资源路径解析，现有写法不用改。"
                        .into(),
                required: true,
                allow_custom_input: true,
                kind: ElicitFieldKindView::Select(vec![
                    option(
                        "按推荐方案",
                        Some("修 GPUI Shell 和 Smelt，保留现有插件图片写法"),
                    ),
                    option("只改 Smelt", Some("新增 Smelt 专用的插件资源路径接口")),
                    option("先不改", None),
                ]),
            },
            ElicitFieldView {
                key: "q1".into(),
                title: "这次发版带上哪些改动？".into(),
                required: true,
                allow_custom_input: true,
                kind: ElicitFieldKindView::MultiSelect(vec![
                    option("提问卡焦点修复", None),
                    option("改动弹窗按内容定高", None),
                    option("纯文本不再画成代码块", None),
                ]),
            },
        ],
        chosen: [(0, vec![0]), (1, vec![0, 2])].into_iter().collect(),
        text_values: Default::default(),
    });
    snapshot
}

/// `ask`：画提问卡，带名字和卡片是否拿着键盘（高亮在第二行）。
fn render(
    snapshot: &ConversationSnapshot,
    theme: &str,
    suffix: &str,
    expanded: bool,
    ask: Option<(&str, bool)>,
    out: &Path,
) -> Vec<std::path::PathBuf> {
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(gpui_apple::AppleTextSystem::new()),
        Arc::new(smelt_plugin_api::ui::assets::Assets),
        || {
            Ok(Some(Box::new(
                gpui_apple::metal_renderer::MetalHeadlessRenderer::new()?,
            )))
        },
    );
    cx.update(|cx| {
        init_ui(cx);
        // 转圈之类的动画停在一帧上。
        cx.set_reduce_motion(true);
        assert!(
            smelt_plugin_api::apply_theme(theme, cx),
            "主题 {theme} 不在目录里"
        );
    });

    let slot: Rc<RefCell<Option<gpui::Entity<super::super::ConversationView>>>> = Rc::default();
    let window = {
        let slot = slot.clone();
        let snapshot = snapshot.clone();
        cx.open_window(size(px(WIDTH), px(HEIGHT)), move |window, cx| {
            let view = cx.new(|cx| {
                let mut view =
                    conversation_fixture("acp-snapshot", Vec::new(), DaemonPhase::Idle, cx);
                view.apply_snapshot(snapshot, cx);
                if expanded {
                    let layout = view.conversation_layout();
                    for item in layout.iter() {
                        if let Some(fold) = item.turn_fold {
                            view.expanded_turn_folds.insert(fold.first);
                        }
                        if let Some(group) = item.process_group {
                            view.expanded_process_groups.insert(group.expansion_key());
                        }
                    }
                }
                view
            });
            *slot.borrow_mut() = Some(view.clone());
            let stage = cx.new(|_| Stage { view });
            cx.new(|cx| gpui_base::Root::new(stage, window, cx))
        })
        .expect("开离屏窗口")
    };
    let view = slot.borrow().clone().expect("视图已建好");
    let draw = |cx: &mut HeadlessAppContext| {
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| {
                let _ = window.draw(cx);
            })
            .expect("重画");
        }
    };
    draw(&mut cx);

    if let Some((name, keyboard)) = ask {
        if keyboard {
            cx.update_window(window.into(), |_, window, cx| {
                view.update(cx, |view, cx| view.focus_elicit_row(1, window, cx));
            })
            .expect("给卡片焦点");
        }
        draw(&mut cx);
        let path = out.join(format!("{name}-{suffix}.png"));
        cx.capture_screenshot(window.into())
            .unwrap_or_else(|error| panic!("截图失败: {error:#}"))
            .save(&path)
            .unwrap_or_else(|error| panic!("写 {} 失败: {error}", path.display()));
        return vec![path];
    }

    let prompts: Vec<usize> = cx.update(|cx| {
        view.read(cx)
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| super::super::is_user_entry(entry))
            .map(|(ix, _)| ix)
            .collect()
    });
    let mode = if expanded { "expanded" } else { "collapsed" };
    let mut written = Vec::new();
    for (k, ix) in prompts.iter().copied().enumerate() {
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.list_state.scroll_to(gpui::ListOffset {
                    item_ix: ix,
                    offset_in_item: px(0.),
                });
                cx.notify();
            })
        });
        draw(&mut cx);
        let path = out.join(format!("{k:02}-{mode}-{suffix}.png"));
        cx.capture_screenshot(window.into())
            .unwrap_or_else(|error| panic!("截图失败: {error:#}"))
            .save(&path)
            .unwrap_or_else(|error| panic!("写 {} 失败: {error}", path.display()));
        written.push(path);
    }
    written
}

/// 对话视图自己不铺底，App 里底是外层面板给的；离屏窗口这里补上，不然底是黑的。
struct Stage {
    view: gpui::Entity<super::super::ConversationView>,
}

impl gpui::Render for Stage {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement as _, Styled as _};
        gpui::div()
            .size_full()
            .bg(gpui::rgb(ui_theme::bg()))
            .child(self.view.clone())
    }
}
