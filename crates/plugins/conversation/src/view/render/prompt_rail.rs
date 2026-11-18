//! 对话左侧的提问导航：一问一条刻度，当前读到的那一问加长加深；指上去出预览，点了跳过去。

use super::*;
use smelt_plugin_api::ui::kit::TYPE;

/// 导航占的左侧留白。正文栏两边都空出这么宽才画，窄了就只靠吸顶胶囊。
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

impl ConversationView {
    pub(super) fn render_prompt_rail(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
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
        let reading = self.loaded_entries_offset + self.list_state.logical_scroll_top().item_ix;
        let current = ticks.iter().rposition(|ix| *ix <= reading);
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
        for (k, &entry_ix) in ticks.iter().enumerate() {
            let is_current = current == Some(k);
            let is_hovered = hovered == Some(entry_ix);
            let color = if is_current || is_hovered {
                ui_theme::text()
            } else {
                ui_theme::faint()
            };
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
                            .w(px(if is_current || is_hovered { 14. } else { 8. }))
                            .rounded_full()
                            .bg(gpui::rgb(color)),
                    )
                    .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                        this.hover_prompt_tick(entry_ix, *hovered, cx);
                    }))
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.jump_to_prompt(entry_ix, cx);
                    })),
            );
        }
        let peek = self.prompt_rail_hover.as_ref().and_then(|item| {
            let k = ticks.iter().position(|ix| *ix == item.entry_ix)?;
            let y = top + pitch * (k as f32 + 0.5);
            Some(render_prompt_peek(
                item,
                (y - 20.).clamp(8., (height - 120.).max(8.)),
            ))
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
}

/// 指着刻度时的预览：提问（最多三行），下面是回答的开头。
fn render_prompt_peek(item: &PromptIndexItem, top: f32) -> gpui::AnyElement {
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
