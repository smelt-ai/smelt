//! 基于 gpui-base 的提示内容。触发、延迟和定位由 GPUI 的 tooltip 机制负责。
//! 紧凑样式：左右 6、上下 5、4 圆角，小号中等字重，`floating` 层级投影；快捷键用 [`crate::keybind::Keybind`]。
//! 出现时原地淡入（[`crate::motion::overlay_enter`]），不挪：它贴着指针，没有「从哪来」要交代。

use gpui::{
    Action, AnyElement, AnyView, App, AppContext as _, Context, IntoElement, KeyContext,
    KeybindingKeystroke, Keystroke, ParentElement, Render, SharedString, StyleRefinement, Styled,
    Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::StyledExt as _;

use crate::keybind::{Keybind, keycaps};
use crate::kit::{RADIUS, TYPE, elevation};
use crate::motion::{EnterFrom, overlay_enter};
use crate::ui_theme;

enum TooltipContent {
    Text(SharedString),
    Element(Box<dyn Fn(&mut Window, &mut App) -> AnyElement>),
}

/// 提示内容；触发和生命周期由调用方使用的 GPUI tooltip 机制负责。
pub struct Tooltip {
    content: TooltipContent,
    style: StyleRefinement,
    key_binding: Option<Vec<Keystroke>>,
    action: Option<(Box<dyn Action>, Option<SharedString>)>,
}

impl Tooltip {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            content: TooltipContent::Text(text.into()),
            style: StyleRefinement::default(),
            key_binding: None,
            action: None,
        }
    }

    pub fn element<E: IntoElement>(builder: impl Fn(&mut Window, &mut App) -> E + 'static) -> Self {
        Self {
            content: TooltipContent::Element(Box::new(move |window, cx| {
                builder(window, cx).into_any_element()
            })),
            style: StyleRefinement::default(),
            key_binding: None,
            action: None,
        }
    }

    /// 显式快捷键（按顺序的一串按键）优先于 action；传入 None 后恢复 action 查询。
    pub fn key_binding(mut self, key_binding: Option<Vec<Keystroke>>) -> Self {
        self.key_binding = key_binding.filter(|keys| !keys.is_empty());
        self
    }

    pub fn action(mut self, action: &dyn Action, context: Option<&str>) -> Self {
        self.action = Some((action.boxed_clone(), context.map(SharedString::new)));
        self
    }

    pub fn build(self, _: &mut Window, cx: &mut App) -> AnyView {
        cx.new(|_| self).into()
    }

    fn resolved_shortcut(&self, window: &Window, cx: &App) -> Option<Vec<Vec<String>>> {
        if let Some(keys) = &self.key_binding {
            let os = cx.platform_info().os;
            return Some(
                keys.iter()
                    .map(|key| keycaps(&KeybindingKeystroke::from_keystroke(key.clone()), os))
                    .collect(),
            );
        }
        self.action
            .as_ref()
            .and_then(|(action, context)| shortcut(action.as_ref(), context.as_deref(), window, cx))
    }
}

// GPUI 的 KeyContext::parse 在遇到非标识符字符时不前进，会无限递归。
// 按它的标识符和 key=value 语法迭代解析，并拒绝缺失的键或值。
fn parse_context(source: &str) -> Option<KeyContext> {
    let identifier_end = |text: &str| {
        text.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(text.len())
    };
    let mut context = KeyContext::default();
    let mut rest = source.trim_start();
    while !rest.is_empty() {
        let end = identifier_end(rest);
        if end == 0 {
            return None;
        }
        let key = &rest[..end];
        rest = rest[end..].trim_start();
        if let Some(after_equals) = rest.strip_prefix('=') {
            rest = after_equals.trim_start();
            let end = identifier_end(rest);
            if end == 0 {
                return None;
            }
            context.set(key.to_owned(), rest[..end].to_owned());
            rest = rest[end..].trim_start();
        } else {
            context.add(key.to_owned());
        }
    }
    Some(context)
}

