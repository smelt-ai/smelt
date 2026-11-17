//! 代码编辑器：整块铺满的源码编辑区，带行号栏、折叠箭头、搜索替换栏和右键菜单。
//!
//! 编辑、折叠、语法分析和搜索的状态都归 gpui-base 的 [`EditorState`]；这里只画：等宽字（用户
//! 设的代码字体，字号取 `TYPE.mono`）、把当前主题的颜色和代码高亮样式（`ActiveTheme::syntax`）
//! 投影给编辑器、折叠箭头、搜索替换栏。语法分析器（tree-sitter 之类）由调用方装在状态上
//! （`set_highlighter_factory`），本库不带语言包。
//!
//! 搜索走状态的搜索接口：⌘F / ⌘⇧F 由状态自己接（状态要开 `searchable`），它每次打开都把
//! `search_activation_revision` 加一；搜索栏看着状态，看到加一就把查询词填进输入框、全选、
//! 拿焦点。查询词、大小写、匹配位置都存在状态的搜索会话里，搜索栏只多留两个输入框。

use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, Edges, Entity, Focusable as _, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, RenderOnce, Role, SharedString,
    StyleRefinement, Styled, Subscription, WeakEntity, Window, div, prelude::FluentBuilder as _,
    px, relative, rgb,
};
use gpui_base::StyledExt as _;
pub use gpui_base::input::EditorState;
use gpui_base::input::{
    DiagnosticColors, Editor as BaseEditor, Enter, Escape, IndentInline, InputAccessibility,
    InputEditorStyle, OutdentInline, Replace, Search,
};

use crate::button::Button;
use crate::field::{InputEvent, InputState, TextInput, edit_menu};
use crate::icon::{Icon, IconName};
use crate::kit::{GLYPHS, TYPE, TypeStyled as _};
use crate::menu::{Menu, MenuTrigger};
use crate::{theme_catalog, ui_theme};

/// 行高是字号的倍数：调用方改字号时行跟着变。
const LINE_HEIGHT: f32 = 1.5;
/// 编辑区内边距。左边紧挨行号栏，行号栏自己留了白。
const PADDING: Edges<Pixels> = Edges {
    top: px(8.),
    right: px(12.),
    bottom: px(8.),
    left: px(6.),
};
/// 搜索栏两个输入框一样宽，上下对齐。
const SEARCH_INPUT_WIDTH: f32 = 260.;
const SEARCH_CONTEXT: &str = "CodeSearch";

/// 往右键菜单里追加项。
type MenuExtension = Rc<dyn Fn(Menu, &mut Window, &mut App) -> Menu>;

/// 代码编辑器。铺满父容器，父容器给定高度。
#[derive(IntoElement)]
pub struct CodeEditor {
    state: Entity<EditorState>,
    style: StyleRefinement,
    menu: Option<MenuExtension>,
    label: Option<SharedString>,
}

impl CodeEditor {
    pub fn new(state: &Entity<EditorState>) -> Self {
        Self {
            state: state.clone(),
            style: StyleRefinement::default(),
            menu: None,
            label: None,
        }
    }

    /// 右键菜单在剪切、复制、粘贴、全选后面追加的项（隔一条分隔线）。菜单打开时才调。
    pub fn context_menu(
        mut self,
        extend: impl Fn(Menu, &mut Window, &mut App) -> Menu + 'static,
    ) -> Self {
        self.menu = Some(Rc::new(extend));
        self
    }

