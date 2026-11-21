//! 滚动内容里按 key 认得出的子项：记下各自在哪，问某一项在可视区上面、里面还是下面，把某一项
//! 滚到看得见。和 [`Scrollable`](crate::scrollbar::Scrollable) 用同一个 `ScrollHandle`。
//!
//! 位置只有画了才知道：挂了 key 的子项在 prepaint 里记下上下沿，[`ScrollAnchors::root`] 包着的
//! 内容 prepaint 完了整份收下（换成相对内容顶的位置，和滚到哪无关）。render 里问的是上一帧收下
//! 的位置配眼下的滚动位置，所以滚动时不晚一帧；位置变了（新来、重排、展开）要等这一帧画完才
//! 知道，收下时和上一帧不一样就通知画它的视图再画一次（绘制中 notify 不生效）。
//!
//! 吸顶区段（[`StickySection`](crate::StickySection)）停在顶上时把自己占的高度报上来：可视区
//! 从它下沿算起，压在标题底下的项算在上面；滚到某一项时也不让它落到标题底下。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, EntityId, Global, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, ParentElement, Pixels, ScrollHandle, SharedString,
    Window, point, px,
};
use gpui_base::ElementExt as _;

/// 一项眼下在可视区的哪边。中线在可视区外才算在外面：只露出一条边的看不清，算在外面；露出
/// 一大半的算看得见。距离是中线到可视区那条边有多远，挑最近的一项用。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    Above(f32),
    Visible,
    Below(f32),
}

/// 一块滚动内容的定位表，视图留一份跨帧用（克隆共享同一份）。
#[derive(Clone)]
pub struct ScrollAnchors {
    scroll: ScrollHandle,
    state: Rc<RefCell<State>>,
}

#[derive(Default)]
struct State {
    /// 这一帧 prepaint 里各项记下的上下沿（窗口坐标），收尾时换算后整份挪进 `items`。
    pending: HashMap<SharedString, (f32, f32)>,
    /// 上一帧收下的上下沿，相对内容顶。
    items: HashMap<SharedString, (f32, f32)>,
    /// 这一帧停在顶上的吸顶标题占了可视区顶上多高，收尾时挪进 `inset`。
    pending_inset: f32,
    inset: f32,
    /// 这一帧吸顶标题里最高的那个有多高（停没停住都算），收尾时挪进 `band`。滚到某一项时按
    /// 它留出顶上一条：滚完后哪个标题会停在顶上要滚完才知道。
    pending_band: f32,
    band: f32,
    /// 这一帧可视区顶边（窗口坐标）。吸顶区段按它认自己是不是吸在这块滚动区域上。
    viewport_top: Option<f32>,
    /// 要滚到看得见的那一项。画出来了就滚、然后清掉；还没画出来就留着。
    reveal: Option<SharedString>,
}

impl ScrollAnchors {
    pub fn new(scroll: &ScrollHandle) -> Self {
        Self {
            scroll: scroll.clone(),
            state: Rc::default(),
        }
    }

    /// 包住整块滚动内容（放进 `Scrollable` 的内容里，作它唯一的子元素）：里面挂了 key 的项
    /// 画完后在这里收位置、按要求滚。
    pub fn root(&self, content: impl IntoElement) -> impl IntoElement {
        AnchorRoot {
            anchors: self.clone(),
            child: content.into_any_element(),
        }
    }

    /// 给一项挂上 `key`：prepaint 时记下它在哪。同一帧里 key 不能重。
    pub fn anchor<E: ParentElement>(&self, key: impl Into<SharedString>, element: E) -> E {
        let (state, key) = (self.state.clone(), key.into());
        element.on_prepaint(move |bounds, _, _| {
            let edges = (f32::from(bounds.top()), f32::from(bounds.bottom()));
            state.borrow_mut().pending.insert(key, edges);
        })
    }

    /// 给一项挂上 `key`，记进包着它的最里层那块滚动内容（[`ScrollAnchors::root`]）。手里没有
    /// 定位表的元素（脚本插件交来的）用它；外面没有定位表就不记。
    pub fn anchor_enclosing<E: ParentElement>(key: impl Into<SharedString>, element: E) -> E {
        let key = key.into();
        element.on_prepaint(move |bounds, _, cx| {
            if let Some(anchors) = cx
                .try_global::<ActiveAnchors>()
                .and_then(|active| active.0.last())
            {
                let edges = (f32::from(bounds.top()), f32::from(bounds.bottom()));
                anchors.state.borrow_mut().pending.insert(key, edges);
            }
        })
    }

