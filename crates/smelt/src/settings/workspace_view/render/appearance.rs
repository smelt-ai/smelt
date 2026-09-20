//! 设置页：外观（主题、终端字体、窗口与背景）。

use super::*;

pub(super) fn theme_page(
    _entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let muted = cx.theme().muted_foreground;
    let ui_font_size_slider = snapshot.ui_font_size_slider.clone();

    SettingPage::new("主题与界面")
        .description("界面主题、字号和字体；终端字体在「终端」里单独设置。")
        .group(SettingGroup::new().items(vec![
                SettingItem::new(
                    "主题模式",
                    SettingField::switch(
                        |cx: &App| cx.global::<Appearance>().theme_mode.is_dark(),
                        |v: bool, cx: &mut App| {
                            let mode = if v { ThemeMode::Dark } else { ThemeMode::Light };
                            apply_appearance(|a| a.theme_mode = mode, cx);
                            apply_theme_mode(mode, cx);
                            // 色板是进程级全局态（见 ui_theme），改完不重绘就还是旧色。
                            cx.refresh_windows();
                        },
                    )
                    .default_value(true),
                )
                .description("开启为深色主题，关闭为浅色主题"),
                SettingItem::new(
                    "界面字号",
                    SettingField::render(move |_, _, cx: &mut App| {
                        let size = cx.global::<Appearance>().ui_font_px;
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(200.))
                                    .children(ui_font_size_slider.as_ref().map(Slider::new)),
                            )
                            .child(
                                div()
                                    .w(px(32.))
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{size}px")),
                            )
                    }),
                )
                .description("控制侧栏、面板、对话区等整体界面的字号缩放基准（默认 16px）"),
                SettingItem::new(
                    "界面字体",
                    SettingField::scrollable_dropdown(
                        snapshot.ui_font_options.as_ref().clone(),
                        |cx: &App| cx.global::<Appearance>().ui_font_family.clone().into(),
                        |v: SharedString, cx: &mut App| {
                            let name = v.trim().to_string();
                            apply_appearance(move |a| a.ui_font_family = name, cx);
                            cx.refresh_windows();
                        },
                    )
                    .max_w(px(220.))
                    .overflow_hidden(),
                )
                .description(
                    "侧栏、按钮、对话正文的字体；默认用系统界面字体（macOS 为 SF Pro + PingFang）",
                ),
            ]))
}

pub(super) fn terminal_page(
    _entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let muted = cx.theme().muted_foreground;
    let font_size_slider = snapshot.font_size_slider.clone();
    // 代码字体下拉的选项：内嵌默认置顶（值为空 = 用默认），其后按字母序列出系统
    // 已装的全部字体族。不做等宽过滤——系统没有可靠的「是否等宽」元数据，漏判
    // 误判都更糟；选了非等宽的后果只是难看，fallback 链保证不会渲染错乱。
    let font_options = snapshot.font_options.as_ref().clone();

    SettingPage::new("终端")
        .description("终端字符网格、输出和代码块使用的字号与字体。")
        .group(SettingGroup::new().items(vec![
                SettingItem::new(
                    "终端字号",
                    SettingField::render(move |_, _, cx: &mut App| {
                        let size = cx.global::<Appearance>().font_px;
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(200.))
                                    .children(font_size_slider.as_ref().map(Slider::new)),
                            )
                            .child(
                                div()
                                    .w(px(32.))
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{size}px")),
                            )
                    }),
                )
                .description("控制终端字符网格与输出文本大小（默认 14px）"),
                SettingItem::new(
                    "代码字体",
                    SettingField::scrollable_dropdown(
                        font_options,
                        |cx: &App| cx.global::<Appearance>().font_family.clone().into(),
                        |v: SharedString, cx: &mut App| {
                            let name = v.trim().to_string();
                            terminal_view::set_font_family(&name);
                            apply_appearance(move |a| a.font_family = name, cx);
                            cx.refresh_windows();
                        },
                    )
                    // 系统里总有名字长得离谱的字体，选中后同样会顶爆按钮，这里封顶兜住。
                    .max_w(px(220.))
                    .overflow_hidden(),
                )
                .description(concat!(
                    "终端、Git diff、代码块的等宽字体，跟界面字体分开。",
                    "建议选等宽，图标缺字回落内嵌默认（Maple Mono NF）",
                )),
            ]))
}

