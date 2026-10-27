//! 让侧栏里某一行看得见，以及「需要你」的行滚出视口时的提示。
//!
//! 行排在哪里要等排版完才知道，render 时拿不到。所以在要看见的那一行、每一行「需要你」
//! 里各挂一个不画东西的 canvas，在 prepaint 量出自己的位置：要看见的那行当场把列表滚过去；
//! 「需要你」的记下来，由列表末尾那个 canvas 算出视口上面、下面各有几个。
//!
//! render 只读上一帧算出的结果来画提示，不在 render 里写任何东西：清空记录、比较、要求
//! 重画都在末尾那个 canvas 的 prepaint 里。结果变了才通知重画一次；提示是浮层，画不画它
//! 都不挪动任何一行，所以重画后算出来一样，不会来回。滚动本来就会重画侧栏，平时不起任何
//! 定时器。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::IconName;

use super::Button;
use crate::Workspace;

/// 视口外「需要你」的行：最近的那一行和一共几行。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OffscreenAttention {
    pub(crate) above: Option<(u64, usize)>,
    pub(crate) below: Option<(u64, usize)>,
}

#[derive(Default)]
pub(crate) struct SidebarReveal {
    /// 要滚到看得见的那一行（会话 `ui_id`）。滚完就清掉。
    target: Cell<Option<u64>>,
    /// 这一帧量到的「需要你」行，末尾算完就清空。
    attention: RefCell<Vec<(u64, Bounds<Pixels>)>>,
    /// 最近一帧末尾算出的结果，render 拿它画提示。
    measured: Cell<OffscreenAttention>,
}

impl SidebarReveal {
    /// 请侧栏下一帧滚到这一行。它已经在视口里就不动。
    pub(crate) fn request(&self, ui_id: u64) {
        self.target.set(Some(ui_id));
    }

    pub(crate) fn target(&self) -> Option<u64> {
        self.target.get()
    }

    /// 视口外的「需要你」：最近一帧算出来的，render 只读它。
    pub(crate) fn offscreen(&self) -> OffscreenAttention {
        self.measured.get()
    }
}