    /// `key` 那一项眼下在可视区哪边：上一帧的位置，按眼下滚到的位置算。没画过是 `None`。
    pub fn placement(&self, key: &str) -> Option<Placement> {
        let state = self.state.borrow();
        let edges = *state.items.get(key)?;
        let (top, height) = viewport(&self.scroll);
        Some(placement(edges, top + state.inset, top + height))
    }

    /// 上一帧可视区顶上被停住的吸顶标题占了多高。贴着可视区顶边浮的东西让到它下面。
    pub fn top_inset(&self) -> Pixels {
        px(self.state.borrow().inset)
    }

    /// 把 `key` 那一项滚到看得见：画出来了就滚（已经看得见不动），还没画出来（数据还没跟上）
    /// 就等它画出来。`None` 是不用滚了。后来的要求顶掉先前没办完的。
    pub fn reveal(&self, key: Option<SharedString>) {
        self.state.borrow_mut().reveal = key;
    }

    /// 吸顶区段在 prepaint 里报上来：它吸在可视区顶边是 `viewport_top`（窗口坐标）的滚动区域
    /// 上，标题 `header` 高，眼下停在顶上占了 `stuck` 高（没停住是 0）。不是这块滚动区域的
    /// 不算。
    fn sticky_header(&self, viewport_top: f32, header: f32, stuck: f32) {
        let mut state = self.state.borrow_mut();
        let Some(top) = state.viewport_top else {
            return;
        };
        if (top - viewport_top).abs() > 0.5 {
            return;
        }
        state.pending_band = state.pending_band.max(header);
        state.pending_inset = state.pending_inset.max(stuck);
    }

    /// 内容 prepaint 完：收下这一帧的位置和吸顶占的高度，要滚到的那一项画出来了就滚。位置、
    /// 吸顶高度变了，或滚了，就在这一帧画完后通知 `view` 重画。
    fn commit(&self, origin: f32, view: EntityId, cx: &mut App) {
        let mut state = self.state.borrow_mut();
        let items: HashMap<_, _> = std::mem::take(&mut state.pending)
            .into_iter()
            .map(|(key, (top, bottom))| (key, (top - origin, bottom - origin)))
            .collect();
        let inset = std::mem::take(&mut state.pending_inset);
        let mut changed = items != state.items || inset != state.inset;
        state.items = items;
        state.inset = inset;
        state.band = std::mem::take(&mut state.pending_band);
        state.viewport_top = None;
        let target = state
            .reveal
            .as_ref()
            .and_then(|key| state.items.get(key))
            .copied();
        if let Some(edges) = target {
            state.reveal = None;
            let (top, height) = viewport(&self.scroll);
            let max = f32::from(self.scroll.max_offset().y);
            let visible = (top + state.inset, top + height);
            if let Some(offset) = reveal_offset(edges, visible, state.band, height, max) {
                self.scroll
                    .set_offset(point(self.scroll.offset().x, px(offset)));
                changed = true;
            }
        }
        if changed {
            cx.defer(move |cx| cx.notify(view));
        }
    }
}

/// 正在 prepaint 的定位表，里层的在最后。吸顶区段从这里找到自己所在的那一块。
#[derive(Default)]
struct ActiveAnchors(Vec<ScrollAnchors>);

impl Global for ActiveAnchors {}

/// 吸顶区段报自己的标题（见 [`ScrollAnchors::sticky_header`]）。外面没有定位表就不报。
pub(crate) fn report_sticky_header(viewport_top: Pixels, header: Pixels, stuck: Pixels, cx: &App) {
    if let Some(anchors) = cx
        .try_global::<ActiveAnchors>()
        .and_then(|active| active.0.last())
    {
        anchors.sticky_header(f32::from(viewport_top), f32::from(header), f32::from(stuck));
    }
}

/// 可视区：内容里从哪起、多高。
fn viewport(scroll: &ScrollHandle) -> (f32, f32) {
    (
        -f32::from(scroll.offset().y),
        f32::from(scroll.bounds().size.height),
    )
}

