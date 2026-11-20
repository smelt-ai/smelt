//! 元素 [`BuddyFigure`]：画一个小人演着一出。
//!
//! 按元素 id 留着状态（[`Figure`]）：是哪个小人、正在演的那出、正画着的那一帧、出帧节拍。元素
//! 不再画，状态连同贴图一起放掉。要下一帧时通知这份状态：`use_keyed_state` 的状态由它所在的
//! 视图观察着，通知它就重画那个视图。

use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    App, Context, ElementId, Global, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Subscription, Window, canvas, div, px,
};

use crate::buddy::{Act, Buddy, CANVAS, Playing, Portrait};
use crate::frames::{Clock, Pacer};

/// 开演的时刻最多往回算多久。更早开演的、一直演的那几出（干活、喊你）看着没区别，演一遍的
/// 早演完了；往回算太多，`Instant` 可能减不动（早于开机）。
const LOOKBACK: Duration = Duration::from_secs(3600);

/// 一个小人演着一出，画在 [`crate::SIZE`] 见方的格子里。
#[derive(IntoElement)]
pub struct BuddyFigure {
    id: ElementId,
    seed: SharedString,
    act: Act,
    since_ms: f64,
    still: bool,
}

impl BuddyFigure {
    /// `seed` 定是哪个小人；从 `since_ms`（Unix 毫秒）起演 `act`。`act` 或 `since_ms` 变了算新的
    /// 一出：从头演，换一个随机种子。`id` 在视图里跟着这个小人的身份走，不跟着位置。
    pub fn new(
        id: impl Into<ElementId>,
        seed: impl Into<SharedString>,
        act: Act,
        since_ms: f64,
    ) -> Self {
        Self {
            id: id.into(),
            seed: seed.into(),
            act,
            since_ms,
            still: false,
        }
    }

    /// 定格：画这一出静止的样子，不要下一帧。用户看不见（应用、窗口在后台）或减弱动态效果时用。
    pub fn still(mut self, still: bool) -> Self {
        self.still = still;
        self
    }
}

/// 按元素留着的状态。
struct Figure {
    seed: SharedString,
    buddy: Buddy,
    /// 正在演的那出，和开演时给的 `(act, since_ms)`，比出换没换戏。
    playing: Option<((Act, u64), Playing)>,
    pacer: Rc<Pacer>,
    /// 正画着的那一帧。
    portrait: Portrait,
    _release: Subscription,
}

impl Figure {
    fn new(seed: SharedString, cx: &mut Context<Self>) -> Self {
        let buddy = Buddy::of(&seed);
        prepare(buddy, cx);
        let _release = cx.on_release(|figure: &mut Self, cx| figure.portrait.release(cx));
        Self {
            seed,
            buddy,
            playing: None,
            pacer: Rc::default(),
            portrait: Portrait::default(),
            _release,
        }
    }
}

/// 第一次画完之后把这个小人要的零件都画好（画过的跳过），转身、换动作时不用现画。不在 render 里。
fn prepare(buddy: Buddy, cx: &mut App) {
    cx.defer(move |_| buddy.prepare());
}

impl RenderOnce for BuddyFigure {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let clock = SharedClock::get(cx);
        let now = cx.background_executor().now();
        // 按所在的格取样，同一格里画面不变（`frames`）。
        let tick = clock.tick(now);
        let at = clock.at(tick);
        let figure = window.use_keyed_state(self.id.clone(), cx, {
            let seed = self.seed.clone();
            move |_, cx| Figure::new(seed, cx)
        });
        let (image, animating, pacer) = figure.update(cx, |figure, cx| {
            if figure.seed != self.seed {
                figure.seed = self.seed.clone();
                figure.buddy = Buddy::of(&figure.seed);
                figure.playing = None;
                prepare(figure.buddy, cx);
            }
            let key = (self.act, self.since_ms.to_bits());
            let playing = match figure.playing {
                Some((seen, playing)) if seen == key => playing,
                _ => {
                    let since = started(now, self.since_ms);
                    let playing = Playing::new(&figure.seed, self.act, since, self.since_ms as u64);
                    figure.playing = Some((key, playing));
                    playing
                }
            };
            let (image, animating) =
                figure
                    .portrait
                    .draw(figure.buddy, &playing, at, !self.still, window);
            (image, animating, figure.pacer.clone())
        });
        // 要不要下一帧放到 prepaint 里定：那时才知道这个小人露没露在可见区里。
        let figure = figure.downgrade();
        let pace = canvas(
            move |bounds, window, _| pacer.drew(tick, animating, bounds, clock, figure, window),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        div()
            .relative()
            .size(px(CANVAS))
            .flex_shrink_0()
            .child(image)
            .child(pace)
    }
}

/// Unix 毫秒 `since_ms` 换成单调时钟上的时刻。还没到的算现在，太早的算 [`LOOKBACK`] 以前。
fn started(now: Instant, since_ms: f64) -> Instant {
    let wall_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |since| since.as_secs_f64() * 1000.);
    let ago = ((wall_ms - since_ms) / 1000.).clamp(0., LOOKBACK.as_secs_f64());
    now.checked_sub(Duration::from_secs_f64(ago)).unwrap_or(now)
}

/// 所有小人共用的出帧格：换格时一起画，相位不错开。
struct SharedClock(Clock);

impl Global for SharedClock {}

impl SharedClock {
    fn get(cx: &mut App) -> Clock {
        if let Some(clock) = cx.try_global::<Self>() {
            return clock.0;
        }
        let clock = Clock::new(cx.background_executor().now());
        cx.set_global(Self(clock));
        clock
    }
}
