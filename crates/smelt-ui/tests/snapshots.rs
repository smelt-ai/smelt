//! 组件快照：离屏把每个组件的各个状态画成 PNG，每套主题一张，给人和 agent
//! 读图核对。不做像素比对：不同机器、GPU 画出来本来就有出入，这里只管出图。
//!
//! ```sh
//! cargo test -p smelt-ui --test snapshots
//! SMELT_SNAPSHOT=button cargo test -p smelt-ui --test snapshots   # 只画名字里带 button 的
//! ```
//!
//! 图在 `target/tmp/snapshots/smelt-ui/<名字>-<主题>.png`，2 倍像素。主题是默认深浅两套，
//! 加上只给种子色的 `seeded-*` 三套，看推导规则。
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
    KeyBinding, ParentElement, Render, Styled, Window, div, px, rgb, size,
};
use gpui_base::{Root, StyledExt as _};
use smelt_model::theme_index::{DEFAULT_DARK, DEFAULT_LIGHT};
use smelt_ui::IconName;
use smelt_ui::field::{InputState, TextareaState};
use smelt_ui::kit::{ControlSize, TYPE, TypeStyled as _};
use smelt_ui::{
    Button, IconAction as _, ListRow, Segmented, Switch, TextArea, TextInput, theme_catalog,
    tooltip::Tooltip, ui_theme,
};

gpui::actions!(tooltip_snapshot, [Save, Sequence]);

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
        size: (720., 300.),
        setup: buttons,
    },
    Snapshot {
        name: "switch",
        size: (400., 100.),
        setup: switches,
    },
    Snapshot {
        name: "tooltip",
        size: (500., 440.),
        setup: tooltips,
    },
    Snapshot {
        name: "button-icon-rem",
        size: (800., 160.),
        setup: button_icon_rem,
    },
    Snapshot {
        name: "button-content",
        size: (500., 160.),
        setup: button_content,
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
        size: (600., 360.),
        setup: fields,
    },
    Snapshot {
        name: "dialog",
        size: (600., 360.),
        setup: dialogs,
    },
    Snapshot {
        name: "menu",
        size: (520., 300.),
        setup: menus,
    },
    Snapshot {
        name: "menu-submenu",
        size: (520., 300.),
        setup: menu_submenu,
    },
    Snapshot {
        name: "menu-long",
        size: (420., 300.),
        setup: menu_long,
    },
    Snapshot {
        name: "menu-context",
        size: (420., 300.),
        setup: context_menu,
    },
    Snapshot {
        name: "settings",
        size: (640., 860.),
        setup: settings_page,
    },
    Snapshot {
        name: "settings-select",
        size: (640., 320.),
        setup: settings_select,
    },
    Snapshot {
        name: "popover",
        size: (480., 300.),
        setup: popover,
    },
    Snapshot {
        name: "combobox",
        size: (420., 400.),
        setup: combobox,
    },
    Snapshot {
        name: "keybind",
        size: (420., 120.),
        setup: keybinds,
    },
    Snapshot {
        name: "link",
        size: (420., 80.),
        setup: links,
    },
    Snapshot {
        name: "progress",
        size: (420., 140.),
        setup: progresses,
    },
    Snapshot {
        name: "slider",
        size: (420., 140.),
        setup: sliders,
    },
    Snapshot {
        name: "icon",
        size: (420., 120.),
        setup: icons,
    },
    Snapshot {
        name: "icon-set",
        size: (640., 460.),
        setup: icon_set,
    },
    Snapshot {
        name: "window-bar",
        size: (560., 320.),
        setup: window_bars,
    },
];

/// 只给种子色的主题：其余颜色和层级全靠推导。一浅两深，底色、强调色的色相和明度都错开，
/// 看推导出的分层、悬停、边线在别的种子上是否还站得住。
const SEEDED: &str = r##"{
  "version": 1,
  "name": "Seeded",
  "themes": [
    {
      "name": "Seeded Light",
      "mode": "light",
      "colors": {
        "bg_base": "#E5E9EF",
        "text_base": "#4C4F69",
        "bg_accent": "#7287FD",
        "info.fg": "#1E66F5",
        "success.fg": "#40A02B",
        "warning.fg": "#DF8E1D",
        "danger.fg": "#D20F39"
      }
    },
    {
      "name": "Seeded Dark",
      "mode": "dark",
      "colors": {
        "bg_base": "#1A1B26",
        "text_base": "#C0CAF5",
        "bg_accent": "#7AA2F7",
        "info.fg": "#7AA2F7",
        "success.fg": "#9ECE6A",
        "warning.fg": "#E0AF68",
        "danger.fg": "#F7768E"
      }
    },
    {
      "name": "Seeded Warm",
      "mode": "dark",
      "colors": {
        "bg_base": "#1D2021",
        "text_base": "#EBDBB2",
        "bg_accent": "#D79921",
        "info.fg": "#83A598",
        "success.fg": "#B8BB26",
        "warning.fg": "#FABD2F",
        "danger.fg": "#FB4934"
      }
    }
  ]
}"##;

