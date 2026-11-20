//! 对话左侧的提问导航：一问一条刻度，当前读到的那一问加长加深；指上去出预览，点了跳过去。
//!
//! 动效：刻度亮起、熄灭按 `control` 弹簧伸缩变色，读到哪一问换了、指针扫过刻度，都看得出从哪
//! 条到哪条。预览是悬停卡片，开在刻度侧边，按浮层入场只淡入不挪；指针在刻度间移动时卡片不重播
//! 入场（会闪），按 `overlay` 弹簧滑到新刻度旁边。

use super::*;
use smelt_plugin_api::ui::kit::{MOTION, TYPE};

/// 导航占的左侧留白。正文栏两边都空出这么宽才画。
const RAIL_GUTTER: f32 = 56.;
/// 刻度列离左边的距离和它的宽度（也是指针能点中的宽度）。
const RAIL_LEFT: f32 = 12.;
const RAIL_WIDTH: f32 = 28.;
/// 刻度之间的间距：问少时疏一点，问多时压到看得出一条条为止。
const TICK_PITCH_MAX: f32 = 10.;
const TICK_PITCH_MIN: f32 = 3.;
/// 刻度列上下至少留的空。
const RAIL_MARGIN: f32 = 48.;
const PEEK_WIDTH: f32 = 300.;
/// 刻度平时的长度和亮起（读到的、指着的）时的长度。
const TICK_SHORT: f32 = 8.;
const TICK_LONG: f32 = 14.;

