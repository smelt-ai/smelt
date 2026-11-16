//! 舞台顶上浮着的东西：可见舞台身份的胶囊，舞台贴着窗口顶时还有拖窗口的条。
//!
//! 标题不拉成通栏。它是舞台正中的一颗胶囊，写活动会话（项目会话或侧栏对话）的名字。
//! 对话里最近一问整条滚出视口后，胶囊换成那一问，按下滚回去。
//!
//! `impl Workspace` 方法，字段仍在 main.rs。

use gpui::StatefulInteractiveElement;
use gpui::*;
use gpui_component::*;

use crate::{Workspace, acp_view, ui_theme, workspace_frame};
use smelt_ui::truncated_line::truncated_line;

/// 舞台顶上的标题槽。没有会话、名字是空的时候不画。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChromeTitle {
    Hidden,
    Visible {
        title: String,
        model: Option<String>,
        /// 标题所属的对话。它的提问滚出视口后，胶囊换成那一问。
        conversation: Option<Entity<acp_view::AcpView>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChromeSessionTitle {
    pub title: String,
    pub model: Option<String>,
    pub conversation: Option<Entity<acp_view::AcpView>>,
}

impl ChromeTitle {
    fn from_session(session: Option<ChromeSessionTitle>) -> Self {
        match session {
            Some(session) if !session.title.trim().is_empty() => Self::Visible {
                title: session.title,
                model: session.model,
                conversation: session.conversation,
            },
            _ => Self::Hidden,
        }
    }
}

/// 舞台贴着窗口顶（单独一组、没有标签条）时，顶上这一条拖窗口。
///
/// `left_guard` 见 [`Workspace::chrome_left_guard`]：`None` 是舞台不贴窗口顶，`Some` 是
/// 左边要让出的宽度（贴左上角时是红黄绿按钮和左侧开合按钮）。返回值是 `(left, height)`。
pub(crate) fn stage_content_drag_band(left_guard: Option<Pixels>) -> Option<(Pixels, Pixels)> {
    left_guard.map(|left| (left, workspace_frame::TOP_BAR_HEIGHT))
}

/// 静止宽度。胶囊浮在内容第一行正中，左右常是终端自己的路径和用量，只能留一个短名字。
const CHROME_ISLAND_MAX_WIDTH: f32 = 200.;

/// 吸顶提问只出现在对话上，要认得出是哪一问，比名字宽。
const CHROME_PROMPT_ISLAND_MAX_WIDTH: f32 = 480.;

/// 胶囊外壳。悬停说完整身份：名字 · 模型。
fn chrome_island(id: &'static str, max_width: f32, tip: String) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(ui_theme::CHROME_ISLAND_HEIGHT_PX))
        .max_w(px(max_width))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        // 浮层实色：平时一档卡片底，指上去再抬到悬停色。不透、不糊。
        .bg(gpui::rgb(ui_theme::surface()))
        .border_1()
        .border_color(ui_theme::hair())
        .hover(|island| island.bg(rgb(ui_theme::soft())))
        .shadow_lg()
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
        })
}

/// 舞台标题胶囊。只显示名字；模型留在悬停和输入条上。
fn chrome_title_island(title: String, tip: String) -> Stateful<Div> {
    workspace_frame::with_window_drag(
        chrome_island("stage-title", CHROME_ISLAND_MAX_WIDTH, tip).child(
            div()
                .min_w(px(0.))
                .overflow_hidden()
                .text_sm()
                .font_semibold()
                .text_color(rgb(ui_theme::text()))
                .child(truncated_line(title)),
        ),
    )
}

/// 吸顶提问。按下滚回那一问，所以不当拖窗口的把手。
fn chrome_prompt_island(
    prompt: String,
    tip: String,
    conversation: Entity<acp_view::AcpView>,
) -> Stateful<Div> {
    chrome_island("stage-pinned-prompt", CHROME_PROMPT_ISLAND_MAX_WIDTH, tip)
        .gap_1p5()
        .child(
            div()
                .flex_shrink_0()
                .text_sm()
                .text_color(rgb(ui_theme::muted()))
                .child("↑"),
        )
        .child(
            div()
                .min_w(px(0.))
                .overflow_hidden()
                .text_sm()
                .text_color(rgb(ui_theme::text()))
                .child(truncated_line(prompt)),
        )
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            conversation.update(cx, |view, cx| view.reveal_pinned_prompt(cx));
        })
}

impl Workspace {
    /// 舞台上的页头、拖窗口条从哪起，见 [`Self::chrome_left_guard_of`]。
    pub(crate) fn chrome_left_guard(&self, window: &Window) -> Option<Pixels> {
        self.chrome_left_guard_of(crate::builtin_views::Builtin::Stage.id(), window)
    }

