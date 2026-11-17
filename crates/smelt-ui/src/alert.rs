//! 提示块：页面里一段要人看到的说明，跟内容一起排，不浮起，`md` 圆角、正文色的字。读屏按
//! alert 播报。两种，颜色守设计语言的状态色：
//! - [`Alert::error`]：出错了（读不到配置、保存失败），`bad` 浅底、圆圈感叹号。
//! - [`Alert::need`]：做之前要你知道的后果（会替换同名的、会盖过别处的），`need` 浅底、三角感叹号。
//!
//! 「完成」这类结果不用提示块：没有要人处理的事，一行弱色的字就够。

use gpui::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce, Role,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div, px, rgb,
};

use crate::icon::{Icon, IconName};
use crate::kit::RADIUS;
use crate::ui_theme;

#[derive(IntoElement)]
pub struct Alert {
    id: ElementId,
    tone: Tone,
    message: SharedString,
}

#[derive(Clone, Copy)]
enum Tone {
    Error,
    Need,
}

impl Alert {
    pub fn error(id: impl Into<ElementId>, message: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            tone: Tone::Error,
            message: message.into(),
        }
    }

    pub fn need(id: impl Into<ElementId>, message: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            tone: Tone::Need,
            message: message.into(),
        }
    }
}

impl RenderOnce for Alert {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (color, icon) = match self.tone {
            Tone::Error => (ui_theme::bad(), IconName::CircleAlert),
            Tone::Need => (ui_theme::need(), IconName::TriangleAlert),
        };
        let fill = ui_theme::flatten(ui_theme::tint(color, 0x1a), ui_theme::bg());
        let mark = ui_theme::legible(color, fill, 3.);
        let text = ui_theme::legible(ui_theme::text(), fill, 4.5);
        div()
            .id(self.id)
            .role(Role::Alert)
            .w_full()
            .flex()
            .items_start()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(px(RADIUS.md))
            .bg(rgb(fill))
            .text_sm()
            .text_color(rgb(text))
            .child(
                // 图标和第一行字对齐：14 的图标在 20 的行高里下移 3。
                Icon::new(icon).size_3p5().mt(px(3.)).text_color(rgb(mark)),
            )
            .child(div().flex_1().min_w_0().child(self.message))
    }
}