    /// 给读屏的名字，通常是文件名。
    pub fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Styled for CodeEditor {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// 编辑器的颜色、高亮样式和折叠箭头，每帧从当前主题取（换主题不用通知编辑器）。
fn editor_style(cx: &App) -> InputEditorStyle {
    let c = ui_theme::colors();
    let text: Hsla = c.text_base.into();
    let faint: Hsla = c.text_faint.into();
    let mut style = InputEditorStyle {
        foreground: text,
        muted_foreground: faint,
        background: c.bg_base.into(),
        border: c.border_strong.into(),
        selection: c.selection.into(),
        caret: text,
        diagnostics: DiagnosticColors {
            error: c.danger.fg.into(),
            warning: c.warning.fg.into(),
            info: c.info.fg.into(),
            hint: c.info.fg.into(),
        },
        editor_invisible: Some(faint),
        editor_active_line: Some(c.hover.into()),
        editor_gutter_background: Some(c.bg_base.into()),
        fold_icon_renderer: Some(Rc::new(fold_icon)),
        ..InputEditorStyle::default()
    };
    if let Some(theme) = theme_catalog::active(cx) {
        style.highlight_styles = theme.syntax().clone();
    }
    style
}

/// 行号栏里的折叠箭头：平时淡，悬停铺底；收起着的那行加深，一眼找得到折了哪。点击由编辑器接。
fn fold_icon(ix: usize, folded: bool) -> gpui::AnyElement {
    let icon = if folded {
        GLYPHS.chevron_right
    } else {
        GLYPHS.chevron_down
    };
    div()
        .id(("smelt-fold-icon", ix))
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .cursor_pointer()
        .text_color(if folded {
            rgb(ui_theme::muted())
        } else {
            rgb(ui_theme::faint())
        })
        .when(folded, |this| this.bg(ui_theme::row_hover()))
        .hover(|this| {
            this.bg(ui_theme::row_hover())
                .text_color(rgb(ui_theme::text()))
        })
        .child(Icon::new(icon).size(px(12.)))
        .into_any_element()
}

impl RenderOnce for CodeEditor {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            state,
            style,
            menu,
            label,
        } = self;
        let editor_style = editor_style(cx);
        state.update(cx, |state, _| {
            state.set_editor_style(editor_style);
            state.set_editor_paddings(PADDING);
            state.set_accessibility(
                InputAccessibility::new(Some(Role::MultilineTextInput)).with_label(label),
            );
        });
        let search = window.use_keyed_state(
            ("smelt-code-search", state.entity_id()),
            cx,
            |window, cx| CodeSearch::new(&state, window, cx),
        );
        let search_open = state.read(cx).search_session().open;

        let base_menu = edit_menu(&state);
        let menu = move |window: &mut Window, cx: &mut App| {
            let items = base_menu(window, cx);
            match &menu {
                Some(extend) => extend(items.separator(), window, cx),
                None => items,
            }
        };
        let text = div()
            .relative()
            .size_full()
            .type_style(TYPE.mono)
            .line_height(relative(LINE_HEIGHT))
            .font_family(smelt_model::font_config::font_family())
            .text_color(rgb(ui_theme::text()))
            .child(BaseEditor::new(&state));
        let area = MenuTrigger::context(("smelt-code-editor-menu", state.entity_id()), text, menu)
            .flex_1()
            .min_h_0()
            .w_full();

        let editor = state.downgrade();
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .bg(rgb(ui_theme::bg()))
            // 光标在编辑区里按 Esc：编辑器不用它时往外冒到这里，搜索栏开着就关掉。
            .on_action(move |_: &Escape, window, cx| {
                let Some(editor) = editor.upgrade() else {
                    return;
                };
                if !editor.read(cx).search_session().open {
                    cx.propagate();
                    return;
                }
                editor.update(cx, |editor, cx| editor.close_search(cx));
                editor.read(cx).focus_handle(cx).focus(window, cx);
            })
            .when(search_open, |this| this.child(search))
            .child(area)
            .refine_style(&style)
    }
}

/// 搜索替换栏。查询词、大小写、匹配都在编辑器状态的搜索会话里，这里只有两个输入框。
struct CodeSearch {
    editor: WeakEntity<EditorState>,
    query: Entity<InputState>,
    replacement: Entity<InputState>,
    /// 处理过的最后一次「打开搜索」（状态的 `search_activation_revision`）。
    activation: u64,
    _subscriptions: [Subscription; 2],
}

