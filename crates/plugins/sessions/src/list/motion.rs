//! 列表的动画：组的开合（[`FoldMotion`]）、重排后各行滑到新位置、刚新建的会话落进列表、
//! 会话开始跑时图标亮一下。开合的进度挂在元素状态上；别的要比前后两次侧栏，状态在列表视图上
//! （[`ListMotion`]），在事件里更新，render 只按时间取样。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ElementId, IntoElement, ParentElement as _, Pixels, Styled as _, Window, div,
    px,
};
use gpui_base::{MotionReveal, spring};
use smelt_plugin_api::ui::kit::MOTION;
use smelt_plugin_api::ui::motion::ease_snap;

use super::{Sidebar, reordered};
use crate::model::ConversationKey;

/// 一组开合到哪了：`collapsed` 是要去的状态，`open` 是动画里的当前进度（1 全开、0 全合），
/// 按 [`MOTION`] 的 `disclosure` 弹簧走。标题行的箭头和下面的子行都跟着它。
pub(super) struct FoldMotion {
    collapsed: bool,
    open: f32,
    body: ElementId,
}

impl FoldMotion {
    /// `key` 在整个列表里唯一，弹簧和裁剪层的状态都挂在它上面。
    pub(super) fn new(key: &str, collapsed: bool, window: &mut Window, cx: &mut App) -> Self {
        let target = if collapsed { 0. } else { 1. };
        let open = spring(
            ElementId::Name(format!("fold:{key}").into()),
            target,
            MOTION.disclosure,
            window,
            cx,
        );
        Self {
            collapsed,
            open,
            body: ElementId::Name(format!("fold-body:{key}").into()),
        }
    }

    /// 箭头的角度：收起朝右 0°，展开朝下 90°。
    pub(super) fn angle(&self) -> f32 {
        self.open * 90.
    }

    /// 把这一组的子行放进 `rows`。动画里套一层按进度裁高、淡入淡出；停在收起时不建子行；
    /// 停在展开时直接铺进列表，不留裁剪层（裁剪层量高度晚一帧，子行增减时会先裁掉一截）。
    pub(super) fn body(
        self,
        rows: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
        build: impl FnOnce(&mut Vec<AnyElement>, &mut Window, &mut App),
    ) {
        const SETTLED: f32 = 0.001;
        if self.collapsed && self.open <= SETTLED {
            return;
        }
        if !self.collapsed && self.open >= 1. - SETTLED {
            build(rows, window, cx);
            return;
        }
        let mut body = Vec::new();
        build(&mut body, window, cx);
        let body = div()
            .flex()
            .flex_col()
            .opacity(self.open)
            .children(body)
            .into_any_element();
        rows.push(MotionReveal::new(self.body, self.open, body).into_any_element());
    }
}

/// 新会话那一行撑开到全高要多久，和浮层入场一样长。
const ARRIVAL_OPEN: Duration = Duration::from_millis(240);
/// 落进来那一行铺的底色：淡入、停一下、再慢慢退掉。
const WASH_RISE: Duration = Duration::from_millis(120);
const WASH_HOLD: Duration = Duration::from_millis(360);
const WASH_END: Duration = Duration::from_millis(1100);
/// 底色最浓时的不透明度（铺的是正文色，深浅主题都看得见，又压不住字）。
const WASH_PEAK: f32 = 0.1;

/// 会话开始跑时图标亮一下要多久：亮起、再回到运行色。
const GLOW: Duration = Duration::from_secs(2);

/// 列表视图上的动画状态。
pub(super) struct ListMotion {
    /// 上一次侧栏里各行的先后（`Sidebar::flip_order`），比出重排用。
    order: Vec<String>,
    /// 重排了几次。各行的 `flip_slot` 看到它变了，就从上一帧画的位置滑到新位置。
    reorder: u64,
    /// 按下开始拖时指针离行上沿多远：拖拽牌子画在指针减去它的地方，松手时牌子的上沿就是
    /// 指针减去它。
    grab: Pixels,
    /// 刚松手的那一项和松手时牌子的上沿，等这次放下引起的重排时交给 `landing`。
    dropped: Option<(String, Pixels)>,
    /// 这一次重排里从手放下的地方落下去的那一项；别的行从原处滑过去。
    landing: Option<(String, Pixels)>,
    /// 刚新建的会话从什么时候开始落进列表。
    arrival: Option<(ConversationKey, Instant)>,
    /// 上一次侧栏里在跑的会话，比出哪些是刚开始跑的。
    running: HashSet<ConversationKey>,
    /// 刚开始跑的会话从什么时候开始亮。
    glows: HashMap<ConversationKey, Instant>,
}

/// 这一帧各行要的动画取样。
pub(super) struct Frame {
    pub(super) reorder: u64,
    pub(super) landing: Option<(String, Pixels)>,
    pub(super) arrival: Option<Arrival>,
    /// 刚开始跑的会话图标亮到哪（0 到 1，两头是运行色，中间最亮）。
    pub(super) glows: HashMap<ConversationKey, f32>,
}

/// 落进列表的那一行这一帧的样子。
#[derive(Clone)]
pub(super) struct Arrival {
    pub(super) key: ConversationKey,
    /// 撑开到几成（0 到 1），高度和不透明度都按它。
    pub(super) open: f32,
    /// 铺底的不透明度。
    pub(super) wash: f32,
}

