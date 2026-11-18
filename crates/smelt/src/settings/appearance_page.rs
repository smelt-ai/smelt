//! 设置页：外观（主题、字号与字体、布局、背景）。宿主自己的页，和插件的设置页一样是登记
//! 的视图（见 [`super::view_specs`]）。

use rust_i18n::t;
use std::rc::Rc;

use super::*;
use smelt_model::font_config;
use smelt_plugin_api::HostEvent;
use smelt_ui::kit::{TYPE, TypeStyled as _};
use smelt_ui::theme_catalog::ThemeSwatch;
use smelt_ui::{
    Combobox, ComboboxGroup, ComboboxOption, Select, SettingsPage, SettingsRow as Row,
    SettingsSection as Section,
};

/// 下拉的选项：值、显示的名字。
type Options = Rc<Vec<(SharedString, SharedString)>>;

pub(super) struct AppearancePage {
    /// 弱引用：「恢复布局」要回工作台做。
    workspace: WeakEntity<Workspace>,
    ui_font_size: Entity<SliderState>,
    bg_opacity: Entity<SliderState>,
    bg_image_opacity: Entity<SliderState>,
    /// 本机装的字体，代码字体和界面字体共用；「默认（…）」那项在 render 里现取，跟着语言变。
    /// 建这页时枚举一次——mac 上要逐个字体 face 取族名，一次约 50ms，不能每次刷新都算。
    fonts: Options,
    /// 主题选择的选项。要解析每款主题的颜色，只在主题目录变了时重算。
    themes: Rc<Vec<ComboboxGroup>>,
    _subscriptions: Vec<Subscription>,
}