/// 挂在要看见的那一行里：不在视口里就把列表滚到刚好露出它。
pub(crate) fn reveal_anchor(
    reveal: Rc<SidebarReveal>,
    ui_id: u64,
    scroll: ScrollHandle,
    workspace: Entity<Workspace>,
) -> impl IntoElement {
    canvas(
        move |row, _window, cx| {
            if reveal.target.get() != Some(ui_id) {
                return;
            }
            reveal.target.set(None);
            let viewport = scroll.bounds();
            let offset = scroll.offset();
            let next = reveal_offset(
                (row.top(), row.bottom()),
                (viewport.top(), viewport.bottom()),
                offset.y,
                scroll.max_offset().y,
            );
            if next != offset.y {
                scroll.set_offset(point(offset.x, next));
                workspace.update(cx, |_, cx| cx.notify());
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// 挂在每一行「需要你」里：记下它这一帧的位置。
pub(crate) fn attention_anchor(reveal: Rc<SidebarReveal>, ui_id: u64) -> impl IntoElement {
    canvas(
        move |row, _window, _cx| reveal.attention.borrow_mut().push((ui_id, row)),
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// 挂在列表最后：所有行都量完了，算出视口外的「需要你」，清空记录留给下一帧；跟 render
/// 刚才按的结果不一样就再画一帧。
pub(crate) fn attention_checker(
    reveal: Rc<SidebarReveal>,
    scroll: ScrollHandle,
    workspace: Entity<Workspace>,
) -> impl IntoElement {
    canvas(
        move |_, _window, cx| {
            let viewport = scroll.bounds();
            let now = offscreen_attention(
                &reveal.attention.borrow(),
                (viewport.top(), viewport.bottom()),
            );
            reveal.attention.borrow_mut().clear();
            if now != reveal.measured.replace(now) {
                workspace.update(cx, |_, cx| cx.notify());
            }
        },
        |_, _, _, _| {},
    )
    .size_0()
}

/// 视口外「需要你」的提示：浮在列表上沿或下沿，写着有几个，点一下滚到最近的那一行。
/// 外层只管定位，不接鼠标，底下的行照常能点。
pub(crate) fn attention_pill(
    above: bool,
    (target, count): (u64, usize),
    reveal: Rc<SidebarReveal>,
    workspace: Entity<Workspace>,
) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .right_0()
        .when(above, |d| d.top(px(6.)))
        .when(!above, |d| d.bottom(px(6.)))
        .flex()
        .justify_center()
        .child(
            Button::new(if above {
                "sidebar-attention-above"
            } else {
                "sidebar-attention-below"
            })
            .surface()
            .compact()
            .icon(if above {
                IconName::ArrowUp
            } else {
                IconName::ArrowDown
            })
            .label(format!("{count} 个需要你"))
            .on_click(move |_, _, cx| {
                reveal.request(target);
                workspace.update(cx, |_, cx| cx.notify());
            }),
        )
}

/// 把 `row` 滚进视口要的纵向偏移：在上面就让它贴着顶，在下面就贴着底，已经看得见就不动。
/// 偏移跟 `ScrollHandle` 一样往下滚是负数，限在 `[-max_y, 0]`。
fn reveal_offset(
    row: (Pixels, Pixels),
    viewport: (Pixels, Pixels),
    offset_y: Pixels,
    max_y: Pixels,
) -> Pixels {
    let (row_top, row_bottom) = row;
    let (view_top, view_bottom) = viewport;
    let next = if row_top < view_top {
        offset_y + (view_top - row_top)
    } else if row_bottom > view_bottom {
        offset_y - (row_bottom - view_bottom)
    } else {
        offset_y
    };
    next.clamp(-max_y.max(px(0.)), px(0.))
}

/// 完全在视口上面、下面的「需要你」行。露出一部分就算看得见。
fn offscreen_attention(
    rows: &[(u64, Bounds<Pixels>)],
    viewport: (Pixels, Pixels),
) -> OffscreenAttention {
    let (view_top, view_bottom) = viewport;
    let mut out = OffscreenAttention::default();
    for &(ui_id, row) in rows {
        if row.bottom() <= view_top {
            // 上面的取离视口最近的，也就是最靠下的那个。
            let count = out.above.map_or(0, |(_, n)| n) + 1;
            let nearest = match out.above {
                Some((id, _)) if rows_bottom(rows, id) > row.bottom() => id,
                _ => ui_id,
            };
            out.above = Some((nearest, count));
        } else if row.top() >= view_bottom {
            let count = out.below.map_or(0, |(_, n)| n) + 1;
            let nearest = match out.below {
                Some((id, _)) if rows_top(rows, id) < row.top() => id,
                _ => ui_id,
            };
            out.below = Some((nearest, count));
        }
    }
    out
}

fn rows_bottom(rows: &[(u64, Bounds<Pixels>)], ui_id: u64) -> Pixels {
    rows.iter()
        .find(|(id, _)| *id == ui_id)
        .map_or(px(0.), |(_, row)| row.bottom())
}

fn rows_top(rows: &[(u64, Bounds<Pixels>)], ui_id: u64) -> Pixels {
    rows.iter()
        .find(|(id, _)| *id == ui_id)
        .map_or(px(0.), |(_, row)| row.top())
}

#[cfg(test)]
mod tests {
    // 不用 `use super::*`，原因见 `keyboard` 的测试模块。
    use super::{OffscreenAttention, offscreen_attention, reveal_offset};
    use gpui::{Bounds, Pixels, point, px, size};

    fn row(ui_id: u64, top: f32) -> (u64, Bounds<Pixels>) {
        (
            ui_id,
            Bounds::new(point(px(0.), px(top)), size(px(200.), px(32.))),
        )
    }

    #[test]
    fn reveal_scrolls_just_enough_and_stays_put_when_visible() {
        let view = (px(100.), px(500.));
        assert_eq!(
            reveal_offset((px(200.), px(232.)), view, px(-40.), px(800.)),
            px(-40.),
            "看得见就不动"
        );
        assert_eq!(
            reveal_offset((px(60.), px(92.)), view, px(-200.), px(800.)),
            px(-160.),
            "在上面：贴着顶"
        );
        assert_eq!(
            reveal_offset((px(520.), px(552.)), view, px(0.), px(800.)),
            px(-52.),
            "在下面：贴着底"
        );
        assert_eq!(
            reveal_offset((px(60.), px(92.)), view, px(-10.), px(800.)),
            px(0.),
            "不滚过头"
        );
    }

    #[test]
    fn offscreen_attention_counts_each_side_and_points_at_the_nearest() {
        let view = (px(100.), px(500.));
        let rows = [
            row(1, 0.),
            row(2, 40.),
            row(3, 300.),
            row(4, 600.),
            row(5, 700.),
        ];
        assert_eq!(
            offscreen_attention(&rows, view),
            OffscreenAttention {
                above: Some((2, 2)),
                below: Some((4, 2)),
            }
        );
        assert_eq!(
            offscreen_attention(&[row(3, 300.), row(6, 80.)], view),
            OffscreenAttention::default(),
            "露出一部分就算看得见"
        );
    }
}
