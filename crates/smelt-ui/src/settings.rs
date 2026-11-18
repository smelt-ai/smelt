//! 设置页的骨架：页、分组、行。宿主和插件的设置页都用它，各页长得一样。
//!
//! - [`SettingsPage`]：竖着滚的一列分组。页名在设置的工具栏里（取自导航），这里不画页头。
//! - [`SettingsSection`]：分组。标题和说明在卡片上面，下面一张描边卡片，子项之间自动画
//!   一道缩进的发丝线，调用点不自己画分隔。
//! - [`SettingsRow`]：一行设置。左边名字和说明（说明可以多行），右边控件（子元素）。
//!
//! 卡片里不是 [`SettingsRow`] 的子项（列表、图表、状态说明）也照样放进分组，一样有内边距
//! 和分隔线。样子：卡片铺 `surface`、1px `hair` 描边、不加阴影
//! （`docs/ui-design-language.md`：嵌在内容里的卡片不浮起）；行至少 44 高。字用 rem，
//! 跟着界面字号缩放，和页里别的内容一起变大变小。页自己限宽居中，放在设置里、放在独立
//! 页面里都一样宽。

use gpui::{
    AnyElement, App, ElementId, IntoElement, ParentElement, RenderOnce, ScrollHandle, SharedString,
    Styled, Window, div, prelude::FluentBuilder as _, px, rems, rgb,
};

use crate::kit::{RADIUS, TYPE, TypeStyled as _};
use crate::scrollbar::{Scrollable, ScrollbarAxis};
use crate::ui_theme;

/// 卡片里每一项的左右内边距；分组标题、分隔线跟它对齐。
const CELL_PADDING_X: f32 = 16.;
/// 卡片里每一项的上下内边距。行内容至少 28 高（一个默认按钮），加上它整行 44。
const CELL_PADDING_Y: f32 = 8.;
const ROW_MIN_HEIGHT: f32 = 28.;
/// 分组之间、分组标题和卡片之间的间距。
const SECTION_GAP: f32 = 24.;
const HEADER_GAP: f32 = 8.;
/// 一页最宽多少（rem）：一行的名字和开关离太远就对不上了。窄于它时跟着缩。不用
/// [`SettingsPage`] 的设置页（插件的 JS 设置页）照它限宽。
pub const PAGE_MAX_WIDTH: f32 = 45.;

/// 一页设置：一列 [`SettingsSection`]，超出时竖着滚。`id` 在同一窗口里要唯一；换了 `id`
/// 滚动位置就从头开始（宿主换页时换它）。
#[derive(IntoElement)]
pub struct SettingsPage {
    id: ElementId,
    children: Vec<AnyElement>,
}

impl SettingsPage {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            children: Vec::new(),
        }
    }
}

impl ParentElement for SettingsPage {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for SettingsPage {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll = window
            .use_keyed_state((self.id.clone(), "scroll"), cx, |_, _| ScrollHandle::new())
            .read(cx)
            .clone();
        // 滚动区铺满，内容一列限宽居中：滚动条贴着页边，不贴着那一列。
        let column = div()
            .w_full()
            .max_w(rems(PAGE_MAX_WIDTH))
            .flex()
            .flex_col()
            .gap(px(SECTION_GAP))
            .px(px(24.))
            .pt(px(16.))
            .pb(px(32.))
            .children(self.children);
        let content = div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .child(column);
        Scrollable::new(self.id, content, &scroll, ScrollbarAxis::Vertical)
    }
}

/// 一组设置：可选的标题和说明，下面一张卡片装各项。没有子项时只画标题和说明。
#[derive(IntoElement, Default)]
pub struct SettingsSection {
    title: Option<SharedString>,
    description: Option<SharedString>,
    children: Vec<AnyElement>,
}

impl SettingsSection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// 整组的说明，在标题下面、卡片上面。只属于某一项的话写进那一行的说明。
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl ParentElement for SettingsSection {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for SettingsSection {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let has_header = self.title.is_some() || self.description.is_some();
        let header = has_header.then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(CELL_PADDING_X))
                .when_some(self.title, |this, title| {
                    this.child(
                        div()
                            .type_style(TYPE.ui_medium)
                            .text_color(rgb(ui_theme::muted()))
                            .child(title),
                    )
                })
                .when_some(self.description, |this, description| {
                    this.child(
                        div()
                            .type_style(TYPE.text_xsmall)
                            .text_color(rgb(ui_theme::muted()))
                            .child(description),
                    )
                })
        });
        let card = (!self.children.is_empty()).then(|| {
            let count = self.children.len();
            div()
                .flex()
                .flex_col()
                .py(px(4.))
                .bg(rgb(ui_theme::surface()))
                .border_1()
                .border_color(ui_theme::hair())
                .rounded(px(RADIUS.xl))
                .children(
                    self.children
                        .into_iter()
                        .enumerate()
                        .flat_map(move |(ix, child)| {
                            let cell = div()
                                .px(px(CELL_PADDING_X))
                                .py(px(CELL_PADDING_Y))
                                .child(child)
                                .into_any_element();
                            let divider = (ix + 1 < count).then(|| {
                                div()
                                    .mx(px(CELL_PADDING_X))
                                    .h(px(1.))
                                    .bg(ui_theme::hair())
                                    .into_any_element()
                            });
                            std::iter::once(cell).chain(divider)
                        }),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(HEADER_GAP))
            .children(header)
            .children(card)
    }
}

/// 一行设置：左边名字和说明，右边控件（子元素，横排）。说明里的换行照原样分行。
///
/// 控件要多宽由控件自己定；名字那一侧占剩下的，太窄时说明换行。控件的禁用、读屏名字也由
/// 控件自己给。
#[derive(IntoElement)]
pub struct SettingsRow {
    title: SharedString,
    description: Option<SharedString>,
    children: Vec<AnyElement>,
}

impl SettingsRow {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: None,
            children: Vec::new(),
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl ParentElement for SettingsRow {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for SettingsRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let label = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(2.))
            .child(
                div()
                    .type_style(TYPE.ui)
                    .text_color(rgb(ui_theme::text()))
                    .child(self.title),
            )
            .when_some(self.description, |this, description| {
                this.child(
                    div()
                        .type_style(TYPE.text_small)
                        .text_color(rgb(ui_theme::muted()))
                        .child(description),
                )
            });
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .min_h(px(ROW_MIN_HEIGHT))
            .child(label)
            .when(!self.children.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap(px(8.))
                        .children(self.children),
                )
            })
    }
}
