//! 可搜索的下拉选择：一个写着当前值的按钮，点开是一张带搜索框的列表。选项多到要搜时用它，
//! 几项的用 [`crate::Select`]。上面搜索框、下面分组列表，当前项
//! 打勾，选项可以带前缀装饰（色点、图标）。
//!
//! 搜索框和列表是 [`SearchList`]，命令面板（[`crate::command_palette`]）也用它：
//! - 搜索不分大小写，按子序列打分：连着的、落在词头的字分高，名字里的匹配比关键词里的高；
//!   组内按分排，组按组里最高分排，空组不显示。没有搜索词时保持调用方给的顺序。
//! - ↑↓ 和 Ctrl-N / Ctrl-P 在可选的行间循环，PageUp / PageDown 翻一屏；键盘移到的行滚进视野。
//!   鼠标移到哪行就高亮哪行，键盘滚动时指针不动不抢高亮。
//! - Enter 选高亮的那行（没高亮时是第一个可选的）。输入法正在组字时这几个键都交给输入法。
//! - Esc 由外面那层（弹出卡片、对话框）关。
//!
//! 选项由调用方给，值由调用方持有：传进当前值，选了别的由 [`Combobox::on_change`] 交回去。

use std::cmp::Reverse;
use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, AppContext as _, Context, ElementId, Entity, EntityInputHandler as _,
    Focusable as _, InteractiveElement as _, IntoElement, KeyBinding, Keystroke, MouseMoveEvent,
    ParentElement as _, Pixels, Render, RenderOnce, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px, rgb, size, svg,
};
use gpui_base::{StyledExt as _, VirtualListScrollHandle, v_virtual_list};
use gpui_kit_assets::IconNamed as _;

use crate::button::Button;
use crate::field::{InputEvent, InputState, TextInput};
use crate::icon::Icon;
use crate::keybind::Keybind;
use crate::kit::{GLYPHS, RADIUS, TYPE};
use crate::popover::Popover;
use crate::ui_theme;

gpui::actions!(
    smelt_search_list,
    [Next, Previous, PageNext, PagePrevious, Choose]
);

const CONTEXT: &str = "SmeltSearchList";

/// 组标题行和选项行的高。
const HEADER_HEIGHT: f32 = 24.;
const OPTION_HEIGHT: f32 = 28.;
/// 搜索框的高。
const SEARCH_HEIGHT: f32 = 36.;

/// 装上列表的键位。[`crate::init`] 调。
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", Previous, Some(CONTEXT)),
        KeyBinding::new("ctrl-p", Previous, Some(CONTEXT)),
        KeyBinding::new("down", Next, Some(CONTEXT)),
        KeyBinding::new("ctrl-n", Next, Some(CONTEXT)),
        KeyBinding::new("pageup", PagePrevious, Some(CONTEXT)),
        KeyBinding::new("pagedown", PageNext, Some(CONTEXT)),
        KeyBinding::new("enter", Choose, Some(CONTEXT)),
    ]);
}

type Prefix = Rc<dyn Fn() -> AnyElement>;
type ChooseHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// 一个选项。`id` 是稳定的值，选中、打勾、回调都认它；`label` 是显示的名字。
#[derive(Clone)]
pub struct ComboboxOption {
    pub id: SharedString,
    pub label: SharedString,
    /// 名字后面的一段灰字，也参与搜索。
    pub detail: Option<SharedString>,
    /// 只参与搜索、不显示的词（别名、所属分组）。
    pub keywords: Vec<SharedString>,
    prefix: Option<Prefix>,
    /// 行尾的快捷键。
    pub keybind: Vec<Keystroke>,
    pub disabled: bool,
}

impl ComboboxOption {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
            keywords: Vec::new(),
            prefix: None,
            keybind: Vec::new(),
            disabled: false,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn keywords(mut self, keywords: impl IntoIterator<Item = impl Into<SharedString>>) -> Self {
        self.keywords = keywords.into_iter().map(Into::into).collect();
        self
    }

    /// 名字前面的装饰（色点、图标），每次画这一行时搭一次；选中后按钮上也画它。
    /// 闭包里只放现成的值，不做 IO、不现算。
    pub fn prefix<E: IntoElement>(mut self, prefix: impl Fn() -> E + 'static) -> Self {
        self.prefix = Some(Rc::new(move || prefix().into_any_element()));
        self
    }

