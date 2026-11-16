//! 组件快照：离屏把每个组件的各个状态画成 PNG，深浅两套主题各一张，给人和 agent
//! 读图核对。不做像素比对：不同机器、GPU 画出来本来就有出入，这里只管出图。
//!
//! ```sh
//! cargo test -p smelt-ui --test snapshots
//! SMELT_SNAPSHOT=button cargo test -p smelt-ui --test snapshots   # 只画名字里带 button 的
//! ```
//!
//! 图在 `target/tmp/snapshots/smelt-ui/<名字>-<light|dark>.png`，2 倍像素。
//!
//! 用 GPUI 的 `HeadlessAppContext`：调度是测试用的确定性调度，字形走真实的
//! CoreText，渲染走离屏 Metal，不开窗口也不抢焦点。只有 macOS 有离屏渲染器。
//!
//! 所有快照在一个测试里顺序画：`ui_theme` 的色板是进程级全局，并行跑深浅两套会互相盖掉。

#![cfg(target_os = "macos")]

use std::path::Path;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, HeadlessAppContext, IntoElement,
    ParentElement, Render, Styled, Window, div, px, rgb, size,
};
use gpui_component::input::{InputState, TextareaState};
use gpui_component::{IconName, Root, StyledExt as _};
use smelt_model::theme_index::{DEFAULT_DARK, DEFAULT_LIGHT};
use smelt_ui::{
    Button, Checkbox, IconAction as _, ListRow, Radio, Segmented, Switch, TextArea, TextInput,
    theme_catalog, ui_theme,
};

/// 每帧把整页画出来。闭包里可以引用 `setup` 时建好的实体（输入框状态等）。
type Draw = Box<dyn Fn(&mut Window, &mut App) -> AnyElement>;

struct Snapshot {
    name: &'static str,
    /// 窗口大小，逻辑像素。
    size: (f32, f32),
    /// 在窗口里建好页面要用的状态，返回画页面的闭包。
    setup: fn(&mut Window, &mut App) -> Draw,
}

const SNAPSHOTS: &[Snapshot] = &[
    Snapshot {
        name: "button",
        size: (600., 250.),
        setup: buttons,
    },
    Snapshot {
        name: "choice",
        size: (400., 180.),
        setup: choices,
    },
    Snapshot {
        name: "segmented",
        size: (400., 180.),
        setup: segmented,
    },
    Snapshot {
        name: "page-toolbar",
        size: (600., 200.),
        setup: page_toolbars,
    },
    Snapshot {
        name: "list-row",
        size: (680., 180.),
        setup: list_rows,
    },
    Snapshot {
        name: "field",
        size: (600., 260.),
        setup: fields,
    },
];

#[test]
fn snapshots() {
    let filter = std::env::var("SMELT_SNAPSHOT").unwrap_or_default();
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("snapshots/smelt-ui");
    std::fs::create_dir_all(&out).expect("建快照目录");

    let mut written = Vec::new();
    for snapshot in SNAPSHOTS
        .iter()
        .filter(|s| s.name.contains(filter.as_str()))
    {
        for (theme, suffix) in [(DEFAULT_LIGHT, "light"), (DEFAULT_DARK, "dark")] {
            let path = out.join(format!("{}-{suffix}.png", snapshot.name));
            render(snapshot, theme, &path);
            written.push(path);
        }
    }
    assert!(!written.is_empty(), "没有名字含「{filter}」的快照");
    for path in &written {
        println!("{}", path.display());
    }
}

fn render(snapshot: &Snapshot, theme: &str, path: &Path) {
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(gpui_apple::AppleTextSystem::new()),
        Arc::new(gpui_kit_assets::AllAssets),
        || {
            Ok(Some(Box::new(
                gpui_apple::metal_renderer::MetalHeadlessRenderer::new()?,
            )))
        },
    );
    cx.update(|cx| {
        gpui_component::init(cx);
        // 加载圈之类的动画停在一帧上，截出来每次一样。
        cx.set_reduce_motion(true);
        assert!(theme_catalog::apply(theme, cx), "主题 {theme} 不在目录里");
    });

    let (width, height) = snapshot.size;
    let setup = snapshot.setup;
    let window = cx
        .open_window(size(px(width), px(height)), |window, cx| {
            let draw = setup(window, cx);
            let stage = cx.new(|_| Stage { draw });
            cx.new(|cx| Root::new(stage, window, cx))
        })
        .expect("开离屏窗口");
    cx.run_until_parked();

    let image = cx
        .capture_screenshot(window.into())
        .unwrap_or_else(|error| panic!("{} 截图失败: {error:#}", snapshot.name));
    image
        .save(path)
        .unwrap_or_else(|error| panic!("写 {} 失败: {error}", path.display()));
}