impl ListMotion {
    pub(super) fn new(sidebar: &Sidebar) -> Self {
        Self {
            order: sidebar.flip_order(),
            reorder: 0,
            grab: px(0.),
            dropped: None,
            landing: None,
            arrival: None,
            running: sidebar.running(),
            glows: HashMap::new(),
        }
    }

    pub(super) fn start_drag(&mut self, grab: Pixels) {
        self.grab = grab;
    }

    /// 松手了：`key` 那一项从牌子所在的高度落到新位置。`pointer` 是松手时指针的高度。
    pub(super) fn drop_at(&mut self, key: String, pointer: Pixels) {
        self.dropped = Some((key, pointer - self.grab));
    }

    /// 侧栏重算过了：先后变了就记一次重排，松手留下的落点只给紧接着的这次；刚开始跑的会话
    /// 从 `now` 起亮一下。打开 app、展开一组时露出来的在跑会话不算刚开始跑，不亮。
    pub(super) fn track(&mut self, sidebar: &Sidebar, now: Instant) {
        let order = sidebar.flip_order();
        let dropped = self.dropped.take();
        if reordered(&self.order, &order) {
            self.reorder += 1;
            self.landing = dropped;
        }
        self.order = order;

        let running = sidebar.running();
        for key in running.difference(&self.running) {
            self.glows.insert(key.clone(), now);
        }
        self.glows.retain(|key, _| running.contains(key));
        self.running = running;
    }

    pub(super) fn arrive(&mut self, key: ConversationKey, now: Instant) {
        self.arrival = Some((key, now));
    }

    /// 这一帧的取样，和还要不要下一帧（还有动画没播完）。减弱动态效果时新行直接到全高，
    /// 只留铺底的淡入淡出（不动位置）。图标亮一下是装饰，用户看不见（app、窗口在后台）
    /// 或减弱动态效果时直接不播。
    pub(super) fn frame(
        &mut self,
        now: Instant,
        reduce_motion: bool,
        ambient: bool,
    ) -> (Frame, bool) {
        let arrival = self.arrival.as_ref().and_then(|(key, at)| {
            let elapsed = now.saturating_duration_since(*at);
            (elapsed < WASH_END).then(|| Arrival {
                key: key.clone(),
                open: if reduce_motion {
                    1.
                } else {
                    arrival_open(elapsed)
                },
                wash: arrival_wash(elapsed),
            })
        });
        if arrival.is_none() {
            self.arrival = None;
        }
        if !ambient || reduce_motion {
            self.glows.clear();
        }
        self.glows
            .retain(|_, at| now.saturating_duration_since(*at) < GLOW);
        let glows: HashMap<ConversationKey, f32> = self
            .glows
            .iter()
            .map(|(key, at)| {
                let phase = now.saturating_duration_since(*at).as_secs_f32() / GLOW.as_secs_f32();
                (key.clone(), phase)
            })
            .collect();
        let animating = arrival.is_some() || !glows.is_empty();
        let frame = Frame {
            reorder: self.reorder,
            landing: self.landing.clone(),
            arrival,
            glows,
        };
        (frame, animating)
    }
}

/// 撑开的进度：前段快、收尾慢，跟浮层入场同一条曲线。
fn arrival_open(elapsed: Duration) -> f32 {
    ease_snap(elapsed.as_secs_f32() / ARRIVAL_OPEN.as_secs_f32())
}

/// 铺底：一下子铺满会像闪了一下，所以先用一小段淡入，停住让眼睛找到它，再慢慢退干净。
fn arrival_wash(elapsed: Duration) -> f32 {
    let t = elapsed.as_secs_f32();
    let rise = smoothstep(t / WASH_RISE.as_secs_f32());
    let hold = WASH_HOLD.as_secs_f32();
    let fall = 1. - smoothstep((t - hold) / (WASH_END.as_secs_f32() - hold));
    WASH_PEAK * rise.min(fall)
}

fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}

/// 落进来那一行：外面一层按进度撑开高度、淡入，行底下铺一层底色（`wash` 给行用）。
pub(super) fn arrival_slot(
    arrival: Option<&Arrival>,
    height: Pixels,
    row: AnyElement,
) -> AnyElement {
    match arrival {
        Some(arrival) if arrival.open < 1. => div()
            .h(height * arrival.open)
            .overflow_hidden()
            .opacity(arrival.open)
            .child(row)
            .into_any_element(),
        _ => row,
    }
}

/// 行底下那层底色：放在行的第一个子元素，压在图标和字下面。
pub(super) fn wash(alpha: f32) -> impl IntoElement {
    use smelt_plugin_api::ui::ui_theme;
    div()
        .absolute()
        .inset_0()
        .rounded(ui_theme::row_radius())
        .bg(gpui::rgb(ui_theme::text()))
        .opacity(alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wash_fades_in_holds_and_leaves_nothing_behind() {
        let at = |ms: u64| arrival_wash(Duration::from_millis(ms));
        assert_eq!(at(0), 0.);
        assert!(at(60) > 0. && at(60) < WASH_PEAK);
        assert_eq!(at(200), WASH_PEAK);
        assert_eq!(at(360), WASH_PEAK);
        assert!(at(700) < WASH_PEAK);
        assert!(at(1100) < 1e-4);
        assert_eq!(at(1500), 0.);
    }

    #[test]
    fn the_row_opens_fully_within_the_overlay_time() {
        assert!(arrival_open(Duration::ZERO) < 1e-4);
        assert!(arrival_open(ARRIVAL_OPEN) > 1. - 1e-4);
        assert_eq!(arrival_open(ARRIVAL_OPEN * 2), 1.);
    }
}
