//! 设置页：外观（主题、字号与字体、窗口与背景）。

use super::*;
use gpui_component::IndexPath;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use smelt_ui::theme_catalog::ThemeSwatch;
use std::collections::HashMap;

pub(super) fn appearance_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    SettingPage::new("外观")
        .description("主题、字号与字体，以及窗口透明度和背景。")
        .group(theme_group())
        .group(typography_group(snapshot, cx))
        .group(window_group(entity, snapshot, cx))
}

fn theme_group() -> SettingGroup {
    SettingGroup::new().title("主题").item(
        SettingItem::new(
            "主题",
            SettingField::render(|_, window, cx: &mut App| {
                let picker =
                    window.use_keyed_state("settings-theme-picker", cx, ThemePicker::new);
                let select = picker.read(cx).select.clone();
                let border = cx.theme().border;
                // 别处换了主题（命令面板等）时，下拉跟着对上，不停在旧的那项。只对目录里
                // 有的名字同步：选不中的名字每次重绘都会再选一遍，停不下来。
                let current = SharedString::from(cx.global::<Appearance>().theme_name.clone());
                let swatch = picker.read(cx).swatches.get(&current).copied();
                if swatch.is_some() && select.read(cx).selected_value() != Some(&current) {
                    select.update(cx, |select, cx| {
                        select.set_selected_value(&current, window, cx)
                    });
                }
                h_flex()
                    .items_center()
                    .gap_2()
                    .children(swatch.map(|swatch| theme_dots(swatch, border)))
                    .child(
                        Select::new(&select)
                            .w(px(220.))
                            .menu_width(px(300.))
                            .search_placeholder("搜索主题，如 Ayu、Catppuccin"),
                    )
            }),
        )
        .description("换主题后界面和终端配色一起变，界面明暗跟着这款主题走。"),
    )
}

/// 主题下拉：可搜索，每项带三个色点和所属系列。挂在设置窗口的元素状态上，窗口关了就丢。
struct ThemePicker {
    select: Entity<SelectState<SearchableVec<ThemeOption>>>,
    /// 每款主题的色点，建下拉时一次算好：每项都要把主题配置套一遍，不放进重绘。
    swatches: HashMap<SharedString, ThemeSwatch>,
    _confirm: Subscription,
}

impl ThemePicker {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current = cx.global::<Appearance>().theme_name.clone();
        let options = smelt_model::theme_index::entries()
            .iter()
            .map(|entry| ThemeOption {
                name: entry.name.clone().into(),
                family: entry.family.clone().into(),
                swatch: smelt_ui::theme_catalog::swatch(&entry.name, cx),
            })
            .collect::<Vec<_>>();
        let selected = options
            .iter()
            .position(|option| *option.name == *current)
            .map(IndexPath::new);
        let swatches = options
            .iter()
            .filter_map(|option| Some((option.name.clone(), option.swatch?)))
            .collect();
        let select = cx.new(|cx| {
            SelectState::new(SearchableVec::new(options), selected, window, cx).searchable(true)
        });
        let _confirm = cx.subscribe(
            &select,
            |_, _, event: &SelectEvent<SearchableVec<ThemeOption>>, cx| {
                if let SelectEvent::Confirm(Some(name)) = event {
                    select_theme(name.to_string(), cx);
                }
            },
        );
        Self {
            select,
            swatches,
            _confirm,
        }
    }
}

#[derive(Clone)]
struct ThemeOption {
    name: SharedString,
    family: SharedString,
    swatch: Option<ThemeSwatch>,
}

impl SelectItem for ThemeOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_2()
            .children(self.swatch.map(|swatch| theme_dots(swatch, border)))
            .child(div().flex_1().min_w_0().truncate().child(self.name.clone()))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(self.family.clone()),
            )
    }

    fn value(&self) -> &SharedString {
        &self.name
    }

    /// 名字或系列（Ayu、Catppuccin）都能搜。
    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.name.to_lowercase().contains(&query) || self.family.to_lowercase().contains(&query)
    }
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
        ])
}

/// 背景、前景、强调三个色点。描边用当前主题的边框色，深色主题的底色点在深色界面上也看得见。
fn theme_dots(swatch: ThemeSwatch, border: gpui::Hsla) -> impl IntoElement {
    h_flex().flex_none().items_center().gap_1().children(
        [swatch.background, swatch.foreground, swatch.accent].map(|color| {
            div()
                .size(px(10.))
                .rounded_full()
                .bg(color)
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