impl AppearancePage {
    pub(super) fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let appearance = cx.global::<Appearance>().clone();
        let ui_font_size = cx.new(|_| {
            SliderState::new()
                .min(MIN_UI_FONT_PX as f32)
                .max(MAX_UI_FONT_PX as f32)
                .step(1.0)
                .default_value(appearance.ui_font_px as f32)
        });
        let bg_opacity = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .step(1.0)
                .default_value(appearance.bg_opacity.clamp(0.0, 1.0) * 100.0)
        });
        let bg_image_opacity = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(MAX_BG_IMAGE_OPACITY * 100.0)
                .step(1.0)
                .default_value(appearance.bg_image_opacity.clamp(0.0, MAX_BG_IMAGE_OPACITY) * 100.0)
        });
        let subscriptions = vec![
            cx.subscribe(&ui_font_size, |_, _, event: &SliderEvent, cx| {
                if let Some(value) = slider_value(event) {
                    let size = value
                        .round()
                        .clamp(MIN_UI_FONT_PX as f32, MAX_UI_FONT_PX as f32)
                        as u32;
                    apply_appearance(move |a| a.ui_font_px = size, cx);
                    cx.refresh_windows();
                }
            }),
            // 配色按窗口底算，拖的时候每一档都重装一次主题，松手前就看得到字色跟着变。
            cx.subscribe(&bg_opacity, |_, _, event: &SliderEvent, cx| {
                if let Some(value) = slider_value(event) {
                    set_bg_opacity(value / 100.0, cx);
                }
            }),
            cx.subscribe(&bg_image_opacity, |_, _, event: &SliderEvent, cx| {
                if let Some(value) = slider_value(event) {
                    let opacity = (value / 100.0).clamp(0.0, MAX_BG_IMAGE_OPACITY);
                    apply_appearance(move |a| a.bg_image_opacity = opacity, cx);
                    cx.refresh_windows();
                }
            }),
            cx.observe_global::<Appearance>(|_, cx| cx.notify()),
            // 插件登记、撤下主题时，主题选择的选项跟着变。
            cx.observe_global::<smelt_ui::theme_catalog::ThemeCatalog>(|this, cx| {
                this.themes = Rc::new(theme_options(cx));
                cx.notify();
            }),
            // 首页的布局登记、撤下了，布局的选项跟着变。
            cx.subscribe(
                &smelt_plugin_api::events(cx),
                |_, _, event: &HostEvent, cx| {
                    if matches!(event, HostEvent::LayoutsChanged | HostEvent::PagesChanged) {
                        cx.notify();
                    }
                },
            ),
        ];

        let mut names = cx.text_system().all_font_names();
        names.sort();
        names.dedup();
        let installed = names
            .into_iter()
            .map(|name| (SharedString::from(name.clone()), SharedString::from(name)));

        Self {
            workspace,
            ui_font_size,
            bg_opacity,
            bg_image_opacity,
            fonts: Rc::new(installed.collect()),
            themes: Rc::new(theme_options(cx)),
            _subscriptions: subscriptions,
        }
    }

    /// 界面：主题、界面字号、界面字体。
    fn interface_section(&self, cx: &App) -> Section {
        let appearance = cx.global::<Appearance>();
        let size = appearance.ui_font_px;
        Section::new()
            .title(t!("appearance.interface"))
            .child(
                Row::new(t!("appearance.theme"))
                    .description(t!("appearance.theme_hint"))
                    .child(
                        Combobox::new("settings-theme", self.themes.clone())
                            .selected(Some(displayed_theme_name(cx)))
                            .on_change(|name, _, cx| select_theme(name.to_string(), cx)),
                    ),
            )
            .child(
                Row::new(t!("appearance.ui_font_size"))
                    .description(t!("appearance.ui_font_size_hint"))
                    .child(slider_with_value(&self.ui_font_size, format!("{size}px"))),
            )
            // 值本身写着「默认（Inter）」，不再另配说明。
            .child(
                Row::new(t!("appearance.ui_font")).child(
                    Select::new("settings-ui-font")
                        .options(font_options(smelt_ui::kit::UI_FONT_FAMILY, &self.fonts))
                        .selected(Some(SharedString::from(
                            appearance.ui_font_family.clone(),
                        )))
                        .on_change(|name: &SharedString, _, cx| {
                            let name = name.trim().to_string();
                            apply_appearance(move |a| a.ui_font_family = name, cx);
                            cx.refresh_windows();
                        }),
                ),
            )
    }

    /// 代码字体：终端、diff、代码块共用。终端字号在终端插件自己的设置页。
    fn code_font_section(&self, cx: &App) -> Section {
        let current = cx.global::<Appearance>().font_family.clone();
        Section::new().title(t!("appearance.code")).child(
            Row::new(t!("appearance.code_font"))
                .description(t!("appearance.code_font_hint"))
                .child(
                    Select::new("settings-code-font")
                        .options(font_options(font_config::DEFAULT_FONT_FAMILY, &self.fonts))
                        .selected(Some(SharedString::from(current)))
                        .on_change(|name: &SharedString, _, cx| {
                            let name = name.trim().to_string();
                            smelt_ui::set_code_font(&name, cx);
                            apply_appearance(move |a| a.font_family = name, cx);
                            cx.refresh_windows();
                        }),
                ),
        )
    }

    fn background_section(&self, cx: &App) -> Section {
        let appearance = cx.global::<Appearance>();
        let mut section = Section::new().title(t!("appearance.background"));
        // 没有材质的机器（Linux、更早的 Windows）窗口不透明，不给调。哪几块透、透哪种材质
        // 由布局说，这里只调透多少。
        if crate::window_backdrop::available() {
            let opacity = (appearance.bg_opacity * 100.0).round() as u32;
            section = section.child(
                Row::new(t!("appearance.glass"))
                    .description(t!("appearance.glass_hint"))
                    .child(slider_with_value(&self.bg_opacity, format!("{opacity}%"))),
            );
        }
        // 背景图：没设时只给「选择图片…」；设了才出文件名、「清除」和透明度。
        let name = appearance
            .bg_image
            .as_deref()
            .and_then(|path| path.rsplit('/').next())
            .map(str::to_string);
        let has_image = name.is_some();
        section = section.child(
            Row::new(t!("appearance.image"))
                .description(t!("appearance.image_hint"))
                .children(name.map(|name| {
                    div()
                        .max_w(px(160.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_middle()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(crate::ui_theme::muted()))
                        .child(name)
                }))
                .child(
                    Button::new("pick-img")
                        .small()
                        .label(if has_image {
                            t!("appearance.change_image")
                        } else {
                            t!("appearance.choose_image")
                        })
                        .on_click(|_, _, cx: &mut App| pick_bg_image(cx)),
                )
                .when(has_image, |row| {
                    row.child(
                        Button::new("clear-img")
                            .quiet()
                            .small()
                            .label(t!("action.clear"))
                            .on_click(|_, _, cx: &mut App| set_bg_image(None, cx)),
                    )
                }),
        );
        if has_image {
            let opacity = (appearance.bg_image_opacity * 100.0).round() as u32;
            section = section.child(
                Row::new(t!("appearance.image_opacity"))
                    .description(t!("appearance.image_opacity_hint"))
                    .child(slider_with_value(
                        &self.bg_image_opacity,
                        format!("{opacity}%"),
                    )),
            );
        }
        section
    }
}