pub(super) fn window_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let (fg, muted, border, popover) = {
        let t = cx.theme();
        (t.foreground, t.muted_foreground, t.border, t.popover)
    };
    let btn_base = move |id: &'static str, label: String| {
        div()
            .id(id)
            .h(px(26.))
            .px_3()
            .flex()
            .flex_none()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .text_color(fg)
            .bg(popover)
            .border_1()
            .border_color(border)
            .child(label)
    };
    let btn = move |id: &'static str, label: String| btn_base(id, label).hover(|s| s.bg(border));
    let bg_color_picker = snapshot.bg_color_picker.clone();
    let opacity_slider = snapshot.opacity_slider.clone();
    let bg_image_opacity_slider = snapshot.bg_image_opacity_slider.clone();
    let pick_entity = entity.clone();
    let clear_entity = entity;

    SettingPage::new("窗口与背景")
        .description("窗口透明度、标题栏玻璃，以及会话内容区的背景色和背景图。")
        .group(SettingGroup::new().items(vec![
                SettingItem::new(
                    "背景色",
                    SettingField::render(move |_, _, _| {
                        div().children(
                            bg_color_picker
                                .as_ref()
                                .map(|p| ColorPicker::new(p).small()),
                        )
                    }),
                ),
                SettingItem::new(
                    "背景图片",
                    SettingField::render(move |_, _, cx: &mut App| {
                        let img_name = cx
                            .global::<Appearance>()
                            .bg_image
                            .as_deref()
                            .and_then(|p| p.rsplit('/').next())
                            .unwrap_or("无")
                            .to_string();
                        let pick_entity = pick_entity.clone();
                        let clear_entity = clear_entity.clone();
                        let bg_image_opacity_slider = bg_image_opacity_slider.clone();
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        // 文件名长度不可控，必须自己封顶：SettingItem 外层是
                                        // overflow_hidden，撑爆的部分不会换行，只会把右边的按钮
                                        // 顶出可视区，导致「选择图片…／清除」点都点不到。
                                        // 中间省略号保留开头和扩展名，比末尾截断更容易认出是哪张图。
                                        div()
                                            .max_w(px(140.))
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis_middle()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(img_name),
                                    )
                                    .child(
                                        btn("pick-img", "选择图片…".into())
                                            .flex_shrink_0()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                move |_, _window, cx: &mut App| {
                                                    pick_entity.update(cx, |this, cx| {
                                                        this.pick_bg_image(cx)
                                                    });
                                                },
                                            ),
                                    )
                                    .child(
                                        btn("clear-img", "清除".into())
                                            .flex_shrink_0()
                                            .text_color(muted)
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                move |_, _window, cx: &mut App| {
                                                    clear_entity.update(cx, |this, cx| {
                                                        this.set_bg_image(None, cx)
                                                    });
                                                },
                                            ),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .w(px(200.))
                                            .children(
                                                bg_image_opacity_slider.as_ref().map(Slider::new),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .w(px(40.))
                                            .text_xs()
                                            .text_color(muted)
                                            .child(format!(
                                                "{}%",
                                                (cx.global::<Appearance>().bg_image_opacity
                                                    * 100.0)
                                                    .round() as u32
                                            )),
                                    ),
                            )
                    }),
                )
                .description(
                    "背景图片在会话内容区显示（终端与 ACP 共用）；透明度默认 25%，\
                     越低越透、越不抢文字",
                ),
                SettingItem::new(
                    "不透明度",
                    SettingField::render(move |_, _, cx: &mut App| {
                        let opacity = (cx.global::<Appearance>().window_opacity() * 100.0)
                            .round() as u32;
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(200.))
                                    .children(opacity_slider.as_ref().map(Slider::new)),
                            )
                            .child(
                                div()
                                    .w(px(40.))
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{opacity}%")),
                            )
                    }),
                )
                .description(
                    "控制工作台窗口的透明度（默认 95%）；设置窗口为保证阅读始终保持不透明",
                ),
                SettingItem::new(
                    "标题栏玻璃",
                    SettingField::dropdown(
                        vec![
                            ("regular".into(), "标准".into()),
                            ("clear".into(), "清透".into()),
                        ],
                        |cx: &App| match cx.global::<Appearance>().glass_style {
                            liquid_glass::GlassStyle::Regular => "regular".into(),
                            liquid_glass::GlassStyle::Clear => "clear".into(),
                        },
                        |v: SharedString, cx: &mut App| {
                            let style = if v == "clear" {
                                liquid_glass::GlassStyle::Clear
                            } else {
                                liquid_glass::GlassStyle::Regular
                            };
                            apply_appearance(move |a| a.glass_style = style, cx);
                            cx.refresh_windows();
                        },
                    ),
                )
                .description(
                    "macOS 26+ 顶部导航的系统玻璃：标准 / 清透（折射更明显）。\
                     只作用于标题栏，内容区保持实色；系统开启「减少透明度」时自动关掉",
                ),
            ]))
}