    pub fn keybind(mut self, keybind: impl IntoIterator<Item = Keystroke>) -> Self {
        self.keybind = keybind.into_iter().collect();
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// 一组选项。`label` 为空时不画组标题。
#[derive(Clone, Default)]
pub struct ComboboxGroup {
    pub label: Option<SharedString>,
    pub options: Vec<ComboboxOption>,
}

impl ComboboxGroup {
    pub fn new(label: Option<SharedString>, options: Vec<ComboboxOption>) -> Self {
        Self { label, options }
    }
}

/// 过滤后列表里的一行，下标指回调用方给的分组。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Header { group: usize },
    Option { group: usize, option: usize },
}

impl Row {
    fn height(self) -> Pixels {
        px(match self {
            Self::Header { .. } => HEADER_HEIGHT,
            Self::Option { .. } => OPTION_HEIGHT,
        })
    }
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// 第 `i` 个字是不是词头：开头、空白或标点后面、小写转大写处。
fn word_start(chars: &[char], i: usize) -> bool {
    let Some(prev) = i.checked_sub(1).map(|p| chars[p]) else {
        return true;
    };
    prev.is_whitespace()
        || matches!(prev, '-' | '_' | '/' | '.' | ':' | '(' | ')' | '[' | ']')
        || (prev.is_lowercase() && chars[i].is_uppercase())
}

/// `query`（已折成小写）是不是 `text` 的子序列，是就给个分：每个字 10，和上一个字连着
/// 加 12，落在词头加 8，中间跳过一个字扣 1。从每个能起头的位置贪心走一遍，取最高的。
fn score(text: &str, query: &[char]) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = text.chars().collect();
    let folded: Vec<char> = chars.iter().copied().map(fold).collect();
    let mut best = None;
    for start in (0..folded.len()).filter(|&i| folded[i] == query[0]) {
        let mut total = 0;
        let mut prev: Option<usize> = None;
        let mut matched = 0;
        for i in start..folded.len() {
            if matched == query.len() {
                break;
            }
            if folded[i] != query[matched] {
                continue;
            }
            let mut points = 10;
            if prev.is_some_and(|p| p + 1 == i) {
                points += 12;
            }
            if word_start(&chars, i) {
                points += 8;
            }
            let skipped = prev.map_or(i, |p| i - p - 1);
            total += points - skipped as i32;
            prev = Some(i);
            matched += 1;
        }
        if matched == query.len() {
            best = best.max(Some(total));
        }
    }
    best
}

/// 一个选项对搜索词的分：名字照算，说明和关键词里的匹配减半，名字里匹配的排前面。
fn option_score(option: &ComboboxOption, query: &[char]) -> Option<i32> {
    let secondary = option
        .detail
        .iter()
        .chain(&option.keywords)
        .filter_map(|text| score(text, query))
        .max()
        .map(|points| points / 2);
    score(&option.label, query).max(secondary)
}

/// 按搜索词过滤、排序，摊成一行行。
fn filter(groups: &[ComboboxGroup], query: &str) -> Vec<Row> {
    let query: Vec<char> = query.trim().chars().map(fold).collect();
    let mut matched: Vec<(i32, usize, Vec<(usize, i32)>)> = Vec::new();
    for (group_ix, group) in groups.iter().enumerate() {
        let mut options: Vec<(usize, i32)> = group
            .options
            .iter()
            .enumerate()
            .filter_map(|(ix, option)| option_score(option, &query).map(|points| (ix, points)))
            .collect();
        // 稳定排序：同分的保持原来的先后。
        options.sort_by_key(|&(_, points)| Reverse(points));
        if let Some(&(_, best)) = options.first() {
            matched.push((best, group_ix, options));
        }
    }
    matched.sort_by_key(|&(best, _, _)| Reverse(best));
    let mut rows = Vec::new();
    for (_, group, options) in matched {
        if groups[group].label.is_some() {
            rows.push(Row::Header { group });
        }
        rows.extend(
            options
                .into_iter()
                .map(|(option, _)| Row::Option { group, option }),
        );
    }
    rows
}

/// 在可选的行（`enabled` 是它们的行号）里从 `active` 挪 `delta` 步。`wrap` 时首尾相接，
/// 不然停在两头。还没有高亮时往下从第一个开始、往上从最后一个开始。
fn step(enabled: &[usize], active: Option<usize>, delta: isize, wrap: bool) -> Option<usize> {
    let last = enabled.len().checked_sub(1)?;
    let Some(position) = active.and_then(|row| enabled.iter().position(|&r| r == row)) else {
        return Some(if delta > 0 { enabled[0] } else { enabled[last] });
    };
    let target = position as isize + delta;
    let target = if wrap {
        target.rem_euclid(enabled.len() as isize) as usize
    } else {
        target.clamp(0, last as isize) as usize
    };
    Some(enabled[target])
}

/// 搜索框加一列可选的行。组合框和命令面板共用；外面那层（弹出卡片、对话框）管开关和 Esc。
pub(crate) struct SearchList {
    query: Entity<InputState>,
    groups: Rc<Vec<ComboboxGroup>>,
    /// 当前值，那一行打勾。`None` 时不留打勾的那一列。
    current: Option<SharedString>,
    rows: Vec<Row>,
    sizes: Rc<Vec<gpui::Size<Pixels>>>,
    /// 键盘或鼠标停在的那项。不在过滤结果里、或不可选时，按第一个可选的算。
    highlight: Option<SharedString>,
    scroll: VirtualListScrollHandle,
    max_height: Pixels,
    on_choose: Option<ChooseHandler>,
    _query_changes: Subscription,
}

impl SearchList {
    pub(crate) fn new(
        placeholder: impl Into<SharedString>,
        max_height: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = placeholder.into();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let query_changes = cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.refilter(cx);
                // 换了搜索词，最匹配的那项排在最前，高亮回到它。
                this.highlight = None;
                this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
        });
        Self {
            query,
            groups: Rc::default(),
            current: None,
            rows: Vec::new(),
            sizes: Rc::default(),
            highlight: None,
            scroll: VirtualListScrollHandle::new(),
            max_height,
            on_choose: None,
            _query_changes: query_changes,
        }
    }

    /// 换选项和当前值。选项是同一份（`Rc` 同一个）时不重新过滤，可以每帧调。
    pub(crate) fn set_groups(
        &mut self,
        groups: Rc<Vec<ComboboxGroup>>,
        current: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        if !Rc::ptr_eq(&self.groups, &groups) {
            self.groups = groups;
            self.refilter(cx);
        }
        self.current = current;
    }

    /// 选了一项时跑，推到这一轮事件之后。
    pub(crate) fn set_on_choose(
        &mut self,
        on_choose: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) {
        self.on_choose = Some(Rc::new(on_choose));
    }

    pub(crate) fn query_focus(&self, cx: &App) -> gpui::FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }

    /// 每次打开时调：清空搜索词，高亮当前值并把它滚到中间。
    pub(crate) fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query
            .update(cx, |query, cx| query.set_value("", window, cx));
        self.refilter(cx);
        self.highlight = self.current.clone();
        match self.active_row() {
            Some(row) if self.highlight.is_some() => {
                self.scroll.scroll_to_item(row, ScrollStrategy::Center)
            }
            _ => self.scroll.scroll_to_item(0, ScrollStrategy::Top),
        }
        cx.notify();
    }

    fn refilter(&mut self, cx: &App) {
        let query = self.query.read(cx).value();
        self.rows = filter(&self.groups, &query);
        self.sizes = Rc::new(
            self.rows
                .iter()
                .map(|row| size(px(0.), row.height()))
                .collect(),
        );
    }

    fn option(&self, row: usize) -> Option<&ComboboxOption> {
        match self.rows.get(row)? {
            Row::Option { group, option } => self.groups.get(*group)?.options.get(*option),
            Row::Header { .. } => None,
        }
    }

    fn enabled_rows(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&row| self.option(row).is_some_and(|option| !option.disabled))
            .collect()
    }

    /// 此刻高亮的行：停在的那项可选就是它，不然是第一个可选的。
    fn active_row(&self) -> Option<usize> {
        let enabled = |row: &usize| self.option(*row).is_some_and(|option| !option.disabled);
        self.highlight
            .as_ref()
            .and_then(|id| {
                (0..self.rows.len())
                    .filter(enabled)
                    .find(|&row| self.option(row).is_some_and(|option| &option.id == id))
            })
            .or_else(|| (0..self.rows.len()).find(enabled))
    }

    /// 输入法正在组字：方向键、回车归输入法。
    fn composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.query.update(cx, |query, cx| {
            query.marked_text_range(window, cx).is_some()
        })
    }

    fn navigate(&mut self, delta: isize, wrap: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.composing(window, cx) {
            cx.propagate();
            return;
        }
        let active = self.active_row();
        let Some(row) = step(&self.enabled_rows(), active, delta, wrap) else {
            return;
        };
        self.highlight = self.option(row).map(|option| option.id.clone());
        // 往上走到一组的第一项时把组标题也露出来。
        let upward = active.is_none_or(|active| row < active);
        let target = match row.checked_sub(1).map(|above| self.rows[above]) {
            Some(Row::Header { .. }) if upward => row - 1,
            _ => row,
        };
        self.scroll.scroll_to_item(target, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn page(&self) -> isize {
        ((self.max_height / px(OPTION_HEIGHT)).floor() as isize).max(1)
    }

    fn next(&mut self, _: &Next, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(1, true, window, cx);
    }

    fn previous(&mut self, _: &Previous, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(-1, true, window, cx);
    }

    fn page_next(&mut self, _: &PageNext, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(self.page(), false, window, cx);
    }

    fn page_previous(&mut self, _: &PagePrevious, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(-self.page(), false, window, cx);
    }

    fn choose_active(&mut self, _: &Choose, window: &mut Window, cx: &mut Context<Self>) {
        if self.composing(window, cx) {
            cx.propagate();
            return;
        }
        if let Some(row) = self.active_row() {
            self.choose(row, window, cx);
        }
    }

    fn choose(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(option) = self.option(row).filter(|option| !option.disabled) else {
            return;
        };
        let id = option.id.clone();
        // 回调要关掉外面那层、改调用方的状态，不在这个实体的更新里跑。
        if let Some(on_choose) = self.on_choose.clone() {
            window.defer(cx, move |window, cx| on_choose(&id, window, cx));
        }
    }

    fn hover(&mut self, row: usize, cx: &mut Context<Self>) {
        let id = self.option(row).map(|option| option.id.clone());
        if id.is_some() && self.highlight != id {
            self.highlight = id;
            cx.notify();
        }
    }

    fn render_row(&self, row: usize, active: Option<usize>, cx: &mut Context<Self>) -> AnyElement {
        let option = match self.rows[row] {
            Row::Header { group } => {
                // 列表按可用宽度排每一行，行自己要撑满，不然只有字那么宽。
                return div()
                    .w_full()
                    .h(px(HEADER_HEIGHT))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .map(|header| TYPE.small_medium.apply(header))
                    .text_color(rgb(ui_theme::muted()))
                    .children(self.groups[group].label.clone())
                    .into_any_element();
            }
            Row::Option { group, option } => &self.groups[group].options[option],
        };
        let is_active = active == Some(row);
        let is_current = self.current.as_ref() == Some(&option.id);
        let background = if is_active {
            ui_theme::soft_hover()
        } else {
            ui_theme::surface()
        };
        let foreground = rgb(if option.disabled {
            ui_theme::muted()
        } else {
            ui_theme::text()
        });
        div()
            .id(("smelt-search-list-option", row))
            .role(gpui::Role::ListBoxOption)
            .debug_selector(move || format!("smelt-search-list-option-{row}").into())
            .aria_label(option.label.clone())
            .aria_selected(is_current)
            .w_full()
            .h(px(OPTION_HEIGHT))
            .px(px(8.))
            .rounded(px(RADIUS.sm))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_color(foreground)
            .when(is_active, |row| row.bg(rgb(background)))
            .children(option.prefix.as_ref().map(|prefix| prefix()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(option.label.clone()),
            )
            .when_some(option.detail.clone(), |row, detail| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_color(rgb(ui_theme::muted()))
                        .child(detail),
                )
            })
            .when(!option.keybind.is_empty(), |row| {
                row.child(Keybind::new(option.keybind.clone()).backdrop(background))
            })
            .when(self.current.is_some(), |row| {
                row.child(
                    svg()
                        .path(GLYPHS.check.path())
                        .size(px(14.))
                        .flex_shrink_0()
                        .text_color(foreground)
                        .when(!is_current, |check| check.opacity(0.)),
                )
            })
            .when(!option.disabled, |el| {
                el.on_mouse_move(
                    cx.listener(move |this, _: &MouseMoveEvent, _, cx| this.hover(row, cx)),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.choose(row, window, cx)))
            })
            .into_any_element()
    }
}