struct Stage {
    draw: Draw,
}

impl Render for Stage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_6()
            .bg(rgb(ui_theme::bg()))
            .text_color(rgb(ui_theme::text()))
            .child((self.draw)(window, cx))
    }
}

/// 一行：左边是这一行在比什么，右边是并排的几个组件。
fn row(label: &'static str, items: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .text_sm()
                .text_color(rgb(ui_theme::muted()))
                .child(label),
        )
        .child(div().flex().items_center().gap_3().children(items))
        .into_any_element()
}

fn page(rows: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_5()
        .children(rows)
        .into_any_element()
}

fn buttons(_: &mut Window, _: &mut App) -> Draw {
    Box::new(|_, _| {
        page(vec![
            row(
                "角色",
                vec![
                    Button::new("primary")
                        .primary()
                        .label("发送")
                        .into_any_element(),
                    Button::new("secondary").label("次要").into_any_element(),
                    Button::new("quiet")
                        .quiet()
                        .label("取消")
                        .into_any_element(),
                    Button::new("danger")
                        .danger()
                        .label("删除")
                        .into_any_element(),
                    Button::new("danger-quiet")
                        .danger_quiet()
                        .label("停止")
                        .into_any_element(),
                ],
            ),
            row(
                "尺寸",
                vec![
                    Button::new("small")
                        .small()
                        .label("28 小")
                        .into_any_element(),
                    Button::new("medium").label("32 默认").into_any_element(),
                    Button::new("large")
                        .large()
                        .label("36 大")
                        .into_any_element(),
                ],
            ),
            row(
                "图标",
                vec![
                    Button::new("icon-label")
                        .icon(IconName::Plus)
                        .label("新建")
                        .into_any_element(),
                    Button::new("icon-only")
                        .quiet()
                        .icon(IconName::Globe)
                        .icon_action("打开网页")
                        .into_any_element(),
                    Button::new("dismiss")
                        .dismiss()
                        .icon(IconName::Close)
                        .icon_action("关闭")
                        .into_any_element(),
                    Button::new("floating")
                        .floating()
                        .icon(IconName::ArrowUp)
                        .icon_action("回到最新")
                        .into_any_element(),
                ],
            ),
            row(
                "状态",
                vec![
                    Button::new("disabled-primary")
                        .primary()
                        .label("禁用")
                        .disabled(true)
                        .into_any_element(),
                    Button::new("disabled")
                        .label("禁用")
                        .disabled(true)
                        .into_any_element(),
                    Button::new("loading")
                        .label("加载中")
                        .loading(true)
                        .into_any_element(),
                    Button::new("selected")
                        .quiet()
                        .label("选中")
                        .selected(true)
                        .into_any_element(),
                    // `toggled` 只给读屏；看得见的开着靠 `selected`，两个一起设。
                    Button::new("toggled")
                        .quiet()
                        .icon(IconName::Check)
                        .icon_action("已开启")
                        .selected(true)
                        .toggled(true)
                        .into_any_element(),
                    Button::new("caret")
                        .label("下拉")
                        .dropdown_caret(true)
                        .into_any_element(),
                ],
            ),
        ])
    })
}

fn choices(_: &mut Window, _: &mut App) -> Draw {
    Box::new(|_, _| {
        page(vec![
            row(
                "开关",
                vec![
                    Switch::new("switch-off").into_any_element(),
                    Switch::new("switch-on").checked(true).into_any_element(),
                    Switch::new("switch-off-disabled")
                        .disabled(true)
                        .into_any_element(),
                    Switch::new("switch-on-disabled")
                        .checked(true)
                        .disabled(true)
                        .into_any_element(),
                ],
            ),
            row(
                "复选框",
                vec![
                    Checkbox::new("check-off").label("没勾").into_any_element(),
                    Checkbox::new("check-on")
                        .checked(true)
                        .label("勾上")
                        .into_any_element(),
                    Checkbox::new("check-disabled")
                        .checked(true)
                        .disabled(true)
                        .label("禁用")
                        .into_any_element(),
                ],
            ),
            row(
                "单选",
                vec![
                    Radio::new("radio-off").label("没选").into_any_element(),
                    Radio::new("radio-on")
                        .checked(true)
                        .label("选中")
                        .into_any_element(),
                    Radio::new("radio-disabled")
                        .disabled(true)
                        .label("禁用")
                        .into_any_element(),
                ],
            ),
        ])
    })
}