impl Render for AppearancePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        SettingsPage::new("settings-page:appearance")
            .child(self.interface_section(cx))
            .child(self.code_font_section(cx))
            .child(layout_section(self.workspace.clone(), cx))
            .child(self.background_section(cx))
    }
}

impl smelt_plugin_api::PluginView for AppearancePage {
    fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

fn slider_value(event: &SliderEvent) -> Option<f32> {
    let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
    match value {
        SliderValue::Single(value) => Some(*value),
        _ => None,
    }
}

/// 设置、清除背景图。背景图画在窗口根上，重绘时跟着换。
fn set_bg_image(path: Option<String>, cx: &mut App) {
    apply_appearance(|a| a.bg_image = path, cx);
    cx.refresh_windows();
}

/// 弹系统的选择框选一张背景图。
fn pick_bg_image(cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(t!("appearance.choose_image_prompt").into()),
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = paths.await
            && let Some(path) = paths
                .into_iter()
                .next()
                .and_then(|path| path.to_str().map(String::from))
        {
            cx.update(|cx| set_bg_image(Some(path), cx));
        }
    })
    .detach();
}

/// 布局：首页用哪份，和把它恢复成本来的样子。选项是插件给首页登记的布局（只读内存）；
/// 别的页（如设置）各用登记给它的那份，不在这里选。
fn layout_section(workspace: WeakEntity<Workspace>, cx: &App) -> Section {
    let options: Vec<(SharedString, SharedString)> = crate::workspace_layout::home_layouts(cx)
        .into_iter()
        .map(|layout| (layout.id, layout.title))
        .collect();
    let section = Section::new().title(t!("appearance.layout"));
    if options.is_empty() {
        // 兜底的摆法不存档，没有可恢复的。
        return section.child(
            Row::new(t!("appearance.layout"))
                .description(t!("appearance.layout_none_hint"))
                .child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(crate::ui_theme::muted()))
                        .child(t!("appearance.no_layouts")),
                ),
        );
    }
    // 选的布局暂时没登记时显示实际在用的那份，不改保存的选择。
    let current =
        SharedString::from(crate::workspace_layout::effective_home_layout(cx).unwrap_or_default());
    section
        .child(
            Row::new(t!("appearance.layout"))
                .description(t!("appearance.layout_hint"))
                .child(
                    Select::new("settings-layout")
                        .options(options)
                        .selected(Some(current))
                        .on_change(|id: &SharedString, _, cx| {
                            let id = id.to_string();
                            // 工作台盯着外观里的这一项，变了自己换布局。
                            apply_appearance(move |a| a.layout_id = Some(id), cx);
                        }),
                ),
        )
        .child(
            Row::new(t!("appearance.reset_layout"))
                .description(t!("appearance.reset_layout_hint"))
                .child(
                    Button::new("restore-layout")
                        .small()
                        .label(t!("action.reset"))
                        .on_click(move |_, _, cx: &mut App| {
                            let _ =
                                workspace.update(cx, |workspace, cx| workspace.restore_layout(cx));
                        }),
                ),
        )
}

