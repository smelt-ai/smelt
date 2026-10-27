//! 设置页：外观、主题与终端（主题、字号与字体、窗口与背景）。

use super::*;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};

pub(super) fn appearance_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    SettingPage::new("外观、主题与终端")
        .description("界面主题、字号、终端字体，以及窗口透明度和背景。")
        .group(theme_group(cx))
        .group(typography_group(snapshot, cx))
        .group(window_group(entity, snapshot, cx))
}

fn theme_group(cx: &App) -> SettingGroup {
    let border = cx.theme().border;
    SettingGroup::new()
        .title("主题")
        .description("选定主题后，按该主题的明暗同步界面模式。共 38 款内置主题。")
        .item(
            SettingItem::new(
                "主题",
                SettingField::render(move |_, window, cx: &mut App| {
                    let current = cx.global::<Appearance>().theme_name.clone();
                    let swatch = smelt_ui::theme_catalog::swatch(&current);
                    let search =
                        window.use_keyed_state("settings-theme-search", cx, |window, cx| {
                            InputState::new(window, cx).placeholder("搜索主题，如 Ayu、Catppuccin")
                        });
                    let search_for_menu = search.clone();
                    let current_for_menu = current.clone();
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(theme_dots(swatch, border))
                        .child(Input::new(&search).w(px(168.)).small())
                        .child(
                            Button::new("settings-theme")
                                .outline()
                                .compact()
                                .label(current.clone())
                                .dropdown_caret(true)
                                .max_w(px(220.))
                                .overflow_hidden()
                                .dropdown_menu_with_anchor(
                                    gpui::Anchor::TopRight,
                                    move |menu, _, cx| {
                                        let query =
                                            search_for_menu.read(cx).value().trim().to_lowercase();
                                        let matches = smelt_model::theme_index::entries()
                                            .iter()
                                            .filter(|entry| {
                                                query.is_empty()
                                                    || entry.name.to_lowercase().contains(&query)
                                                    || entry.family.to_lowercase().contains(&query)
                                            })
                                            .cloned()
                                            .collect::<Vec<_>>();
                                        if matches.is_empty() {
                                            return menu.scrollable(true).item(
                                                PopupMenuItem::new("没有匹配的主题").disabled(true),
                                            );
                                        }
                                        let current = current_for_menu.clone();
                                        matches.into_iter().fold(
                                            menu.scrollable(true).max_h(px(280.)).min_w(px(280.)),
                                            |menu, entry| {
                                                let name = entry.name.clone();
                                                let family = entry.family.clone();
                                                let swatch = smelt_ui::theme_catalog::swatch(&name);
                                                let selected = name == current;
                                                let item_name = name.clone();
                                                menu.item(
                                                    PopupMenuItem::element(move |_, cx| {
                                                        let muted = cx.theme().muted_foreground;
                                                        let border = cx.theme().border;
                                                        h_flex()
                                                            .w_full()
                                                            .min_w(px(240.))
                                                            .items_center()
                                                            .gap_2()
                                                            .child(theme_dots(swatch, border))
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .min_w_0()
                                                                    .overflow_hidden()
                                                                    .text_ellipsis()
                                                                    .whitespace_nowrap()
                                                                    .child(item_name.clone()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .flex_shrink_0()
                                                                    .text_xs()
                                                                    .text_color(muted)
                                                                    .child(family.clone()),
                                                            )
                                                    })
                                                    .checked(selected)
                                                    .on_click(move |_, _, cx| {
                                                        select_theme(name.clone(), cx);
                                                    }),
                                                )
                                            },
                                        )
                                    },
                                ),
                        )
                }),
            )
            .description("Default 是库自带的默认外观。换主题后，组件和自绘界面一起变"),
        )
}

fn select_theme(name: String, cx: &mut App) {
    let Some(entry) = smelt_model::theme_index::find(&name) else {
        return;
    };
    let mode = if entry.mode == "light" {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    apply_appearance(
        move |appearance| {
            appearance.theme_name = name;
            appearance.theme_mode = mode;
        },
        cx,
    );
    let selected = cx.global::<Appearance>().theme_name.clone();
    apply_named_theme(&selected, cx);
    cx.refresh_windows();
}

fn typography_group(snapshot: &SettingsRenderSnapshot, cx: &App) -> SettingGroup {
    let muted = cx.theme().muted_foreground;
    let ui_font_size_slider = snapshot.ui_font_size_slider.clone();
    let font_size_slider = snapshot.font_size_slider.clone();
    let font_options = snapshot.font_options.as_ref().clone();

    SettingGroup::new()
        .title("字号与字体")
        .description("界面和终端的字号、字体在同一页对照调整。")
        .items(vec![
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
                .max_w(px(220.))
                .overflow_hidden(),
            )
            .description(concat!(
                "终端、Git diff、代码块的等宽字体，跟界面字体分开。",
                "建议选等宽，图标缺字回落内嵌默认（Maple Mono NF）",
            )),
            SettingItem::render(|_, _, cx: &mut App| typography_preview(cx)),
        ])
}