impl CodeSearch {
    fn new(editor: &Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 首次画出来之前搜索就可能开着（调用方先开了搜索），查询词从会话里接上。
        let seed = editor.read(cx).search_session().query.clone();
        let query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("查找")
                .default_value(seed)
        });
        let replacement = cx.new(|cx| InputState::new(window, cx).placeholder("替换为"));
        let _subscriptions = [
            cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search(None, cx);
                }
            }),
            cx.observe_in(editor, window, Self::sync),
        ];
        Self {
            editor: editor.downgrade(),
            query,
            replacement,
            activation: editor.read(cx).search_activation_revision(),
            _subscriptions,
        }
    }

    /// 编辑器变了。又按了一次「打开搜索」时，把会话里的查询词（打开时有选区就是选区）填进
    /// 输入框、全选、拿焦点；别的变化（打字、跳到下一处）只重画计数。
    fn sync(&mut self, editor: Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) {
        let (revision, open, query) = {
            let editor = editor.read(cx);
            let session = editor.search_session();
            (
                editor.search_activation_revision(),
                session.open,
                session.query.clone(),
            )
        };
        if revision != self.activation {
            self.activation = revision;
            // 填值不发 Change，不会回头再查一次。
            self.query.update(cx, |input, cx| {
                input.set_value(query, window, cx);
                input.select_all(window, cx);
                input.focus(window, cx);
            });
        }
        if open {
            cx.notify();
        }
    }

    /// 按输入框里的词重新查。`case_insensitive` 不给时沿用会话里的。
    fn search(&mut self, case_insensitive: Option<bool>, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_string();
        self.editor
            .update(cx, |editor, cx| {
                let case_insensitive =
                    case_insensitive.unwrap_or(editor.search_session().case_insensitive);
                editor.set_search_query(query, case_insensitive, cx);
            })
            .ok();
    }

    fn toggle_case(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let case_insensitive = editor.read(cx).search_session().case_insensitive;
        self.search(Some(!case_insensitive), cx);
    }

    fn replaceable(&self, cx: &App) -> bool {
        self.editor
            .upgrade()
            .is_some_and(|editor| editor.read(cx).is_replaceable())
    }

    /// 开关替换栏，焦点去要用的那个输入框。
    fn toggle_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        if !editor.read(cx).is_replaceable() {
            return;
        }
        let replace = !editor.read(cx).search_session().replace_mode;
        editor.update(cx, |editor, cx| editor.set_search_replace_mode(replace, cx));
        let target = if replace {
            &self.replacement
        } else {
            &self.query
        };
        target.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Tab 在两个输入框之间来回，不跑出搜索栏。
    fn cycle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let replace_open = self.editor.upgrade().is_some_and(|editor| {
            editor.read(cx).search_session().replace_mode && editor.read(cx).is_replaceable()
        });
        if !replace_open {
            return;
        }
        let target = if self.query.read(cx).focus_handle(cx).is_focused(window) {
            &self.replacement
        } else {
            &self.query
        };
        target.update(cx, |input, cx| input.focus(window, cx));
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| {
                if forward {
                    editor.next_search_match(cx);
                } else {
                    editor.previous_search_match(cx);
                }
            })
            .ok();
    }

    fn replace(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        let replacement = self.replacement.read(cx).value();
        self.editor
            .update(cx, |editor, cx| {
                if all {
                    editor.replace_all_search_matches(&replacement, window, cx);
                } else {
                    editor.replace_current_search_match(&replacement, window, cx);
                }
            })
            .ok();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| {
                editor.close_search(cx);
                editor.focus(window, cx);
            })
            .ok();
    }
}

/// 计数：没输词时不显示，有词没匹配时说没有，否则「第几处/共几处」。
fn match_label(query: &str, current: usize, total: usize) -> SharedString {
    if query.is_empty() {
        SharedString::default()
    } else if total == 0 {
        "无结果".into()
    } else {
        format!("{}/{}", current + 1, total).into()
    }
}

