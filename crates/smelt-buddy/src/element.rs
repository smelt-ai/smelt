//! 元素 [`BuddyFigure`]：画一个小人演着一出。
//!
//! 小人自己是一个视图（[`Figure`]），按元素 id 留在它所在的视图里：是哪个小人、正在演的那出、
//! 正画着的那一帧、出帧节拍。元素不再画，视图连同贴图一起放掉。换帧只通知这个视图，重画的
//! 只有小人这一小块，所在的视图（会话列表）和窗口其余部分从上一帧沿用；所在的视图重画时把
//! 新给的参数交给它，参数没变就不碰它。

use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    App, Context, ElementId, Global, IntoElement, ParentElement as _, Render, RenderOnce,
    SharedString, Styled as _, Subscription, Window, canvas, div, px,
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

/// 所在的视图给小人的参数。
#[derive(Clone, PartialEq)]
struct Cast {
    seed: SharedString,
    act: Act,
    since_ms: f64,
    still: bool,
}

/// 小人这个视图，按元素 id 留着。
struct Figure {
    cast: Cast,
    buddy: Buddy,
    /// 正在演的那出，和开演时给的 `(act, since_ms)`，比出换没换戏。
    playing: Option<((Act, u64), Playing)>,
    pacer: Rc<Pacer>,
    /// 正画着的那一帧。
    portrait: Portrait,
    _release: Subscription,
}

impl Figure {
    fn new(cast: Cast, cx: &mut Context<Self>) -> Self {
        let buddy = Buddy::of(&cast.seed);
        prepare(buddy, cx);
        let _release = cx.on_release(|figure: &mut Self, cx| figure.portrait.release(cx));
        Self {
            cast,
            buddy,
            playing: None,
            pacer: Rc::default(),
            portrait: Portrait::default(),
            _release,
        }
    }

    /// 所在的视图给了新参数。
    fn recast(&mut self, cast: Cast, cx: &mut Context<Self>) {
        if cast.seed != self.cast.seed {
            self.buddy = Buddy::of(&cast.seed);
            self.playing = None;
            prepare(self.buddy, cx);
        }
        self.cast = cast;
        cx.notify();
    }
}

/// 第一次画完之后把这个小人要的零件都画好（画过的跳过），转身、换动作时不用现画。不在 render 里。
fn prepare(buddy: Buddy, cx: &mut App) {
    cx.defer(move |_| buddy.prepare());
}

impl RenderOnce for BuddyFigure {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let cast = Cast {
            seed: self.seed,
            act: self.act,
            since_ms: self.since_ms,
            still: self.still,
        };
        let figure = window.use_keyed_state(self.id, cx, {
            let cast = cast.clone();
            move |_, cx| Figure::new(cast, cx)
        });
        if figure.read(cx).cast != cast {
            figure.update(cx, |figure, cx| figure.recast(cast, cx));
        }
        figure
    }
}

impl Render for Figure {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let clock = SharedClock::get(cx);
        let now = cx.background_executor().now();
        // 按所在的格取样，同一格里画面不变（`frames`）。
        let tick = clock.tick(now);
        let at = clock.at(tick);
        let key = (self.cast.act, self.cast.since_ms.to_bits());
        let playing = match self.playing {
            Some((seen, playing)) if seen == key => playing,
            _ => {
                let since = started(now, self.cast.since_ms);
                let playing = Playing::new(
                    &self.cast.seed,
                    self.cast.act,
                    since,
                    self.cast.since_ms as u64,
                );
                self.playing = Some((key, playing));
                playing
            }
        };
        let (image, animating) =
            self.portrait
                .draw(self.buddy, &playing, at, !self.cast.still, window);
        // 要不要下一帧放到 prepaint 里定：那时才知道这个小人露没露在可见区里。
        let pacer = self.pacer.clone();
        let figure = cx.entity().downgrade();
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