fn shortcut(
    action: &dyn Action,
    context: Option<&str>,
    window: &Window,
    cx: &App,
) -> Option<Vec<Vec<String>>> {
    let binding = match context {
        Some(context) => {
            window.highest_precedence_binding_for_action_in_context(action, parse_context(context)?)
        }
        // 焦点尚未出现在已绘制帧中时查询返回 None，不借用不相关的渲染栈。
        None => match window.focused(cx) {
            Some(focus) => window.highest_precedence_binding_for_action_in(action, &focus),
            None => window
                .highest_precedence_binding_for_action_in_context(action, KeyContext::default()),
        },
    }?;
    let keys = binding.keystrokes();
    if keys.is_empty() {
        return None;
    }
    let os = cx.platform_info().os;
    Some(keys.iter().map(|key| keycaps(key, os)).collect())
}

/// 浮层上的字：主题的浮层字色，压在 `surface` 上不够 4.5:1 时拉开。
fn tooltip_foreground(surface: u32, foreground: u32) -> u32 {
    ui_theme::legible(foreground, surface, 4.5)
}

impl gpui::prelude::FluentBuilder for Tooltip {}

impl Styled for Tooltip {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Render for Tooltip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let shortcut = self.resolved_shortcut(window, cx);
        let content = match &self.content {
            TooltipContent::Text(text) => text.clone().into_any_element(),
            TooltipContent::Element(builder) => builder(window, cx),
        };

        // 贴着指针的小浮层用第一层底，比弹层的 `surface` 更跳出窗口底（深色下尤其）。
        let surface = ui_theme::soft();
        let foreground = tooltip_foreground(surface, ui_theme::popover_foreground(surface));
        // 每次出现都是新建的提示视图，元素状态是新的，入场从头播。
        let enter = overlay_enter("smelt-tooltip-enter", EnterFrom::InPlace, window, cx);

        // 留出阴影绘制的空间；悬浮层尺寸和定位由 GPUI 测量。
        div().child(
            gpui_base::Tooltip::new("tooltip-popup")
                .m_3()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(6.))
                .py(px(5.))
                .rounded(px(RADIUS.sm))
                .bg(rgb(surface))
                .text_color(rgb(foreground))
                .map(|this| TYPE.small_medium.apply(this))
                .font_features(ui_theme::tabular_figures())
                .shadow(elevation::floating())
                .opacity(enter.opacity)
                .refine_style(&self.style)
                .child(div().min_w_0().flex_1().child(content))
                .when_some(shortcut, |this, steps| {
                    this.child(Keybind::caps(steps).backdrop(surface))
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{FocusHandle, InteractiveElement as _, KeyBinding, OperatingSystem, TestAppContext};

    gpui::actions!(tooltip_tests, [Save, Unbound, Sequence]);

    struct Stage;

    impl Render for Stage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            div()
        }
    }

    struct FocusStage {
        editor: FocusHandle,
        terminal: FocusHandle,
    }