impl Render for SearchList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let height: Pixels = self.sizes.iter().map(|size| size.height).sum();
        let list = if self.rows.is_empty() {
            div()
                .py(px(16.))
                .flex()
                .justify_center()
                .text_color(rgb(ui_theme::muted()))
                .child("没有匹配的结果")
                .into_any_element()
        } else {
            v_virtual_list(
                cx.entity(),
                "smelt-search-list-rows",
                self.sizes.clone(),
                |this, range, _, cx| {
                    let active = this.active_row();
                    range
                        .map(|row| this.render_row(row, active, cx))
                        .collect::<Vec<_>>()
                },
            )
            .track_scroll(&self.scroll)
            .h(height.min(self.max_height))
            .into_any_element()
        };
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::page_next))
            .on_action(cx.listener(Self::page_previous))
            .on_action(cx.listener(Self::choose_active))
            .flex()
            .flex_col()
            .w_full()
            .map(|list| TYPE.ui_compact.apply(list))
            .child(
                TextInput::new(&self.query)
                    .prefix(Icon::new(GLYPHS.search).text_color(rgb(ui_theme::muted())))
                    // 和卡片融成一块：不要外框，焦点就在这里，不再画焦点线。
                    .bare()
                    .rounded(px(RADIUS.md))
                    .h(px(SEARCH_HEIGHT))
                    .px(px(12.)),
            )
            .child(div().h(px(1.)).bg(ui_theme::hair()))
            .child(div().p(px(4.)).child(list))
    }
}

