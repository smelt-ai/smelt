//! 对话左侧的提问导航：一问一条刻度，当前读到的那一问加长加深；指上去出预览，点了跳过去。
//!
//! 按迁到 JS 后的分工写成两半（docs/app-plugins.md「终态：各功能怎么分」）：
//! - 数据和动作留在对话能力里：[`PromptRail`]（有哪几问、正在读哪一问、视口多大）、预览那一问
//!   的内容（`prompt_rail_item`）、跳到某一问（`jump_to_prompt`）。正在读哪一问要看消息列表
//!   量过的位置，只有宿主看得见。
//! - 画法将来是 JS 插件：[`render_prompt_rail`] 只读 `PromptRail` 和指着的那一问，不碰
//!   `ConversationView` 的字段。指着哪一问是界面自己的临时状态，跟着画法走。
//!
//! 动效：刻度亮起、熄灭按 `control` 弹簧伸缩变色，读到哪一问换了、指针扫过刻度，都看得出从哪
//! 条到哪条。预览是悬停卡片，开在刻度侧边，按浮层入场只淡入不挪；指针在刻度间移动时卡片不重播
//! 入场（会闪），按 `overlay` 弹簧滑到新刻度旁边。
//!
//! 动效只用 gpui-shell 脚本里也写得出的：宽度、透明度、位置的弹簧。脚本的弹簧动不了颜色，所以
//! 刻度变色是 `faint` 底上叠一层 `text`、动它的透明度。例外是预览卡入场的淡入：新挂上的元素
//! 弹簧首帧就到位，脚本里还没有入场零件，见文档「已知缺口」。

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

/// 提问导航要的数据，将来由对话能力给 JS 插件。
pub(in crate::view) struct PromptRail {
    /// 每一问的全局条目下标，按先后排。
    pub prompts: Vec<usize>,
    /// 正在读的那一问（全局条目下标）。
    pub reading: Option<usize>,
    /// 消息流视口的大小：留白够不够画、刻度怎么排。
    pub viewport: gpui::Size<gpui::Pixels>,
}

impl ConversationView {
    pub(in crate::view) fn prompt_rail(&self) -> PromptRail {
        let prompts = self.prompt_rail_entries();
        let reading = self.reading_prompt(&prompts);
        PromptRail {
            prompts,
            reading,
            viewport: self.list_state.viewport_bounds().size,
        }
    }

    /// 正在读哪一问（`prompts` 里的全局条目下标）。刚点刻度跳过去的就是那一问；否则是阅读线
    /// 以上最后开始的那一问。阅读线平时贴着视口顶；离底不到一屏时随剩下的距离滑到视口底。
    /// 从头滚到尾它连续扫过整段对话，每一问都轮得到，包括挤在最后一屏里那几问。
    fn reading_prompt(&self, prompts: &[usize]) -> Option<usize> {
        if let Some(jumped) = self.jumped_prompt
            && prompts.contains(&jumped)
        {
            return Some(jumped);
        }
        let list = &self.list_state;
        let viewport = list.viewport_bounds();
        let scrolled = -list.scroll_px_offset_for_scrollbar().y;
        let remaining = (list.max_offset_for_scrollbar().y - scrolled).max(px(0.));
        let line = viewport.top() + (viewport.size.height - remaining).max(px(0.)) + px(1.);
        let scroll_top = list.logical_scroll_top().item_ix;
        prompts.iter().copied().rfind(|&ix| {
            // 没加载的那一截和滚过顶的都在阅读线上面；还没量过的在下面。
            ix.checked_sub(self.loaded_entries_offset)
                .is_none_or(|local| {
                    local < scroll_top
                        || list.bounds_for_item(local).is_some_and(|b| b.top() <= line)
                })
        })
    }
}

/// 消息流两边的留白够不够画导航。不够就不用取 [`PromptRail`]：它每次都要扫一遍条目。
pub(super) fn prompt_rail_fits(viewport: gpui::Size<gpui::Pixels>) -> bool {
    viewport.width >= ui_theme::conversation_max_width() + px(2. * RAIL_GUTTER)
}

/// 画导航。要 `window` 取弹簧和入场进度，所以在 `render` 里调，结果交给 `view` 摆。
/// `hover` 是指针停着的那一问。
pub(super) fn render_prompt_rail(
    rail: &PromptRail,
    hover: Option<&PromptIndexItem>,
    window: &mut Window,
    cx: &mut Context<ConversationView>,
) -> Option<gpui::AnyElement> {
    if !prompt_rail_fits(rail.viewport) || rail.prompts.len() < 2 {
        return None;
    }
    let height = f32::from(rail.viewport.height);
    let count = rail.prompts.len() as f32;
    let pitch = ((height - 2. * RAIL_MARGIN) / count).clamp(TICK_PITCH_MIN, TICK_PITCH_MAX);
    let rail_height = pitch * count;
    let top = ((height - rail_height) / 2.).max(0.);
    let hovered = hover.map(|item| item.entry_ix);
    let mut ticks = div()
        .absolute()
        .left(px(RAIL_LEFT))
        .top(px(top))
        .w(px(RAIL_WIDTH))
        .h(px(rail_height))
        .flex()
        .flex_col()
        .debug_selector(|| "ACP_PROMPT_RAIL".into());
    // 弹簧状态按元素 id 挂；套上视图的 id，分屏里几段会话的刻度不串。
    let rail_scope: gpui::ElementId = ("acp-prompt-rail", cx.entity_id()).into();
    for (k, &entry_ix) in rail.prompts.iter().enumerate() {
        let target = if rail.reading == Some(entry_ix) || hovered == Some(entry_ix) {
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
        ticks = ticks.child(
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
                        .relative()
                        .h(px(if pitch < 5. { 1. } else { 2. }))
                        .w(px(TICK_SHORT + (TICK_LONG - TICK_SHORT) * lit))
                        .rounded_full()
                        .bg(gpui::rgb(ui_theme::faint()))
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .rounded_full()
                                .bg(gpui::rgb(ui_theme::text()))
                                .opacity(lit),
                        ),
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
    let peek = hover.and_then(|item| {
        let k = rail.prompts.iter().position(|ix| *ix == item.entry_ix)?;
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
            .child(ticks)
            .children(peek)
            .into_any_element(),
    )
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