    impl Render for FocusStage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .key_context("Workspace")
                .child(
                    div()
                        .key_context("Editor mode=insert")
                        .track_focus(&self.editor),
                )
                .child(div().key_context("Terminal").track_focus(&self.terminal))
        }
    }

    #[test]
    fn context_parser_matches_gpui_for_valid_contexts_and_rejects_invalid_ones() {
        for source in [
            "",
            " ",
            "Editor",
            "Editor mode=insert",
            " Editor   mode = insert   file_type=rust ",
            "编辑器 mode=vim-visual",
        ] {
            assert_eq!(parse_context(source), KeyContext::parse(source).ok());
        }
        for source in ["Editor=", "Editor &", "Editor mode==insert"] {
            assert!(parse_context(source).is_none(), "{source:?}");
        }
    }

    #[test]
    fn tooltip_foreground_keeps_theme_color_when_readable_and_fixes_it_otherwise() {
        assert_eq!(tooltip_foreground(0xffffff, 0x222222), 0x222222);
        for (surface, foreground) in [
            (0x101010, 0x101010),
            (0xfafafa, 0xfafafa),
            (0x777777, 0x777777),
        ] {
            assert!(
                ui_theme::contrast_ratio(tooltip_foreground(surface, foreground), surface) >= 4.5
            );
        }
    }

    #[gpui::test]
    fn tooltip_uses_theme_popover_foreground(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let source = r##"{"version":1,"name":"t","themes":[{"name":"Popover Test","mode":"light",
                "colors":{"bg_base":"#ffffff","text_base":"#204060"}}]}"##;
            crate::theme_catalog::register_theme_sets("tooltip-test", &[source], cx).unwrap();
            assert!(crate::theme_catalog::apply("Popover Test", cx));
            assert_eq!(ui_theme::popover_foreground(0xfafafa), 0x204060);
        });
    }

    #[test]
    fn tooltip_builder_preserves_text_and_optional_action_context() {
        fn accepts_existing_generic(text: impl Into<SharedString>) -> Tooltip {
            Tooltip::new(text)
        }
        assert!(matches!(
            accepts_existing_generic("text").content,
            TooltipContent::Text(_)
        ));
        let tooltip = Tooltip::new("第一行\n第二行").action(&Save, Some("Editor mode=insert"));
        match &tooltip.content {
            TooltipContent::Text(text) => assert_eq!(text.as_ref(), "第一行\n第二行"),
            _ => panic!("expected plain text"),
        }
        assert_eq!(
            tooltip.action.as_ref().unwrap().1.as_deref(),
            Some("Editor mode=insert")
        );
        assert!(
            Tooltip::new("")
                .action(&Save, None)
                .action
                .unwrap()
                .1
                .is_none()
        );
        assert!(Tooltip::new("无快捷键").action.is_none());
    }

    #[gpui::test]
    fn explicit_key_binding_overrides_action_and_none_restores_it(cx: &mut TestAppContext) {
        cx.update(|cx| cx.bind_keys([KeyBinding::new("ctrl-k ctrl-s", Sequence, None)]));
        let (_, cx) = cx.add_window_view(|_, _| Stage);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let binding = vec![Keystroke::parse("alt-x").unwrap()];
            let tooltip = Tooltip::new("test")
                .action(&Sequence, None)
                .key_binding(Some(binding));
            assert_eq!(
                tooltip
                    .resolved_shortcut(window, cx)
                    .map(|steps| steps.len()),
                Some(1)
            );
            let tooltip = tooltip.key_binding(None);
            assert_eq!(
                tooltip
                    .resolved_shortcut(window, cx)
                    .map(|steps| steps.len()),
                Some(2)
            );
            let tooltip = Tooltip::new("test").key_binding(Some(Vec::new()));
            assert_eq!(tooltip.resolved_shortcut(window, cx), None);
        });
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn custom_element_is_built_and_styled_width_reaches_content(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc};

        cx.update(gpui_base::init);
        let builds = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let builds = builds.clone();
            move |_, _| {
                Tooltip::element(move |_, _| {
                    builds.set(builds.get() + 1);
                    div()
                        .w_full()
                        .h(px(20.))
                        .debug_selector(|| "TOOLTIP_CUSTOM".into())
                })
                .w(px(200.))
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(builds.get() > 0);
        let bounds = cx
            .debug_bounds("TOOLTIP_CUSTOM")
            .expect("custom element rendered");
        // Horizontal padding remains 6px per side; the styled width applies to the popup.
        assert_eq!(bounds.size.width, px(188.));
        assert_eq!(bounds.size.height, px(20.));
    }

    #[test]
    fn floating_shadow_layers_and_opaque_surface() {
        for light in [true, false] {
            let mode = if light {
                crate::theme_config::ThemeMode::Light
            } else {
                crate::theme_config::ThemeMode::Dark
            };
            let shadows = crate::theme_catalog::builtin_for_mode(mode)
                .elevation()
                .floating
                .clone();
            assert_eq!(shadows.len(), if light { 3 } else { 4 });
            assert_eq!(shadows[0].blur_radius, px(16.));
            assert_eq!(shadows[1].blur_radius, px(8.));
            assert_eq!(shadows[2].spread_radius, px(0.5));
            // 背景是实色才能遮住 GPUI 投影画进矩形内部的部分。
            assert_eq!(rgb(ui_theme::builtin_fallback(light).soft).a, 1.);
        }
    }

    #[gpui::test]
    fn shortcut_shows_all_keystrokes_and_rejects_invalid_context(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys([KeyBinding::new("ctrl-k ctrl-s", Sequence, None)]);
        });
        let (_, cx) = cx.add_window_view(|_, _| Stage);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            let control = if cx.platform_info().os == OperatingSystem::Mac {
                "⌃"
            } else {
                "Ctrl"
            };
            let expected = vec![
                vec![control.to_owned(), "K".into()],
                vec![control.to_owned(), "S".into()],
            ];
            assert_eq!(
                shortcut(&Sequence, None, window, cx),
                Some(expected.clone())
            );
            assert_eq!(shortcut(&Sequence, Some("Editor="), window, cx), None);
            assert_eq!(shortcut(&Sequence, Some("Editor &"), window, cx), None);
            assert_eq!(shortcut(&Sequence, Some(""), window, cx), Some(expected));
        });
    }

    #[gpui::test]
    fn shortcut_uses_the_real_focused_context_stack_and_global_fallback(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys([
                KeyBinding::new("ctrl-g", Save, None),
                KeyBinding::new("ctrl-e", Save, Some("Workspace > Editor && mode == insert")),
                KeyBinding::new("ctrl-t", Save, Some("Terminal")),
                KeyBinding::new("ctrl-x", Unbound, Some("Editor")),
                KeyBinding::new("ctrl-x", Sequence, Some("Editor")),
            ]);
        });
        let (view, cx) = cx.add_window_view(|_, cx| FocusStage {
            editor: cx.focus_handle(),
            terminal: cx.focus_handle(),
        });
        let (editor, terminal) = cx.update(|_, cx| {
            let view = view.read(cx);
            (view.editor.clone(), view.terminal.clone())
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let control = if cx.platform_info().os == OperatingSystem::Mac {
                "⌃"
            } else {
                "Ctrl"
            };
            let expected = |key: &str| Some(vec![vec![control.to_owned(), key.to_owned()]]);
            assert!(window.focused(cx).is_none());
            assert_eq!(shortcut(&Save, None, window, cx), expected("G"));
            window.focus(&editor, cx);
            window.draw(cx).clear(cx);
            assert_eq!(shortcut(&Save, None, window, cx), expected("E"));
            assert_eq!(shortcut(&Unbound, None, window, cx), None);
            assert_eq!(shortcut(&Sequence, None, window, cx), expected("X"));
            // Explicit context does not use the focused context stack.
            assert_eq!(shortcut(&Save, Some("Terminal"), window, cx), expected("T"));
            window.focus(&terminal, cx);
            window.draw(cx).clear(cx);
            assert_eq!(shortcut(&Save, None, window, cx), expected("T"));
            window.blur(cx);
            assert_eq!(shortcut(&Save, None, window, cx), expected("G"));
            assert_eq!(shortcut(&Save, Some("Editor="), window, cx), None);
        });
    }

    #[gpui::test]
    fn shortcut_uses_highest_precedence_in_the_requested_context(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys([
                KeyBinding::new("ctrl-s", Save, Some("Editor")),
                KeyBinding::new("ctrl-shift-s", Save, Some("Editor")),
            ]);
        });
        let (_, cx) = cx.add_window_view(|_, _| Stage);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            let expected = if cx.platform_info().os == OperatingSystem::Mac {
                vec![vec!["⌃".into(), "⇧".into(), "S".into()]]
            } else {
                vec![vec!["Ctrl".into(), "Shift".into(), "S".into()]]
            };
            assert_eq!(shortcut(&Save, Some("Editor"), window, cx), Some(expected));
            assert_eq!(shortcut(&Save, Some("Terminal"), window, cx), None);
            assert_eq!(shortcut(&Unbound, None, window, cx), None);
        });
    }
}