/// 可搜索的下拉选择。
///
/// ```ignore
/// Combobox::new("theme", options)
///     .selected(Some(current))
///     .on_change(|id, _, cx| select_theme(id.to_string(), cx))
/// ```
#[derive(IntoElement)]
pub struct Combobox {
    id: ElementId,
    groups: Rc<Vec<ComboboxGroup>>,
    selected: Option<SharedString>,
    placeholder: SharedString,
    anchor: Anchor,
    on_change: Option<ChooseHandler>,
    style: StyleRefinement,
}

impl Combobox {
    /// 选项给同一份 `Rc` 时不重新过滤；调用方把它缓存在自己状态上，不要每帧现搭。
    pub fn new(id: impl Into<ElementId>, groups: Rc<Vec<ComboboxGroup>>) -> Self {
        Self {
            id: id.into(),
            groups,
            selected: None,
            placeholder: "请选择".into(),
            anchor: Anchor::TopRight,
            on_change: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn selected(mut self, selected: Option<impl Into<SharedString>>) -> Self {
        self.selected = selected.map(Into::into);
        self
    }

    /// 当前值不在选项里时按钮上的字。
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// 列表的哪个角对着按钮，默认 [`Anchor::TopRight`]：放在行的右边，往左展开不出页面。
    pub fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// 选了一项（包括再选当前那项）。
    pub fn on_change(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

/// 改的是按钮。
impl Styled for Combobox {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Combobox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let list = window.use_keyed_state((self.id.clone(), "list"), cx, |window, cx| {
            SearchList::new("搜索", px(320.), window, cx)
        });
        list.update(cx, |list, cx| {
            list.set_groups(self.groups.clone(), self.selected.clone(), cx)
        });
        let selected = self.selected.as_ref().and_then(|id| {
            self.groups
                .iter()
                .flat_map(|group| &group.options)
                .find(|option| &option.id == id)
        });
        let label = selected.map_or(self.placeholder.clone(), |option| option.label.clone());
        let button = Button::new((self.id.clone(), "button"))
            .small()
            .dropdown_caret(true)
            .accessibility_label(label.clone())
            .max_w(px(240.))
            .refine_style(&self.style)
            .children(selected.and_then(|option| option.prefix.as_ref().map(|prefix| prefix())))
            .child(div().min_w_0().truncate().child(label));
        let focus = list.read(cx).query_focus(cx);
        let on_change = self.on_change;
        Popover::new(self.id)
            .anchor(self.anchor)
            .track_focus(&focus)
            .on_open_change({
                let list = list.clone();
                move |open, window, cx| {
                    if *open {
                        list.update(cx, |list, cx| list.reset(window, cx));
                    }
                }
            })
            .w(px(280.))
            .p_0()
            .rounded(px(RADIUS.md))
            .trigger(button)
            .content(move |_, _, cx| {
                let popover = cx.entity().downgrade();
                list.update(cx, |list, _| {
                    list.set_on_choose(move |id, window, cx| {
                        let _ = popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                        if let Some(on_change) = &on_change {
                            on_change(id, window, cx);
                        }
                    })
                });
                list
            })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn header(group: usize) -> Row {
        Row::Header { group }
    }

    fn opt(group: usize, option: usize) -> Row {
        Row::Option { group, option }
    }

    fn q(query: &str) -> Vec<char> {
        query.chars().map(fold).collect()
    }

    fn groups() -> Vec<ComboboxGroup> {
        vec![
            ComboboxGroup::new(
                Some("Smelt".into()),
                vec![
                    ComboboxOption::new("smelt-dark", "Smelt Dark"),
                    ComboboxOption::new("smelt-light", "Smelt Light"),
                ],
            ),
            ComboboxGroup::new(
                Some("Solarized".into()),
                vec![
                    ComboboxOption::new("solarized-dark", "Solarized Dark").disabled(true),
                    ComboboxOption::new("solarized-light", "Solarized Light"),
                ],
            ),
        ]
    }

    #[test]
    fn score_ignores_case_and_needs_a_subsequence() {
        assert!(score("GitHub Dark", &q("gd")).is_some());
        assert!(score("github dark", &q("GD")).is_some());
        assert_eq!(score("GitHub Dark", &q("dg")), None);
        assert_eq!(score("abc", &q("abcd")), None);
        assert_eq!(score("任意", &q("")), Some(0));
    }

    #[test]
    fn consecutive_and_word_start_matches_rank_higher() {
        // 连着的比散开的高。
        assert!(score("dark", &q("dark")) > score("d-a-r-k", &q("dark")));
        // 落在词头的比落在词中的高。
        assert!(score("Solarized Dark", &q("dark")) > score("darkish", &q("ark")));
        // 不被前面散落的同名字带偏：「solarized」里的 d 起头只有 54，从「dark」起头是
        // 词头 18 扣前面 10 个字、后面三个连着的各 22。
        assert_eq!(score("solarized dark", &q("dark")), Some(74));
        // 驼峰处算词头。
        assert!(score("openFile", &q("f")) > score("offset", &q("f")));
    }

    #[test]
    fn label_outranks_keywords() {
        let by_label = ComboboxOption::new("a", "Terminal");
        let by_keyword = ComboboxOption::new("b", "New tab").keywords(["Terminal"]);
        assert!(option_score(&by_label, &q("term")) > option_score(&by_keyword, &q("term")));
        assert!(option_score(&by_keyword, &q("term")).is_some());
        let by_detail = ComboboxOption::new("c", "Split").detail("Terminal");
        assert!(option_score(&by_detail, &q("term")).is_some());
    }

    #[test]
    fn empty_query_keeps_order_with_headers() {
        let rows = filter(&groups(), "  ");
        assert_eq!(
            rows,
            vec![
                header(0),
                opt(0, 0),
                opt(0, 1),
                header(1),
                opt(1, 0),
                opt(1, 1),
            ]
        );
    }

    #[test]
    fn filtering_hides_empty_groups_and_orders_by_best_score() {
        let rows = filter(&groups(), "LIGHT");
        assert_eq!(rows, vec![header(0), opt(0, 1), header(1), opt(1, 1)]);
        // 只有第二组有匹配：第一组整组连标题都不画。
        assert_eq!(filter(&groups(), "solar light"), vec![header(1), opt(1, 1)]);
        assert!(filter(&groups(), "zzz").is_empty());
        // 没有组名的组不画标题。
        let unlabeled = [ComboboxGroup::new(
            None,
            vec![ComboboxOption::new("x", "X")],
        )];
        assert_eq!(filter(&unlabeled, ""), vec![opt(0, 0)]);
    }

    #[test]
    fn step_wraps_over_enabled_rows() {
        let enabled = [1, 2, 5];
        assert_eq!(step(&enabled, Some(1), 1, true), Some(2));
        assert_eq!(step(&enabled, Some(2), 1, true), Some(5));
        assert_eq!(step(&enabled, Some(5), 1, true), Some(1));
        assert_eq!(step(&enabled, Some(1), -1, true), Some(5));
        assert_eq!(step(&enabled, None, 1, true), Some(1));
        assert_eq!(step(&enabled, None, -1, true), Some(5));
        // 翻页停在两头。
        assert_eq!(step(&enabled, Some(2), 10, false), Some(5));
        assert_eq!(step(&enabled, Some(2), -10, false), Some(1));
        assert_eq!(step(&[], None, 1, true), None);
    }

    struct Harness {
        groups: Rc<Vec<ComboboxGroup>>,
        chosen: Rc<RefCell<Vec<SharedString>>>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let chosen = self.chosen.clone();
            div().size_full().child(
                Combobox::new("combo", self.groups.clone())
                    .selected(Some("smelt-dark"))
                    .anchor(Anchor::TopLeft)
                    .on_change(move |id, _, _| chosen.borrow_mut().push(id.clone())),
            )
        }
    }

    #[gpui::test]
    fn keyboard_skips_disabled_rows_and_enter_chooses(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            crate::init(cx);
        });
        let chosen = Rc::new(RefCell::new(Vec::new()));
        let (_, window) = cx.add_window_view({
            let chosen = chosen.clone();
            move |_, _| Harness {
                groups: Rc::new(groups()),
                chosen,
            }
        });
        window.update(|window, cx| window.draw(cx).clear(cx));
        window.simulate_mouse_down(
            gpui::point(px(10.), px(10.)),
            gpui::MouseButton::Left,
            Default::default(),
        );
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert!(window.debug_bounds("smelt-search-list-option-1").is_some());
        // 当前是 Smelt Dark（第 1 行）。往下：Smelt Light，再往下跳过禁用的 Solarized Dark。
        window.simulate_keystrokes("down ctrl-n enter");
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(chosen.borrow().as_slice(), ["solarized-light"]);
        // 选完就关。
        assert!(window.debug_bounds("smelt-search-list-option-1").is_none());

        // 再开：回到当前值（第一个可选的），往上绕到最后一个可选的。
        window.simulate_mouse_down(
            gpui::point(px(10.), px(10.)),
            gpui::MouseButton::Left,
            Default::default(),
        );
        window.update(|window, cx| window.draw(cx).clear(cx));
        window.simulate_keystrokes("up enter");
        assert_eq!(
            chosen.borrow().as_slice(),
            ["solarized-light", "solarized-light"]
        );

        // 搜索后 Enter 选最匹配的；Esc 关掉不选。
        window.simulate_mouse_down(
            gpui::point(px(10.), px(10.)),
            gpui::MouseButton::Left,
            Default::default(),
        );
        window.update(|window, cx| window.draw(cx).clear(cx));
        window.simulate_input("light");
        window.simulate_keystrokes("enter");
        assert_eq!(
            chosen.borrow().last().map(|id| id.as_ref()),
            Some("smelt-light")
        );
        window.simulate_mouse_down(
            gpui::point(px(10.), px(10.)),
            gpui::MouseButton::Left,
            Default::default(),
        );
        window.update(|window, cx| window.draw(cx).clear(cx));
        window.simulate_keystrokes("escape");
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert!(window.debug_bounds("smelt-search-list-option-1").is_none());
        assert_eq!(chosen.borrow().len(), 3);
    }
}