#[test]
fn snapshots() {
    let filter = std::env::var("SMELT_SNAPSHOT").unwrap_or_default();
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("snapshots/smelt-ui");
    std::fs::create_dir_all(&out).expect("建快照目录");

    let themes = [
        (DEFAULT_LIGHT, "light"),
        (DEFAULT_DARK, "dark"),
        ("Seeded Light", "seeded-light"),
        ("Seeded Dark", "seeded-dark"),
        ("Seeded Warm", "seeded-warm"),
    ];
    let mut written = Vec::new();
    for snapshot in SNAPSHOTS
        .iter()
        .filter(|s| s.name.contains(filter.as_str()))
    {
        for (theme, suffix) in themes {
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
        Arc::new(smelt_ui::assets::Assets),
        || {
            Ok(Some(Box::new(
                gpui_apple::metal_renderer::MetalHeadlessRenderer::new()?,
            )))
        },
    );
    cx.update(|cx| {
        gpui_base::init(cx);
        smelt_ui::init(cx);
        // 加载圈之类的动画停在一帧上，截出来每次一样。
        cx.set_reduce_motion(true);
        theme_catalog::register_theme_sets("snapshots", &[SEEDED], cx).expect("登记种子主题");
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
        // 和应用外壳一样在根上设界面字体。
        div()
            .size_full()
            .p_6()
            .font_family(smelt_ui::kit::UI_FONT_FAMILY)
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
                .type_style(TYPE.ui)
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

/// 收进来的全部界面图标，16、14、12 三档字号各一组，看粗细和在小号下糊不糊。
fn icon_set(_: &mut Window, _: &mut App) -> Draw {
    Box::new(|_, _| {
        page(
            [16., 14., 12.]
                .map(|size| {
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_3()
                        .text_size(px(size))
                        .text_color(rgb(ui_theme::text()))
                        .children(IconName::ALL.iter().map(|icon| smelt_ui::Icon::new(*icon)))
                        .into_any_element()
                })
                .into(),
        )
    })
}

fn button_content(_: &mut Window, _: &mut App) -> Draw {
    Box::new(|_, _| {
        page(vec![row(
            "多行内容",
            vec![
                Button::new("two-lines")
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().line_height(px(20.)).child("子智能体"))
                            .child(div().line_height(px(20.)).child("正在处理任务")),
                    )
                    .into_any_element(),
            ],
        )])
    })
}

fn button_icon_rem(window: &mut Window, _: &mut App) -> Draw {
    window.set_rem_size(px(26.));
    Box::new(|_, _| {
        page(vec![row(
            "26px rem",
            vec![
                Button::new("rem-dismiss")
                    .dismiss()
                    .icon(IconName::Close)
                    .icon_action("关闭")
                    .into_any_element(),
                Button::new("rem-small-caret")
                    .small()
                    .label("小")
                    .dropdown_caret(true)
                    .into_any_element(),
                Button::new("rem-caret")
                    .label("默认")
                    .dropdown_caret(true)
                    .into_any_element(),
                Button::new("rem-icon")
                    .icon(IconName::Plus)
                    .label("按钮")
                    .into_any_element(),
                Button::new("rem-icon-only")
                    .small()
                    .icon(IconName::Plus)
                    .icon_action("新建")
                    .into_any_element(),
            ],
        )])
    })
}

fn tooltips(_: &mut Window, cx: &mut App) -> Draw {
    cx.bind_keys([
        KeyBinding::new("cmd-s", Save, None),
        KeyBinding::new("ctrl-k ctrl-s", Sequence, None),
    ]);
    Box::new(|window, cx| {
        page(vec![
            row(
                "文字",
                vec![
                    Tooltip::new("新建会话")
                        .build(window, cx)
                        .into_any_element(),
                ],
            ),
            row(
                "快捷键",
                vec![
                    Tooltip::new("保存")
                        .action(&Save, None)
                        .build(window, cx)
                        .into_any_element(),
                ],
            ),
            row(
                "多步",
                vec![
                    Tooltip::new("打开快捷键")
                        .action(&Sequence, None)
                        .build(window, cx)
                        .into_any_element(),
                ],
            ),
            row(
                "多行",
                vec![
                    Tooltip::new("第一行提示\n第二行说明")
                        .build(window, cx)
                        .into_any_element(),
                ],
            ),
            row(
                "指定按键",
                vec![
                    Tooltip::new("保存")
                        .key_binding(Some(vec![gpui::Keystroke::parse("cmd-s").unwrap()]))
                        .build(window, cx)
                        .into_any_element(),
                ],
            ),
            row(
                "自定义",
                vec![
                    Tooltip::element(|_, _| div().child("自定义提示"))
                        .max_w(px(160.))
                        .build(window, cx)
                        .into_any_element(),
                ],
            ),
        ])
    })
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
                        .label("24 小")
                        .into_any_element(),
                    Button::new("medium").label("28 默认").into_any_element(),
                    Button::new("large")
                        .large()
                        .label("36 大")
                        .into_any_element(),
                ],
            ),
            row(
                "五档",
                [
                    ("16", ControlSize::Mini),
                    ("20", ControlSize::Small),
                    ("24", ControlSize::Regular),
                    ("28", ControlSize::Large),
                    ("36", ControlSize::ExtraLarge),
                ]
                .into_iter()
                .flat_map(|(label, size)| {
                    [
                        Button::new(ElementId::Name(format!("size-{label}").into()))
                            .control_size(size)
                            .icon(IconName::Plus)
                            .label(label)
                            .into_any_element(),
                        Button::new(ElementId::Name(format!("icon-size-{label}").into()))
                            .control_size(size)
                            .icon(IconName::Plus)
                            .icon_action("新建")
                            .into_any_element(),
                    ]
                })
                .collect(),
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
                    Button::new("asset-icon")
                        .icon(smelt_ui::IconName::Plus)
                        .icon_action("资源图标")
                        .into_any_element(),
                    Button::new("path-icon")
                        .icon_path(smelt_ui::IconName::Plus.path())
                        .icon_action("自定义图标")
                        .into_any_element(),
                    // 纯图标主按钮走 submit 那组色（输入框的发送钮）。
                    Button::new("submit")
                        .primary()
                        .icon(IconName::ArrowUp)
                        .icon_action("发送")
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

fn switches(_: &mut Window, _: &mut App) -> Draw {
    Box::new(|_, _| {
        page(vec![row(
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
        )])
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
                        .type_style(TYPE.ui)
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
                .type_style(TYPE.ui)
                .selected(selected)
                .child(
                    div()
                        .flex_1()
                        .when(selected, |name| name.font_medium())
                        .child(label),
                )
                .child(
                    div()
                        .type_style(TYPE.text_xsmall)
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

fn dialogs(window: &mut Window, cx: &mut App) -> Draw {
    // 对话框画在窗口根的对话框层上，根这时还没建好：等这一轮建完再开。
    window.defer(cx, |window, cx| {
        smelt_ui::dialog::Confirm::new("删除会话？", "删除", |_, _| true)
            .danger()
            .description("这会从列表中移除会话。")
            .open(window, cx);
    });
    Box::new(|_, _| {
        page(vec![row(
            "背后",
            vec![div().child("对话框背后的内容").into_any_element()],
        )])
    })
}

/// 在 `position` 按下鼠标 `button`。
fn press(
    position: gpui::Point<gpui::Pixels>,
    button: gpui::MouseButton,
    window: &mut Window,
    cx: &mut App,
) {
    window.dispatch_event(
        gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            button,
            position,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
}

/// 下拉菜单快照共用的菜单：图标、彩色图标、勾选、子菜单、分组标题、禁用、危险项都有。
fn sample_menu() -> smelt_ui::menu::Menu {
    use smelt_ui::menu::{Menu, MenuItem};

    Menu::new()
        .item(MenuItem::new("复制路径").icon(IconName::Copy))
        .item(MenuItem::new("置顶").checked(true))
        .item(
            MenuItem::new("移到").submenu(
                Menu::new()
                    .item(MenuItem::new("工作").checked(true))
                    .item(MenuItem::new("个人").checked(false))
                    .separator()
                    .item(MenuItem::new("新建分组…"))
                    .item(MenuItem::new("用 Pi 打开").icon(IconName::Terminal).image(
                        std::sync::Arc::new(gpui::Image::from_bytes(
                            gpui::ImageFormat::Png,
                            include_bytes!("../../../assets/agent-pi.png").to_vec(),
                        )),
                    )),
            ),
        )
        .separator()
        .label("危险操作")
        .item(MenuItem::new("归档").disabled(true))
        .item(MenuItem::new("删除").icon(smelt_ui::trash_icon()).danger())
}

/// 触发按钮放在左上角固定的地方。
fn dropdown_page() -> AnyElement {
    div()
        .flex()
        .child(smelt_ui::menu::MenuTrigger::dropdown(
            "menu",
            Button::new("menu-trigger").label("更多").small(),
            |_, _| sample_menu(),
        ))
        .into_any_element()
}

/// 下拉菜单开着的样子：画好第一帧后在触发按钮上按下鼠标。
fn menus(window: &mut Window, cx: &mut App) -> Draw {
    window.defer(cx, |window, cx| {
        let _ = window.draw(cx);
        press(
            gpui::point(px(44.), px(38.)),
            gpui::MouseButton::Left,
            window,
            cx,
        );
    });
    Box::new(|_, _| dropdown_page())
}

/// 子菜单开着：打开下拉菜单后用键盘走到「移到」，再按右方向键进子菜单。
fn menu_submenu(window: &mut Window, cx: &mut App) -> Draw {
    use smelt_ui::menu::{Down, Right};

    window.defer(cx, |window, cx| {
        let _ = window.draw(cx);
        press(
            gpui::point(px(44.), px(38.)),
            gpui::MouseButton::Left,
            window,
            cx,
        );
        let _ = window.draw(cx);
        for _ in 0..3 {
            window.dispatch_action(Box::new(Down), cx);
        }
        window.dispatch_action(Box::new(Right), cx);
        // 子菜单按上一帧量到的行位置摆，多画两帧让它落到行旁边。
        let _ = window.draw(cx);
        let _ = window.draw(cx);
    });
    Box::new(|_, _| dropdown_page())
}

/// 项数超过面板最高高度（窗口高的一半）：行高不变，面板里滚动，不把每行挤扁；打开时
/// 打钩的那项滚到面板中间。
fn menu_long(window: &mut Window, cx: &mut App) -> Draw {
    use smelt_ui::menu::{Menu, MenuItem, MenuTrigger};

    window.defer(cx, |window, cx| {
        let _ = window.draw(cx);
        press(
            gpui::point(px(44.), px(38.)),
            gpui::MouseButton::Left,
            window,
            cx,
        );
    });
    Box::new(|_, _| {
        div()
            .flex()
            .child(MenuTrigger::dropdown(
                "menu-long",
                Button::new("menu-long-trigger").label("模型").small(),
                |_, _| {
                    (1..=20).fold(Menu::new().label("模型"), |menu, ix| {
                        menu.item(MenuItem::new(format!("Model-{ix}")).checked(ix == 15))
                    })
                },
            ))
            .into_any_element()
    })
}

/// 右键菜单：在靠下的地方按右键，下面放不下，菜单翻到按下处的上面。键盘停在最后一项
/// 危险项上，看危险色压在高亮底上的样子；它上面是禁用的危险项。
fn context_menu(window: &mut Window, cx: &mut App) -> Draw {
    use smelt_ui::menu::{Menu, MenuItem, MenuTrigger, Up};

    window.defer(cx, |window, cx| {
        let _ = window.draw(cx);
        press(
            gpui::point(px(160.), px(250.)),
            gpui::MouseButton::Right,
            window,
            cx,
        );
        let _ = window.draw(cx);
        window.dispatch_action(Box::new(Up), cx);
    });
    Box::new(|_, _| {
        MenuTrigger::context(
            "context-menu",
            div()
                .size_full()
                .rounded(px(8.))
                .border_1()
                .border_dashed()
                .border_color(ui_theme::hair_strong())
                .p_3()
                .type_style(TYPE.ui)
                .text_color(rgb(ui_theme::muted()))
                .child("在这里按右键"),
            |_, _| {
                Menu::new()
                    .item(MenuItem::new("恢复"))
                    .item(MenuItem::new("复制文件路径").icon(IconName::Copy))
                    .separator()
                    .item(
                        MenuItem::new("移除（有自动化在用）")
                            .icon(smelt_ui::trash_icon())
                            .danger()
                            .disabled(true),
                    )
                    .item(
                        MenuItem::new("彻底删除")
                            .icon(smelt_ui::trash_icon())
                            .danger(),
                    )
            },
        )
        .size_full()
        .into_any_element()
    })
}

fn fields(window: &mut Window, cx: &mut App) -> Draw {
    let empty = cx.new(|cx| InputState::new(window, cx).placeholder("搜索会话"));
    let filled = cx.new(|cx| InputState::new(window, cx).default_value("smelt-ui 快照"));
    let small = cx.new(|cx| InputState::new(window, cx).placeholder("筛选"));
    let disabled = cx.new(|cx| InputState::new(window, cx).default_value("不能改"));
    let search = cx.new(|cx| InputState::new(window, cx).default_value("筛选内容"));
    let area = cx.new(|cx| {
        TextareaState::new(window, cx)
            .auto_grow(3, 6)
            .default_value("多行文本框\n第二行")
    });
    let bare = cx.new(|cx| {
        TextareaState::new(window, cx)
            .auto_grow(2, 6)
            .placeholder("想让它做什么？")
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
                "前缀、清空",
                vec![
                    TextInput::new(&search)
                        .prefix(
                            gpui::svg()
                                .path(smelt_ui::IconName::Search.path())
                                .size_4()
                                .flex_shrink_0()
                                .text_color(rgb(ui_theme::muted())),
                        )
                        .cleanable(true)
                        .w(px(200.))
                        .into_any_element(),
                ],
            ),
            row(
                "多行",
                vec![TextArea::new(&area).w(px(412.)).into_any_element()],
            ),
            row(
                "无框",
                vec![
                    div()
                        .w(px(412.))
                        .px_2()
                        .rounded(px(16.))
                        .border_1()
                        .border_color(ui_theme::hair())
                        .bg(rgb(ui_theme::surface()))
                        .child(TextArea::new(&bare).bare())
                        .into_any_element(),
                ],
            ),
        ])
    })
}

/// 设置页里出现的几种行：下拉、按钮组、开关、多行说明、输入框，和不是行的自由块。
fn settings_page(window: &mut Window, cx: &mut App) -> Draw {
    use smelt_ui::{Select, SettingsPage, SettingsRow, SettingsSection};

    let relay = cx.new(|cx| InputState::new(window, cx).placeholder("https://relay.example.com"));
    Box::new(move |_, _| {
        SettingsPage::new("settings")
            .child(
                SettingsSection::new()
                    .title("界面")
                    .child(
                        SettingsRow::new("主题")
                            .description("界面和终端配色一起换，明暗跟着主题走")
                            .child(
                                Select::new("theme")
                                    .options([
                                        ("smelt".to_string(), "Smelt Light".into()),
                                        ("ayu".to_string(), "Ayu Dark".into()),
                                    ])
                                    .selected(Some("smelt".to_string())),
                            ),
                    )
                    .child(
                        SettingsRow::new("字号")
                            .description("8–32px")
                            .child(Button::new("smaller").small().label("−"))
                            .child(
                                div()
                                    .w(px(40.))
                                    .text_center()
                                    .type_style(TYPE.ui)
                                    .child("14px"),
                            )
                            .child(Button::new("larger").small().label("+")),
                    )
                    .child(
                        SettingsRow::new("响铃")
                            .description(
                                "终端里的程序响铃时发一条系统通知\n窗口在前台、正看着这个终端时不发",
                            )
                            .child(Switch::new("bell").checked(true)),
                    )
                    .child(SettingsRow::new("恢复布局").child(
                        Button::new("restore").small().label("恢复"),
                    )),
            )
            .child(
                SettingsSection::new()
                    .title("让其他设备连进来")
                    .description("手机和别的电脑经 relay 连到这台电脑上的运行时。")
                    .child(
                        div()
                            .type_style(TYPE.ui)
                            .text_color(rgb(ui_theme::muted()))
                            .child("还没有配对的设备。"),
                    )
                    .child(
                        SettingsRow::new("自建 relay")
                            .description("留空用默认的公共 relay")
                            .child(TextInput::new(&relay).w(px(220.))),
                    ),
            )
            .child(
                SettingsSection::new().title("已安装").child(
                    SettingsRow::new("示例技能")
                        .description(
                            "一段很长的说明，用来看收起的样子：第一行写满，第二行也写满，\
                             第三行开始应该被省略号截掉，悬停时才看得到全文。\n\
                             第二段：这里的字不应该出现在收起后的行里。",
                        )
                        .clamp_description("long-description", 2)
                        .note("~/.agents/skills/example · 示例智能体 在用\n附注第二行不受收起影响。")
                        .child(Button::new("delete").quiet().small().label("删除")),
                ).child(
                    SettingsRow::new("会话列表")
                        .description("smelt.session-list · 0.1.0\n~/.smelt/plugins/session-list")
                        .error(
                            "你的版本没装上，正在用自带版本。插件用到了这版 Smelt 没有的 SDK 接口\
                             （SmeltContextMenu、SmeltMenu），需要更新 Smelt",
                        )
                        .child(Button::new("restore-default").quiet().small().label("恢复默认…")),
                ),
            )
            .into_any_element()
    })
}

/// 下拉开着的样子：画好第一帧后在下拉按钮上按下鼠标。
fn settings_select(window: &mut Window, cx: &mut App) -> Draw {
    use smelt_ui::{Select, SettingsPage, SettingsRow, SettingsSection};

    window.defer(cx, |window, cx| {
        let _ = window.draw(cx);
        // 按钮在第一行的右边，大约 (511..575, 93..116)。
        press(
            gpui::point(px(545.), px(105.)),
            gpui::MouseButton::Left,
            window,
            cx,
        );
    });
    Box::new(|_, _| {
        SettingsPage::new("settings-select")
            .child(
                SettingsSection::new().title("背景").child(
                    SettingsRow::new("材质")
                        .description("垫在整个窗口下面的系统毛玻璃")
                        .child(
                            Select::new("material")
                                .options([
                                    (0, "内容区".into()),
                                    (1, "侧栏".into()),
                                    (2, "窗口底".into()),
                                    (3, "菜单".into()),
                                ])
                                .selected(Some(1)),
                        ),
                ),
            )
            .into_any_element()
    })
}

/// 弹出卡片开着：画好第一帧后在触发按钮上按下鼠标。触发按钮跟着显示「开着」。
fn popover(window: &mut Window, cx: &mut App) -> Draw {
    window.defer(cx, |window, cx| {
        let _ = window.draw(cx);
        press(
            gpui::point(px(44.), px(38.)),
            gpui::MouseButton::Left,
            window,
            cx,
        );
    });
    Box::new(|_, _| {
        div()
            .flex()
            .child(
                smelt_ui::popover::Popover::new("popover")
                    .w(px(280.))
                    .trigger(Button::new("popover-trigger").label("能力").small())
                    .content(|_, _, _| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().type_style(TYPE.ui_medium).child("这个对话能用的能力"))
                            .child(
                                div()
                                    .type_style(TYPE.text_xsmall)
                                    .text_color(rgb(ui_theme::muted()))
                                    .child("关掉的能力，智能体在这个对话里就用不了。"),
                            )
                            .child(div().flex().flex_col().children(
                                ["读写文件", "运行命令", "联网搜索"].map(|label| {
                                    ListRow::new(label)
                                        .px_2()
                                        .py_1()
                                        .type_style(TYPE.ui)
                                        .child(label)
                                }),
                            ))
                    }),
            )
            .into_any_element()
    })
}

/// 主题选择那样分组、带色点的组合框开着：按下按钮打开，再按一次 ↓，高亮离开打勾的当前项。
/// GitHub Light 是禁用项。
fn combobox(window: &mut Window, cx: &mut App) -> Draw {
    use smelt_ui::{Combobox, ComboboxGroup, ComboboxOption};

    window.defer(cx, |window, cx| {
        let _ = window.draw(cx);
        press(
            gpui::point(px(44.), px(36.)),
            gpui::MouseButton::Left,
            window,
            cx,
        );
        let _ = window.draw(cx);
        window.dispatch_action(Box::new(smelt_ui::combobox::Next), cx);
    });
    let theme = |id: &'static str, label: &'static str, colors: [u32; 3]| {
        ComboboxOption::new(id, label).prefix(move || {
            div().flex().gap_1().children(colors.map(|color| {
                div()
                    .size(px(10.))
                    .rounded_full()
                    .border_1()
                    .border_color(ui_theme::hair_strong())
                    .bg(rgb(color))
            }))
        })
    };
    let groups = std::rc::Rc::new(vec![
        ComboboxGroup::new(
            Some("Smelt".into()),
            vec![
                theme("smelt-dark", "Smelt Dark", [0x1e1e1d, 0xe8e6e3, 0x6b9eff]),
                theme("smelt-light", "Smelt Light", [0xfbfaf8, 0x1f1f1e, 0x2f6fe4]),
            ],
        ),
        ComboboxGroup::new(
            Some("GitHub".into()),
            vec![
                theme("github-dark", "GitHub Dark", [0x0d1117, 0xe6edf3, 0x2f81f7]),
                theme(
                    "github-light",
                    "GitHub Light",
                    [0xffffff, 0x1f2328, 0x0969da],
                )
                .disabled(true),
            ],
        ),
        ComboboxGroup::new(
            Some("Solarized".into()),
            vec![
                theme(
                    "solarized-dark",
                    "Solarized Dark",
                    [0x002b36, 0x839496, 0x268bd2],
                ),
                theme(
                    "solarized-light",
                    "Solarized Light",
                    [0xfdf6e3, 0x657b83, 0x268bd2],
                ),
            ],
        ),
    ]);
    Box::new(move |_, _| {
        div()
            .flex()
            .child(
                Combobox::new("theme", groups.clone())
                    .selected(Some("smelt-dark"))
                    .anchor(gpui::Anchor::TopLeft),
            )
            .into_any_element()
    })
}

fn keybinds(_: &mut Window, _: &mut App) -> Draw {
    use smelt_ui::Keybind;
    fn key(spec: &'static str) -> gpui::Keystroke {
        gpui::Keystroke::parse(spec).unwrap()
    }
    Box::new(|_, _| {
        page(vec![
            row(
                "一步",
                vec![
                    Keybind::new([key("cmd-s")]).into_any_element(),
                    Keybind::new([key("cmd-shift-p")]).into_any_element(),
                    Keybind::new([key("enter")]).into_any_element(),
                ],
            ),
            row(
                "多步 / 卡片上",
                vec![
                    Keybind::new([key("cmd-k"), key("cmd-s")]).into_any_element(),
                    div()
                        .p_2()
                        .rounded_md()
                        .bg(rgb(ui_theme::surface()))
                        .child(Keybind::new([key("ctrl-alt-t")]).backdrop(ui_theme::surface()))
                        .into_any_element(),
                ],
            ),
        ])
    })
}

fn links(_: &mut Window, _: &mut App) -> Draw {
    use smelt_ui::Link;
    Box::new(|_, _| {
        page(vec![row(
            "链接",
            vec![
                Link::new("docs", "https://example.com")
                    .type_style(TYPE.ui)
                    .child("帮助文档 ↗")
                    .into_any_element(),
                Link::new("source", "https://example.com")
                    .type_style(TYPE.ui)
                    .child("GitHub ↗")
                    .into_any_element(),
            ],
        )])
    })
}

fn progresses(_: &mut Window, _: &mut App) -> Draw {
    use smelt_ui::Progress;
    Box::new(|_, _| {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(240.))
            .child(Progress::new("zero").value(0.))
            .child(Progress::new("half").value(45.))
            .child(Progress::new("full").value(100.))
            .child(Progress::new("thin").value(60.).w(px(120.)).h(px(5.)))
            .child(Progress::new("unknown").indeterminate(true))
            .into_any_element()
    })
}

fn sliders(_: &mut Window, cx: &mut App) -> Draw {
    use smelt_ui::slider::{Slider, SliderState};
    let low = cx.new(|_| SliderState::new().min(0.).max(100.).default_value(15.));
    let high = cx.new(|_| {
        SliderState::new()
            .min(10.)
            .max(20.)
            .step(1.)
            .default_value(17.)
    });
    Box::new(move |_, _| {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(200.))
            .child(Slider::new(&low))
            .child(Slider::new(&high))
            .child(Slider::new(&high).disabled(true))
            .into_any_element()
    })
}

