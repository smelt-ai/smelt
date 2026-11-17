//! 徽标：贴在标题、行尾的一小块标记（「默认」、数量、「已登录」）：
//! 16 高、左右 4、`xs` 圆角、caption 字。
//!
//! 颜色守设计语言：平常是灰（[`Badge::new`]）；要压过周围的（页签上的未读数、「默认」）用
//! `ink` 实底；状态色只给状态：`ok` 正常、`need` 要你处理、`bad` 出错，都是浅底配同色字。
//! 分类、名字这类不是状态的标记用灰，不借状态色。

use gpui::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::StyledExt as _;

use crate::kit::{RADIUS, TYPE};
use crate::ui_theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Tone {
    #[default]
    Neutral,
    Accent,
    Ok,
    Need,
    Bad,
}

#[derive(IntoElement)]
pub struct Badge {
    tone: Tone,
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl Badge {
    /// 灰：`soft` 底、`muted` 字、一圈 `hair`。
    pub fn new() -> Self {
        Self {
            tone: Tone::Neutral,
            style: StyleRefinement::default(),
            children: Vec::new(),
        }
    }

    /// `ink` 实底：需要压过周围的少数标记。
    pub fn accent(mut self) -> Self {
        self.tone = Tone::Accent;
        self
    }

    /// 状态正常（已登录、已配置）。
    pub fn ok(mut self) -> Self {
        self.tone = Tone::Ok;
        self
    }

    /// 要你处理。
    pub fn need(mut self) -> Self {
        self.tone = Tone::Need;
        self
    }

    /// 出错、不可用。
    pub fn bad(mut self) -> Self {
        self.tone = Tone::Bad;
        self
    }
}

impl Default for Badge {
    fn default() -> Self {
        Self::new()
    }
}

/// 底和字。状态色的浅底按铺在 `bg` 上算实色，字对它至少 4.5:1。
fn colors(tone: Tone) -> (u32, u32) {
    let tinted = |color: u32| {
        let fill = ui_theme::flatten(ui_theme::tint(color, 0x24), ui_theme::bg());
        (fill, ui_theme::legible(color, fill, 4.5))
    };
    match tone {
        Tone::Neutral => {
            let fill = ui_theme::soft();
            (fill, ui_theme::legible(ui_theme::muted(), fill, 4.5))
        }
        Tone::Accent => (ui_theme::ink(), ui_theme::on_ink()),
        Tone::Ok => tinted(ui_theme::ok()),
        Tone::Need => tinted(ui_theme::need()),
        Tone::Bad => tinted(ui_theme::bad()),
    }
}

impl ParentElement for Badge {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for Badge {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Badge {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (fill, fg) = colors(self.tone);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(16.))
            .px(px(4.))
            .rounded(px(RADIUS.xs))
            .bg(rgb(fill))
            .when(self.tone == Tone::Neutral, |this| {
                this.border(px(0.5)).border_color(ui_theme::hair())
            })
            .text_color(rgb(fg))
            .map(|this| TYPE.small_medium.lh(11.).apply(this))
            .font_features(ui_theme::tabular_figures())
            .whitespace_nowrap()
            .children(self.children)
            .refine_style(&self.style)
    }
}