impl Render for CodeSearch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(editor) = self.editor.upgrade() else {
            return div().into_any_element();
        };
        let (case_insensitive, replace_mode, has_matches, label) = {
            let session = editor.read(cx).search_session();
            let matcher = &session.matcher;
            (
                session.case_insensitive,
                session.replace_mode,
                !matcher.is_empty(),
                match_label(&session.query, matcher.current_match_index(), matcher.len()),
            )
        };
        let replaceable = self.replaceable(cx);
        let replace_open = replace_mode && replaceable;
        let icon_button = |id: &'static str, icon: IconName, label: &'static str| {
            Button::new(id)
                .quiet()
                .small()
                .icon(icon)
                .accessibility_label(label)
                .tooltip(label)
        };

        let find_row = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(
                TextInput::new(&self.query)
                    .small()
                    .aria_label("查找")
                    .w(px(SEARCH_INPUT_WIDTH)),
            )
            .child(
                icon_button("case", IconName::CaseSensitive, "区分大小写")
                    .selected(!case_insensitive)
                    .toggled(!case_insensitive)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_case(cx))),
            )
            .when(replaceable, |this| {
                this.child(
                    icon_button("replace-mode", IconName::Replace, "替换")
                        .selected(replace_open)
                        .toggled(replace_open)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_replace(window, cx)),
                        ),
                )
            })
            .child(
                icon_button("prev", IconName::ArrowUp, "上一处")
                    .disabled(!has_matches)
                    .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
            )
            .child(
                icon_button("next", IconName::ArrowDown, "下一处")
                    .disabled(!has_matches)
                    .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
            )
            .child(
                div()
                    .min_w(px(48.))
                    .px(px(4.))
                    .type_style(TYPE.small)
                    .font_features(ui_theme::tabular_figures())
                    .text_color(rgb(if has_matches {
                        ui_theme::muted()
                    } else {
                        ui_theme::faint()
                    }))
                    .child(label),
            )
            .child(div().flex_1())
            .child(
                Button::new("close")
                    .quiet()
                    .small()
                    .icon(GLYPHS.close)
                    .accessibility_label("关闭搜索")
                    .tooltip("关闭搜索")
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            );

        div()
            .id("smelt-code-search")
            .key_context(SEARCH_CONTEXT)
            .on_action(cx.listener(|this, action: &Enter, _, cx| this.step(!action.shift, cx)))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, _: &IndentInline, window, cx| {
                this.cycle_focus(window, cx)
            }))
            .on_action(cx.listener(|this, _: &OutdentInline, window, cx| {
                this.cycle_focus(window, cx)
            }))
            .on_action(cx.listener(|this, _: &Replace, window, cx| this.toggle_replace(window, cx)))
            // 焦点已经在搜索栏里时再按 ⌘F：重新全选查询词。
            .on_action(cx.listener(|this, _: &Search, window, cx| {
                this.query.update(cx, |input, cx| {
                    input.select_all(window, cx);
                    input.focus(window, cx);
                });
            }))
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(4.))
            .px(px(8.))
            .py(px(6.))
            .border_b_1()
            .border_color(ui_theme::hair())
            .bg(rgb(ui_theme::side()))
            .child(find_row)
            .when(replace_open, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            TextInput::new(&self.replacement)
                                .small()
                                .aria_label("替换为")
                                .w(px(SEARCH_INPUT_WIDTH)),
                        )
                        .child(
                            Button::new("replace-one")
                                .small()
                                .label("替换")
                                .disabled(!has_matches)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.replace(false, window, cx)
                                })),
                        )
                        .child(
                            Button::new("replace-all")
                                .small()
                                .label("全部替换")
                                .disabled(!has_matches)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.replace(true, window, cx)
                                })),
                        ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_label_reads_naturally() {
        assert_eq!(match_label("", 0, 0).as_ref(), "");
        assert_eq!(match_label("foo", 0, 0).as_ref(), "无结果");
        assert_eq!(match_label("foo", 1, 3).as_ref(), "2/3");
    }
}