fn segmented(_: &mut Window, _: &mut App) -> Draw {
    Box::new(|_, _| {
        page(vec![
            row(
                "单选",
                vec![
                    Segmented::new("single")
                        .segments(["对话", "终端", "文件"])
                        .selected(Some(1))
                        .on_select(|_, _, _| {})
                        .into_any_element(),
                ],
            ),
            row(
                "多选",
                vec![
                    Segmented::new("multiple")
                        .segments(["粗体", "斜体", "下划线"])
                        .multiple([true, false, true])
                        .on_select(|_, _, _| {})
                        .into_any_element(),
                ],
            ),
            row(
                "禁用",
                vec![
                    Segmented::new("disabled")
                        .segments(["浅色", "深色", "跟随系统"])
                        .selected(Some(2))
                        .disabled(true)
                        .into_any_element(),
                ],
            ),
        ])
    })
}

/// 页面工具行：分段控件、个数和中号按钮放在一行里，上下留得开；只有文字时也一样高。
fn page_toolbars(_: &mut Window, _: &mut App) -> Draw {
    Box::new(|_, _| {
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                smelt_ui::page_toolbar()
                    .gap_3()
                    .child(
                        Segmented::new("toolbar-tabs")
                            .segments(["自动化", "运行"])
                            .selected(Some(0))
                            .on_select(|_, _, _| {})
                            .into_any_element(),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("toolbar-add")
                            .icon(IconName::Plus)
                            .label("新建")
                            .into_any_element(),
                    ),
            )
            .child(
                smelt_ui::page_toolbar().child(
                    div()
                        .text_sm()
                        .text_color(rgb(ui_theme::muted()))
                        .child("3 个智能体"),
                ),
            )
            .into_any_element()
    })
}

fn list_rows(_: &mut Window, _: &mut App) -> Draw {
    Box::new(|_, _| {
        // 行尾的时间是附属信息：选中行里换成按选中底配过的那档次要字。
        let item = |id: ElementId, label: &'static str, selected: bool| {
            ListRow::new(id)
                .w(px(280.))
                .px_3()
                .py_2()
                .flex()
                .text_sm()
                .selected(selected)
                .child(
                    div()
                        .flex_1()
                        .when(selected, |name| name.font_medium())
                        .child(label),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(ui_theme::row_muted(selected)))
                        .child("3 分钟前"),
                )
                .on_click(|_, _, _| {})
        };
        // 左边压在实色窗口底上，右边压在一块彩色上，代替窗口材质：选中的底要透出它。
        let column = |key: &'static str, backdrop: u32| {
            let id = |row: &'static str| ElementId::Name(format!("{key}-{row}").into());
            div()
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .bg(rgb(backdrop))
                .child(item(id("plain"), "普通行", false))
                // 名字的字色三行一样；选中只换底，名字加中等字重（调用点给）。
                .child(item(id("selected"), "选中的行", true))
                .child(item(id("disabled"), "禁用的行", false).disabled(true))
        };
        div()
            .flex()
            .gap_4()
            .child(column("bg", ui_theme::bg()))
            .child(column("backdrop", ui_theme::chart(0)))
            .into_any_element()
    })
}

fn fields(window: &mut Window, cx: &mut App) -> Draw {
    let empty = cx.new(|cx| InputState::new(window, cx).placeholder("搜索会话"));
    let filled = cx.new(|cx| InputState::new(window, cx).default_value("smelt-ui 快照"));
    let small = cx.new(|cx| InputState::new(window, cx).placeholder("筛选"));
    let disabled = cx.new(|cx| InputState::new(window, cx).default_value("不能改"));
    let area = cx.new(|cx| {
        TextareaState::new(window, cx)
            .auto_grow(3, 6)
            .default_value("多行文本框\n第二行")
    });
    Box::new(move |_, _| {
        page(vec![
            row(
                "单行",
                vec![
                    TextInput::new(&empty).w(px(200.)).into_any_element(),
                    TextInput::new(&filled).w(px(200.)).into_any_element(),
                ],
            ),
            row(
                "小号、禁用",
                vec![
                    TextInput::new(&small)
                        .small()
                        .w(px(200.))
                        .into_any_element(),
                    TextInput::new(&disabled)
                        .disabled(true)
                        .w(px(200.))
                        .into_any_element(),
                ],
            ),
            row(
                "多行",
                vec![TextArea::new(&area).w(px(412.)).into_any_element()],
            ),
        ])
    })
}