fn icons(_: &mut Window, _: &mut App) -> Draw {
    use smelt_ui::Icon;
    Box::new(|_, _| {
        page(vec![
            row(
                "跟字",
                vec![
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .type_style(TYPE.ui)
                        .text_color(rgb(ui_theme::muted()))
                        .child(Icon::new(IconName::Folder))
                        .child("跟着字号和字色")
                        .into_any_element(),
                ],
            ),
            row(
                "指定",
                vec![
                    Icon::new(IconName::Check).size_3().into_any_element(),
                    Icon::new(IconName::Check)
                        .size_4()
                        .text_color(rgb(ui_theme::ok()))
                        .into_any_element(),
                    Icon::new(IconName::Trash).size_4().into_any_element(),
                    Icon::new(IconName::ChevronRight)
                        .size_4()
                        .rotate(gpui::radians(std::f32::consts::FRAC_PI_2))
                        .into_any_element(),
                ],
            ),
        ])
    })
}

fn window_bars(_: &mut Window, _: &mut App) -> Draw {
    use gpui::{Corners, size};
    use smelt_ui::window_bar::{
        BAR_MARGIN, SafeArea, TOP_BAR_HEIGHT, TRAFFIC_LIGHTS_WIDTH, WINDOW_CORNER_INSET,
        WINDOW_EDGE_MARGIN, window_bar,
    };
    // 一块视图的顶：角上被宿主控件占的地方画成淡底，看顶带让得对不对。
    fn stage(id: &'static str, area: SafeArea, title: &'static str) -> AnyElement {
        let corner = |width: gpui::Pixels| {
            div()
                .absolute()
                .top_0()
                .w(width)
                .h(TOP_BAR_HEIGHT)
                .bg(rgb(ui_theme::soft()))
        };
        div()
            .relative()
            .w_full()
            .border_1()
            .border_color(ui_theme::hair())
            .child(corner(area.corners.top_left.width).left_0())
            .child(corner(area.corners.top_right.width).right_0())
            .child(
                // 不再设横向内边距：那是顶带按角让出来的。
                window_bar(id, &area)
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .justify_center()
                            .type_style(TYPE.ui)
                            .text_color(rgb(ui_theme::muted()))
                            .child(title),
                    )
                    .child(Button::new("zoom").quiet().small().icon(IconName::Maximize)),
            )
            .into_any_element()
    }
    // 贴底的一条（侧栏底下的设置）：整个视图就是一行，按下面的角让。
    fn bottom_row(area: SafeArea, title: &'static str) -> AnyElement {
        let insets = area.bar_insets();
        let corner = |width: gpui::Pixels| {
            div()
                .absolute()
                .bottom_0()
                .w(width)
                .h(width)
                .bg(rgb(ui_theme::soft()))
        };
        div()
            .relative()
            .w_full()
            .h(TOP_BAR_HEIGHT)
            .border_1()
            .border_color(ui_theme::hair())
            .child(corner(area.corners.bottom_left.width).left_0())
            .child(corner(area.corners.bottom_right.width).right_0())
            .child(
                div()
                    .size_full()
                    .pl(insets.left)
                    .pr(insets.right)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        Button::new("settings")
                            .quiet()
                            .icon(IconName::Settings)
                            .label(title),
                    )
                    .child(Button::new("more").quiet().small().icon(IconName::Maximize)),
            )
            .into_any_element()
    }
    Box::new(|_, _| {
        // 和外壳给的一样：左上是红黄绿 + 左开合按钮 + 间距，右上和它对称。
        let toggle = px(20.);
        let lights = SafeArea {
            title_bar: true,
            corners: Corners {
                top_left: size(TRAFFIC_LIGHTS_WIDTH + toggle + BAR_MARGIN, TOP_BAR_HEIGHT),
                top_right: size(WINDOW_EDGE_MARGIN + toggle + BAR_MARGIN, TOP_BAR_HEIGHT),
                ..Default::default()
            },
        };
        let rounded = size(WINDOW_CORNER_INSET, WINDOW_CORNER_INSET);
        let bottom = SafeArea {
            corners: Corners {
                bottom_left: rounded,
                bottom_right: rounded,
                ..Default::default()
            },
            ..Default::default()
        };
        let fullscreen = SafeArea {
            title_bar: true,
            ..Default::default()
        };
        page(vec![
            stage("bar-corners", lights, "贴左右上角"),
            stage("bar-plain", fullscreen, "全屏 / 不贴角"),
            stage(
                "bar-left",
                lights.child(gpui::Edges {
                    top: true,
                    left: true,
                    bottom: true,
                    right: false,
                }),
                "横排左边那格",
            ),
            bottom_row(bottom, "贴左右下角"),
            bottom_row(SafeArea::default(), "不贴角"),
        ])
    })
}