/// 上下沿是 `edges` 的一项在可视区（内容里 `visible.0..visible.1`）哪边，按中线算。
fn placement(edges: (f32, f32), visible_top: f32, visible_bottom: f32) -> Placement {
    let center = (edges.0 + edges.1) / 2.;
    if center < visible_top {
        Placement::Above(visible_top - center)
    } else if center > visible_bottom {
        Placement::Below(center - visible_bottom)
    } else {
        Placement::Visible
    }
}

/// 让上下沿是 `edges` 的一项看得见要滚到的位置（`ScrollHandle` 的 y，夹在 `-max..=0`），不用
/// 滚是 `None`。`visible` 是眼下真正看得见的那段（顶上扣掉停住的吸顶标题），整项在里面就不动。
/// 露出一截的少滚一点，刚好整项露出：往上滚时上沿留出 `band`（滚完会停在顶上的标题）；整项在
/// 外面的（从远处跳过来）滚到 `band` 以下那段的中间。
fn reveal_offset(
    edges: (f32, f32),
    visible: (f32, f32),
    band: f32,
    height: f32,
    max: f32,
) -> Option<f32> {
    let (top, bottom) = edges;
    let (visible_top, visible_bottom) = visible;
    if top >= visible_top && bottom <= visible_bottom {
        return None;
    }
    let scroll_top = visible_bottom - height;
    let new_top = if bottom <= scroll_top || top >= visible_bottom {
        (top + bottom) / 2. - (height + band) / 2.
    } else if top < visible_top {
        top - band
    } else {
        bottom - height
    };
    Some((-new_top).clamp(-max, 0.))
}

/// [`ScrollAnchors::root`] 包出来的元素：布局就是里面那个，prepaint 时把定位表摆出来给里面的
/// 吸顶区段找，完了收位置。
struct AnchorRoot {
    anchors: ScrollAnchors,
    child: AnyElement,
}

impl Element for AnchorRoot {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        // 滚动区域先于内容 prepaint：这里的可视区是这一帧的。
        let viewport_top = f32::from(window.content_mask().bounds.top());
        self.anchors.state.borrow_mut().viewport_top = Some(viewport_top);
        cx.default_global::<ActiveAnchors>()
            .0
            .push(self.anchors.clone());
        self.child.prepaint(window, cx);
        cx.default_global::<ActiveAnchors>().0.pop();
        let view = window.current_view();
        self.anchors.commit(f32::from(bounds.top()), view, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 看得见的是 100..300。
    #[test]
    fn placement_goes_by_the_center() {
        let at = |edges| placement(edges, 100., 300.);
        assert_eq!(at((40., 80.)), Placement::Above(40.));
        assert_eq!(at((80., 110.)), Placement::Above(5.), "只露一条边算在上面");
        assert_eq!(at((90., 130.)), Placement::Visible, "露出一大半算看得见");
        assert_eq!(at((310., 350.)), Placement::Below(30.));
    }

    /// 可视区 100..300（`height` 200），最多滚到 500。
    #[test]
    fn revealing_scrolls_only_as_far_as_needed() {
        let reveal = |edges| reveal_offset(edges, (100., 300.), 0., 200., 500.);
        assert_eq!(reveal((150., 190.)), None, "整项看得见不动");
        assert_eq!(reveal((80., 120.)), Some(-80.), "上面露一截：上沿齐顶");
        assert_eq!(reveal((280., 320.)), Some(-120.), "下面露一截：下沿齐底");
        assert_eq!(reveal((480., 520.)), Some(-400.), "远处的滚到中间");
        assert_eq!(reveal((0., 40.)), Some(0.), "滚不过头");
        assert_eq!(reveal((680., 720.)), Some(-500.), "滚不过底");
    }

    /// 内容 100..300 在视口里，顶上 24 被停住的标题占着，标题最高 24。
    #[test]
    fn revealing_keeps_clear_of_sticky_headers() {
        let reveal = |edges| reveal_offset(edges, (124., 300.), 24., 200., 500.);
        assert_eq!(reveal((130., 160.)), None, "在标题下面看得见");
        assert_eq!(
            reveal((110., 140.)),
            Some(-86.),
            "压在标题底下：上沿滚到标题下沿"
        );
        assert_eq!(
            reveal((480., 520.)),
            Some(-388.),
            "远处的滚到标题以下那段的中间"
        );
    }
}