/// 滑块加右侧数值。数值框留够「100%」的宽度并右对齐，几行的数字对得齐。
fn slider_with_value(slider: &Entity<SliderState>, value: String) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().w(px(200.)).child(Slider::new(slider)))
        .child(
            div()
                .w(px(40.))
                .text_right()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(crate::ui_theme::muted()))
                .child(value),
        )
}

/// 字体下拉的选项：打头一项是「默认（出厂字体）」，值为空。
fn font_options(
    default: &str,
    fonts: &Options,
) -> impl Iterator<Item = (SharedString, SharedString)> {
    std::iter::once((
        SharedString::default(),
        SharedString::from(t!("appearance.default_font", name = default)),
    ))
    .chain(fonts.iter().cloned())
}

/// 主题选择的选项：每个系列一组，系列按目录里第一次出现的先后排（库自带的在前）；每款
/// 前面是它的三个色点，按系列名、明暗也搜得到。要解析每款主题的颜色，只在主题目录变了时
/// 算（见 [`AppearancePage`]），不在 render 里算。
fn theme_options(cx: &App) -> Vec<ComboboxGroup> {
    let mut families: Vec<(SharedString, Vec<ComboboxOption>)> = Vec::new();
    for theme in smelt_ui::theme_catalog::themes(cx) {
        let mode = if theme.mode.is_dark() {
            ["深色", "dark"]
        } else {
            ["浅色", "light"]
        };
        let mut option = ComboboxOption::new(theme.name.clone(), theme.name.clone()).keywords([
            theme.family.clone(),
            mode[0].into(),
            mode[1].into(),
        ]);
        if let Some(swatch) = smelt_ui::theme_catalog::swatch(&theme.name, cx) {
            option = option.prefix(move || theme_dots(swatch));
        }
        match families
            .iter_mut()
            .find(|(family, _)| *family == theme.family)
        {
            Some((_, options)) => options.push(option),
            None => families.push((theme.family, vec![option])),
        }
    }
    families
        .into_iter()
        .map(|(family, options)| ComboboxGroup::new(Some(family), options))
        .collect()
}

/// 主题暂时不可用时显示实际生效的 Default，不覆盖保存的用户选择。
fn displayed_theme_name(cx: &App) -> String {
    let appearance = cx.global::<Appearance>();
    if smelt_ui::theme_catalog::contains(&appearance.theme_name, cx) {
        appearance.theme_name.clone()
    } else {
        smelt_model::theme_index::default_for_mode(if appearance.theme_mode.is_dark() {
            "dark"
        } else {
            "light"
        })
        .to_string()
    }
}

fn select_theme(name: String, cx: &mut App) {
    let Some(entry) = smelt_ui::theme_catalog::themes(cx)
        .into_iter()
        .find(|entry| entry.name.as_ref() == name)
    else {
        return;
    };
    let mode = if !entry.mode.is_dark() {
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

/// 背景、前景、强调三个色点。描边用 `hair_strong`，深色主题的底色点在深色界面上也看得见。
fn theme_dots(swatch: ThemeSwatch) -> Div {
    div().flex().items_center().flex_none().gap_1().children(
        [swatch.background, swatch.foreground, swatch.accent].map(|color| {
            div()
                .size(px(10.))
                .rounded_full()
                .bg(color)
                .border_1()
                .border_color(crate::ui_theme::hair_strong())
        }),
    )
}