fn typography_preview(cx: &App) -> gpui::AnyElement {
    let appearance = cx.global::<Appearance>();
    let swatch = smelt_ui::theme_catalog::swatch(&appearance.theme_name).unwrap_or(
        smelt_ui::theme_catalog::ThemeSwatch {
            background: 0x101113,
            foreground: 0xeceae6,
            accent: 0xe09f5a,
            terminal: 0x17191c,
        },
    );
    let ui_px = appearance.ui_font_px;
    let term_px = appearance.font_px;
    let ui_font = resolved_ui_font_family(appearance);
    let mono = terminal_view::font_family();
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let ink = rgb(swatch.foreground);
    let accent = rgb(swatch.accent);

    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child("实时字号与配色预览"),
        )
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(border)
                .bg(rgb(swatch.background))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_shrink_0()
                                .px_1p5()
                                .py_0p5()
                                .rounded(cx.theme().radius)
                                .bg(accent)
                                .text_color(rgb(swatch.background))
                                .text_size(px((ui_px.saturating_sub(4).max(10)) as f32))
                                .font_family(ui_font.clone())
                                .child(format!("UI {ui_px}px")),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_color(ink)
                                .text_size(px(ui_px as f32))
                                .font_family(ui_font)
                                .child("对话气泡与界面排版预览"),
                        ),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(border)
                        .bg(rgb(swatch.terminal))
                        .text_color(ink)
                        .text_size(px(term_px as f32))
                        .font_family(mono)
                        .child(div().text_color(accent).child("$"))
                        .child("cargo check --workspace")
                        .child(div().text_color(rgb(crate::ui_theme::green())).child("✓")),
                ),
        )
        .into_any_element()
}

fn theme_dots(
    swatch: Option<smelt_ui::theme_catalog::ThemeSwatch>,
    border: gpui::Hsla,
) -> impl IntoElement {
    let colors = swatch.map(smelt_ui::theme_catalog::ThemeSwatch::colors);
    h_flex().items_center().gap_1().children(
        colors
            .unwrap_or([0x808080, 0xeceae6, 0xe09f5a, 0x17191c])
            .map(|color| {
                div()
                    .size(px(10.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(rgb(color))
                    .border_1()
                    .border_color(border)
            }),
    )
}

fn window_group(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingGroup {
    let muted = cx.theme().muted_foreground;
    let bg_color_picker = snapshot.bg_color_picker.clone();
    let opacity_slider = snapshot.opacity_slider.clone();
    let bg_image_opacity_slider = snapshot.bg_image_opacity_slider.clone();
    let pick_entity = entity.clone();
    let clear_entity = entity;

    SettingGroup::new()
        .title("窗口与背景")
        .description("窗口透明度、标题栏玻璃，以及会话内容区的背景色和背景图。")
        .items(vec![
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
                                    Button::new("pick-img")
                                        .compact()
                                        .label("选择图片…")
                                        .flex_shrink_0()
                                        .on_click(move |_, _window, cx: &mut App| {
                                            pick_entity
                                                .update(cx, |this, cx| this.pick_bg_image(cx));
                                        }),
                                )
                                .child(
                                    Button::new("clear-img")
                                        .quiet()
                                        .compact()
                                        .label("清除")
                                        .flex_shrink_0()
                                        .on_click(move |_, _window, cx: &mut App| {
                                            clear_entity
                                                .update(cx, |this, cx| this.set_bg_image(None, cx));
                                        }),
                                ),
                        )
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div().w(px(200.)).children(
                                        bg_image_opacity_slider.as_ref().map(Slider::new),
                                    ),
                                )
                                .child(div().w(px(40.)).text_xs().text_color(muted).child(
                                    format!(
                                        "{}%",
                                        (cx.global::<Appearance>().bg_image_opacity * 100.0).round()
                                            as u32
                                    ),
                                )),
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
                    let opacity =
                        (cx.global::<Appearance>().window_opacity() * 100.0).round() as u32;
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
            .description("控制工作台窗口的透明度（默认 95%）；设置窗口为保证阅读始终保持不透明"),
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
        ])
}
