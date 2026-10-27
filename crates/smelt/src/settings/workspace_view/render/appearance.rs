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
        .group(interface_group(snapshot))
        .group(terminal_group(snapshot))
        .group(window_group(entity, snapshot, cx))
}

/// 界面：主题、界面字号、界面字体。
fn interface_group(snapshot: &SettingsRenderSnapshot) -> SettingGroup {
    let ui_font_size_slider = snapshot.ui_font_size_slider.clone();
    SettingGroup::new().title("界面").items(vec![
        theme_item(),
        SettingItem::new(
            "界面字号",
            SettingField::render(move |_, _, cx: &mut App| {
                let size = cx.global::<Appearance>().ui_font_px;
                slider_with_value(ui_font_size_slider.clone(), format!("{size}px"), cx)
            }),
        )
        .description("默认 16px"),
        // 值本身写着「默认（系统）」，不再另配说明。
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
        ),
    ])
}

/// 终端与代码：终端字号、代码字体。
fn terminal_group(snapshot: &SettingsRenderSnapshot) -> SettingGroup {
    let font_size_slider = snapshot.font_size_slider.clone();
    SettingGroup::new().title("终端与代码").items(vec![
        SettingItem::new(
            "终端字号",
            SettingField::render(move |_, _, cx: &mut App| {
                let size = cx.global::<Appearance>().font_px;
                slider_with_value(font_size_slider.clone(), format!("{size}px"), cx)
            }),
        )
        .description("默认 14px"),
        SettingItem::new(
            "代码字体",
            SettingField::scrollable_dropdown(
                snapshot.font_options.as_ref().clone(),
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
        .description("终端、diff 和代码块共用，建议选等宽；缺字时回落内嵌的 Maple Mono NF"),
    ])
}

/// 滑块加右侧数值。数值框留够「100%」的宽度并右对齐，几行的数字对得齐。
fn slider_with_value(
    slider: Option<Entity<SliderState>>,
    value: String,
    cx: &App,
) -> impl IntoElement + use<> {
    h_flex()
        .items_center()
        .gap_2()
        .child(div().w(px(200.)).children(slider.as_ref().map(Slider::new)))
        .child(
            div()
                .w(px(40.))
                .text_right()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(value),
        )
}

fn theme_item() -> SettingItem {
    SettingItem::new(
        "主题",
        SettingField::render(|_, window, cx: &mut App| {
            let picker = window.use_keyed_state("settings-theme-picker", cx, ThemePicker::new);
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
    .description("界面和终端配色一起换，明暗跟着主题走")
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
    let appearance = cx.global::<Appearance>();
    let opacity_slider = snapshot.opacity_slider.clone();
    let mut items = vec![
        SettingItem::new(
            "不透明度",
            SettingField::render(move |_, _, cx: &mut App| {
                let opacity = (cx.global::<Appearance>().window_opacity() * 100.0).round() as u32;
                slider_with_value(opacity_slider.clone(), format!("{opacity}%"), cx)
            }),
        )
        .description("只作用于工作台窗口，默认 95%；设置窗口始终不透明"),
    ];
    // 系统玻璃是 macOS 26 的标题栏材质，别的平台没有这个东西。
    #[cfg(target_os = "macos")]
    items.push(
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
        .description("macOS 26 起；系统开了「减少透明度」时不生效"),
    );
    let bg_color_picker = snapshot.bg_color_picker.clone();
    items.push(
        SettingItem::new(
            "背景色",
            SettingField::render(move |_, _, cx: &mut App| {
                // 没改过时写「跟随主题」：出厂值会跟着明暗自动换（见 bg_color_is_default）。
                let appearance = cx.global::<Appearance>();
                let label = if appearance.bg_color_is_default() {
                    "跟随主题".to_string()
                } else {
                    format!("#{:06x}", appearance.bg_color)
                };
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                    .children(
                        bg_color_picker
                            .as_ref()
                            .map(|p| ColorPicker::new(p).small()),
                    )
            }),
        )
        .description("会话内容区的底色"),
    );
    // 背景图：没设时只给「选择图片…」；设了才出文件名、「清除」和透明度。
    let has_image = appearance.bg_image.is_some();
    let pick_entity = entity.clone();
    let clear_entity = entity;
    items.push(
        SettingItem::new(
            "背景图片",
            SettingField::render(move |_, _, cx: &mut App| {
                let name = cx
                    .global::<Appearance>()
                    .bg_image
                    .as_deref()
                    .and_then(|path| path.rsplit('/').next())
                    .map(str::to_string);
                let pick_entity = pick_entity.clone();
                let clear_entity = clear_entity.clone();
                h_flex()
                    .items_center()
                    .gap_2()
                    .children(name.map(|name| {
                        div()
                            .max_w(px(160.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_middle()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(name)
                    }))
                    .child(
                        Button::new("pick-img")
                            .compact()
                            .label(if has_image {
                                "更换…"
                            } else {
                                "选择图片…"
                            })
                            .flex_shrink_0()
                            .on_click(move |_, _window, cx: &mut App| {
                                pick_entity.update(cx, |this, cx| this.pick_bg_image(cx));
                            }),
                    )
                    .when(has_image, |row| {
                        row.child(
                            Button::new("clear-img")
                                .quiet()
                                .compact()
                                .label("清除")
                                .flex_shrink_0()
                                .on_click(move |_, _window, cx: &mut App| {
                                    clear_entity.update(cx, |this, cx| this.set_bg_image(None, cx));
                                }),
                        )
                    })
            }),
        )
        .description("会话内容区，终端和对话共用"),
    );
    if has_image {
        let bg_image_opacity_slider = snapshot.bg_image_opacity_slider.clone();
        items.push(
            SettingItem::new(
                "背景图透明度",
                SettingField::render(move |_, _, cx: &mut App| {
                    let opacity =
                        (cx.global::<Appearance>().bg_image_opacity * 100.0).round() as u32;
                    slider_with_value(bg_image_opacity_slider.clone(), format!("{opacity}%"), cx)
                }),
            )
            .description("默认 25%，越低越不抢文字"),
        );
    }
    SettingGroup::new().title("窗口").items(items)
}