impl ConversationView {
    /// 要 `window` 取弹簧和入场进度，所以在 `render` 里调，结果交给 `view` 摆。
    pub(super) fn render_prompt_rail(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let viewport = self.list_state.viewport_bounds();
        let width = f32::from(viewport.size.width);
        let height = f32::from(viewport.size.height);
        let room = f32::from(ui_theme::conversation_max_width()) + 2. * RAIL_GUTTER;
        if width < room {
            return None;
        }
        let ticks = self.prompt_rail_entries();
        if ticks.len() < 2 {
            return None;
        }
        let pitch = ((height - 2. * RAIL_MARGIN) / ticks.len() as f32)
            .clamp(TICK_PITCH_MIN, TICK_PITCH_MAX);
        let rail_height = pitch * ticks.len() as f32;
        let top = ((height - rail_height) / 2.).max(0.);
        let current = self.reading_prompt_tick(&ticks);
        let hovered = self.prompt_rail_hover.as_ref().map(|item| item.entry_ix);
        let mut rail = div()
            .absolute()
            .left(px(RAIL_LEFT))
            .top(px(top))
            .w(px(RAIL_WIDTH))
            .h(px(rail_height))
            .flex()
            .flex_col()
            .debug_selector(|| "ACP_PROMPT_RAIL".into());
        let dim = gpui::rgb(ui_theme::faint());
        let lit_color = gpui::rgb(ui_theme::text());
        // 弹簧状态按元素 id 挂；套上视图的 id，分屏里几段会话的刻度不串。
        let rail_scope: gpui::ElementId = ("acp-prompt-rail", cx.entity_id()).into();
        for (k, &entry_ix) in ticks.iter().enumerate() {
            let target = if current == Some(k) || hovered == Some(entry_ix) {
                1.
            } else {
                0.
            };
            let lit: f32 = window.with_id(rail_scope.clone(), |window| {
                gpui_base::spring(
                    gpui::ElementId::named_usize("tick-lit", entry_ix),
                    target,
                    MOTION.control,
                    window,
                    cx,
                )
            });
            rail = rail.child(
                div()
                    .id(("acp-prompt-tick", entry_ix))
                    .debug_selector(move || format!("ACP_PROMPT_TICK_{k}"))
                    .w_full()
                    .h(px(pitch))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(
                        div()
                            .h(px(if pitch < 5. { 1. } else { 2. }))
                            .w(px(TICK_SHORT + (TICK_LONG - TICK_SHORT) * lit))
                            .rounded_full()
                            .bg(mix_rgba(dim, lit_color, lit)),
                    )
                    .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                        this.hover_prompt_tick(entry_ix, *hovered, cx);
                    }))
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.jump_to_prompt(entry_ix, cx);
                    })),
            );
        }
        // 卡片的进度和位置只在它开着时取：关掉那一帧没人取，状态就回收了，下次打开从头淡入，
        // 位置也直接落在新刻度旁，不从上次的地方滑过来。
        let peek = self.prompt_rail_hover.as_ref().and_then(|item| {
            let k = ticks.iter().position(|ix| *ix == item.entry_ix)?;
            let y = top + pitch * (k as f32 + 0.5);
            let target = (y - 20.).clamp(8., (height - 120.).max(8.));
            let peek_scope: gpui::ElementId = ("acp-prompt-peek", cx.entity_id()).into();
            let (enter, top) = window.with_id(peek_scope, |window| {
                let enter = overlay_enter("enter", EnterFrom::InPlace, window, cx);
                let top: f32 =
                    gpui_base::spring("top", target, MOTION.overlay.with_epsilon(0.5), window, cx);
                (enter, top)
            });
            Some(render_prompt_peek(item, top, enter.opacity))
        });
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(rail)
                .children(peek)
                .into_any_element(),
        )
    }

    /// 正在读第几问（`ticks` 里的位置）。刚点刻度跳过去的就是那一问；否则是阅读线以上
    /// 最后开始的那一问。阅读线平时贴着视口顶；离底不到一屏时随剩下的距离滑到视口底。
    /// 从头滚到尾它连续扫过整段对话，每一问都轮得到，包括挤在最后一屏里那几问。
    pub(in crate::view) fn reading_prompt_tick(&self, ticks: &[usize]) -> Option<usize> {
        if let Some(jumped) = self.jumped_prompt
            && let Some(k) = ticks.iter().position(|ix| *ix == jumped)
        {
            return Some(k);
        }
        let list = &self.list_state;
        let viewport = list.viewport_bounds();
        let scrolled = -list.scroll_px_offset_for_scrollbar().y;
        let remaining = (list.max_offset_for_scrollbar().y - scrolled).max(px(0.));
        let line = viewport.top() + (viewport.size.height - remaining).max(px(0.)) + px(1.);
        let scroll_top = list.logical_scroll_top().item_ix;
        ticks.iter().rposition(|&ix| {
            // 没加载的那一截和滚过顶的都在阅读线上面；还没量过的在下面。
            ix.checked_sub(self.loaded_entries_offset)
                .is_none_or(|local| {
                    local < scroll_top
                        || list.bounds_for_item(local).is_some_and(|b| b.top() <= line)
                })
        })
    }
}

/// 指着刻度时的预览：提问（最多三行），下面是回答的开头。
fn render_prompt_peek(item: &PromptIndexItem, top: f32, opacity: f32) -> gpui::AnyElement {
    let images = (item.images > 0).then(|| {
        crate::plural(
            item.images,
            t!("images.count.one"),
            t!("images.count.other", count = item.images),
        )
    });
    let prompt = match images {
        Some(images) if item.prompt.is_empty() => images,
        Some(images) => format!("{} · {images}", item.prompt),
        None => item.prompt.clone(),
    };
    div()
        .absolute()
        .left(px(RAIL_LEFT + RAIL_WIDTH + 4.))
        .top(px(top))
        .w(px(PEEK_WIDTH))
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2p5()
        .rounded(ui_theme::card_radius())
        .border_1()
        .border_color(ui_theme::hair())
        .bg(gpui::rgb(ui_theme::surface()))
        .shadow_lg()
        .opacity(opacity)
        .debug_selector(|| "ACP_PROMPT_PEEK".into())
        .child(
            div()
                .type_style(TYPE.text_small_medium)
                .text_color(gpui::rgb(ui_theme::text()))
                .line_clamp(3)
                .child(prompt),
        )
        .when(!item.answer.is_empty(), |card| {
            card.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(gpui::rgb(ui_theme::muted()))
                    .line_clamp(2)
                    .child(item.answer.clone()),
            )
        })
        .into_any_element()
}
