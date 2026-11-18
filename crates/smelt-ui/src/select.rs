//! 下拉选择：一个写着当前值的按钮，点开是一张菜单，当前那项打勾。跟 macOS 的弹出按钮
//! 一样，选项不多、不用搜。按钮和菜单都是本库的（[`Button`]、[`crate::menu`]），键盘、
//! 弹层位置、长了滚动都归菜单。
//!
//! 值由调用方持有：传进当前值，选了别的由 [`Select::on_change`] 交回去，调用方存好再重画。

use rust_i18n::t;
use std::rc::Rc;

use gpui::{
    Anchor, App, ElementId, IntoElement, RenderOnce, SharedString, StyleRefinement, Styled, Window,
    px,
};
use gpui_base::StyledExt as _;

use crate::button::Button;
use crate::menu::{Menu, MenuItem, MenuTrigger};

type ChangeHandler<T> = Rc<dyn Fn(&T, &mut Window, &mut App)>;

/// 选项是 `(值, 显示的名字)`。当前值不在选项里时按钮写占位字。按钮按名字定宽，默认最宽
/// 240，名字再长就截断；要别的宽度用 `Styled` 改（落在按钮上）。
#[derive(IntoElement)]
pub struct Select<T: Clone + PartialEq + 'static> {
    id: ElementId,
    options: Rc<Vec<(T, SharedString)>>,
    selected: Option<T>,
    placeholder: SharedString,
    disabled: bool,
    on_change: Option<ChangeHandler<T>>,
    style: StyleRefinement,
}

impl<T: Clone + PartialEq + 'static> Select<T> {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            options: Rc::default(),
            selected: None,
            placeholder: t!("select.placeholder").into(),
            disabled: false,
            on_change: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn options(mut self, options: impl IntoIterator<Item = (T, SharedString)>) -> Self {
        self.options = Rc::new(options.into_iter().collect());
        self
    }

    pub fn selected(mut self, selected: Option<T>) -> Self {
        self.selected = selected;
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 选了一项（包括再选当前那项）。
    pub fn on_change(mut self, handler: impl Fn(&T, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl<T: Clone + PartialEq + 'static> Styled for Select<T> {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl<T: Clone + PartialEq + 'static> RenderOnce for Select<T> {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let label = self
            .selected
            .as_ref()
            .and_then(|selected| {
                self.options
                    .iter()
                    .find(|(value, _)| value == selected)
                    .map(|(_, label)| label.clone())
            })
            .unwrap_or(self.placeholder);
        let disabled = self.disabled || self.options.is_empty();
        let button = Button::new((self.id.clone(), "button"))
            .small()
            .label(label)
            .dropdown_caret(true)
            .disabled(disabled)
            .max_w(px(240.))
            .refine_style(&self.style);
        let options = self.options;
        let selected = self.selected;
        let on_change = self.on_change;
        // 放在行的右边，菜单右沿对齐按钮右沿，不往页面外伸。
        MenuTrigger::dropdown(self.id, button, move |_, _| {
            options.iter().fold(Menu::new(), |menu, (value, label)| {
                let item = MenuItem::new(label.clone()).checked(selected.as_ref() == Some(value));
                let item = match on_change.clone() {
                    Some(on_change) => {
                        let value = value.clone();
                        item.on_click(move |_, window, cx| on_change(&value, window, cx))
                    }
                    None => item,
                };
                menu.item(item)
            })
        })
        .anchor(Anchor::TopRight)
        .disabled(disabled)
    }
}