    /// 一个视图的页头、拖窗口条从哪起。它（或装着它的出口）单独一组、没有标签条时贴着
    /// 窗口顶，返回左边要让出的宽度：贴左上角时让红黄绿按钮和左侧开合按钮，不然是 0。在
    /// 标签条下面时是 `None`：顶上那条归标签条，页头不用再留拖窗口的高度。只读缓存（见
    /// `workspace_layout`）。
    pub(crate) fn chrome_left_guard_of(&self, view: &str, window: &Window) -> Option<Pixels> {
        let view = self.shell_view.panel_of(view);
        self.shell_view.bare.contains(view).then(|| {
            if self.shell_view.top_left.as_deref() == Some(view) {
                self.shell_view.leading_inset(window)
            } else {
                px(0.)
            }
        })
    }

    /// 自己画顶上那条的宿主视图（设置）的标题行，按它此刻摆在哪让位，见
    /// `workspace_frame::page_chrome`。
    pub(crate) fn page_chrome_of(&self, view: &str, window: &Window) -> Div {
        crate::workspace_frame::page_chrome(
            self.chrome_left_guard_of(view, window),
            self.chrome_right_inset_of(view),
        )
    }

    /// 一个视图贴着窗口右上角时，页头末尾要给右侧开合按钮让出的宽度；不贴着是 0。
    pub(crate) fn chrome_right_inset_of(&self, view: &str) -> Pixels {
        let view = self.shell_view.panel_of(view);
        if self.shell_view.bare.contains(view) && self.shell_view.top_right.as_deref() == Some(view)
        {
            self.shell_view.trailing_inset()
        } else {
            px(0.)
        }
    }

    /// 舞台画的那个会话（见 `render_stage_session`）的名字。
    fn chrome_title(&self, cx: &App) -> ChromeTitle {
        let session = self.sessions.get(self.active_session).filter(|session| {
            !session.is_product_conversation(cx) || session.is_agent_conversation(cx)
        });
        ChromeTitle::from_session(session.map(|session| {
            let conversation = session.active_acp().cloned();
            ChromeSessionTitle {
                title: Self::title_with_runtime(session, cx),
                model: conversation
                    .as_ref()
                    .and_then(|view| view.read(cx).model_name()),
                conversation,
            }
        }))
    }

    /// 浮在舞台内容上的标题胶囊，以及舞台贴窗口顶时的拖窗口条。不占布局高度。
    pub(crate) fn render_stage_chrome(
        &mut self,
        left_guard: Option<Pixels>,
        cx: &mut Context<Self>,
    ) -> Div {
        let island = match self.chrome_title(cx) {
            ChromeTitle::Hidden => None,
            ChromeTitle::Visible {
                title,
                model,
                conversation,
            } => {
                let tip = match &model {
                    Some(model) if !model.is_empty() => format!("{title} · {model}"),
                    _ => title.clone(),
                };
                let pinned = conversation.and_then(|view| {
                    let prompt = view.read(cx).pinned_prompt()?;
                    Some((prompt, view))
                });
                Some(match pinned {
                    Some((prompt, view)) => chrome_prompt_island(prompt, tip, view),
                    None => chrome_title_island(title, tip),
                })
            }
        };
        let drag_band = stage_content_drag_band(left_guard);
        div()
            .absolute()
            .inset_0()
            // 拖动条在胶囊下面。点到胶囊时胶囊先拦住，不会把窗口拖走。
            .children(drag_band.map(|(left, height)| {
                workspace_frame::with_window_drag(
                    div().absolute().top_0().left(left).right_0().h(height),
                )
            }))
            .children(island.map(|island| {
                div()
                    .absolute()
                    .top(px(ui_theme::CHROME_ISLAND_TOP_PX))
                    .left(left_guard.unwrap_or_default())
                    .right_0()
                    .h(px(ui_theme::CHROME_ISLAND_HEIGHT_PX))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(island)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{ChromeSessionTitle, ChromeTitle, stage_content_drag_band};
    use crate::workspace_frame;
    use gpui::px;

    #[test]
    fn the_chrome_shows_the_session_title() {
        let title = ChromeSessionTitle {
            title: "查登录".into(),
            model: Some("grok-4".into()),
            conversation: None,
        };
        assert_eq!(
            ChromeTitle::from_session(Some(title)),
            ChromeTitle::Visible {
                title: "查登录".into(),
                model: Some("grok-4".into()),
                conversation: None,
            },
        );
        assert_eq!(ChromeTitle::from_session(None), ChromeTitle::Hidden);
    }

    #[test]
    fn blank_titles_do_not_occupy_the_chrome() {
        assert_eq!(
            ChromeTitle::from_session(Some(ChromeSessionTitle {
                title: "   ".into(),
                model: None,
                conversation: None,
            })),
            ChromeTitle::Hidden,
        );
    }

    #[test]
    fn a_stage_at_the_window_top_drags_there() {
        let band = stage_content_drag_band(Some(px(0.))).expect("贴窗口顶要能拖");
        assert_eq!(band, (px(0.), workspace_frame::TOP_BAR_HEIGHT));

        let corner = stage_content_drag_band(Some(px(80.))).expect("左上角让开红黄绿");
        assert_eq!(corner.0, px(80.));

        assert!(
            stage_content_drag_band(None).is_none(),
            "在标签条下面，拖窗口归标签条"
        );
    }
}
