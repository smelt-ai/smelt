//! 滚到可视区外的、要你看一眼的会话：在列表那一侧的边上浮一个小胶囊（「↓ 2 个需要你」），
//! 点一下打开最近的那个；打开了就算看过，再点就轮到下一个。最近的是折起来的一组，打开组里
//! 第一段要你看的（打开时会展开它所在的组）。打开后滚过去由列表在选中变了时做
//! （`ScrollAnchors::reveal`），从通知、命令跳过来的选中也走那一条。
//!
//! 要你看的有两种：需要你处理的，和有没看过的提醒的（跑完了、失败了你还没看）。在跑的、闲着
//! 的不算，算上了胶囊会常驻，等于没提示。正选中的那段不算，人正看着它。折起来的一组里有，就
//! 指向那一组的标题，数里面有几段。哪些行要盯由 `Sidebar` 在事件里算好（[`Notice`]）；行在
//! 可视区哪边问列表的 `ScrollAnchors`（压在吸顶标题底下的算在上面）。

use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, IntoElement, ParentElement, Pixels, Styled as _, div, px};
use rust_i18n::t;
use smelt_plugin_api::ui::IconName;
use smelt_plugin_api::ui::{Button, Placement};

use crate::ops;
use crate::model::{ConversationKey, FailTarget};

/// 一处有几段要你看的会话。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Tally {
    /// 需要你处理的。
    pub(super) need: usize,
    /// 不需要你处理、但有没看过的提醒的。
    pub(super) unread: usize,
}

impl Tally {
    pub(super) fn is_empty(self) -> bool {
        self.need == 0 && self.unread == 0
    }

    pub(super) fn add(&mut self, other: Tally) {
        self.need += other.need;
        self.unread += other.unread;
    }

    fn label(self) -> String {
        let need = (self.need > 0).then(|| t!("offscreen.need", count = self.need).into_owned());
        let unread =
            (self.unread > 0).then(|| t!("offscreen.unread", count = self.unread).into_owned());
        need.into_iter()
            .chain(unread)
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// 列表上要盯的一行：会话行，或折起来的一组的标题。`key` 是那一行的滑动身份
/// （`Sidebar::flip_order` 里的，也是它在 `ScrollAnchors` 里的 key），`session` 是点胶囊时
/// 打开的会话：会话行就是它自己，组标题是组里第一段要你看的。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Notice {
    pub(super) key: String,
    pub(super) tally: Tally,
    pub(super) session: ConversationKey,
}

/// 一边的提示：有几段，点了打开哪段（最近的那一行的）。
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Side {
    pub(super) tally: Tally,
    nearest: Option<(f32, ConversationKey)>,
}

/// 上下两边各有什么在可视区外。
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Hint {
    pub(super) above: Side,
    pub(super) below: Side,
}

/// 按各行在可视区哪边（`placement`，没画过的是 `None`）算两边。
pub(super) fn hint(notices: &[Notice], placement: impl Fn(&str) -> Option<Placement>) -> Hint {
    let mut hint = Hint::default();
    for notice in notices {
        let (side, distance) = match placement(&notice.key) {
            Some(Placement::Above(distance)) => (&mut hint.above, distance),
            Some(Placement::Below(distance)) => (&mut hint.below, distance),
            Some(Placement::Visible) | None => continue,
        };
        side.tally.add(notice.tally);
        if side
            .nearest
            .as_ref()
            .is_none_or(|(best, _)| distance < *best)
        {
            side.nearest = Some((distance, notice.session.clone()));
        }
    }
    hint
}

/// 浮在列表上下边上的两个胶囊，点了打开那一边最近的那段会话。叠在列表上，父元素要
/// `relative`；`top` 是列表顶上被吸顶标题占着的高度，上边的胶囊让到它下面。
pub(super) fn pills(hint: Hint, top: Pixels) -> Vec<AnyElement> {
    [(hint.above, true), (hint.below, false)]
        .into_iter()
        .filter_map(|(side, up)| {
            let (_, session) = side.nearest.filter(|_| !side.tally.is_empty())?;
            let button = Button::new(if up {
                "sessions-offscreen-above"
            } else {
                "sessions-offscreen-below"
            })
            .floating()
            .small()
            .icon(if up {
                IconName::ArrowUp
            } else {
                IconName::ArrowDown
            })
            .label(side.tally.label())
            .on_click(move |_, _, cx| {
                // 出口里可能正放着别的页（归档这些）：和点那一行一样，打开会话视图再选中。
                let opened = ops::open_conversation(session.clone(), cx);
                let target = FailTarget::Conversation(session.clone());
                crate::report_row_failure(target, t!("action.open_session"), opened, cx);
            });
            Some(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .when(up, |pill| pill.top(top + px(8.)))
                    .when(!up, |pill| pill.bottom(px(8.)))
                    .flex()
                    .justify_center()
                    .child(button)
                    .into_any_element(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(key: &str, need: usize, unread: usize) -> Notice {
        Notice {
            key: key.to_string(),
            tally: Tally { need, unread },
            session: ConversationKey {
                machine: smelt_plugin_api::RuntimeKey::Local,
                id: key.to_string(),
            },
        }
    }

    /// 在外面的按边数，各边记最近的那一行；看得见的、没画过的不算。
    #[test]
    fn rows_outside_the_viewport_count_on_their_side() {
        let notices = [
            notice("far-above", 1, 0),
            notice("above", 0, 1),
            notice("inside", 1, 0),
            notice("below", 0, 2),
            notice("far-below", 1, 0),
            notice("not-drawn", 1, 0),
        ];
        let hint = hint(&notices, |key| match key {
            "far-above" => Some(Placement::Above(80.)),
            "above" => Some(Placement::Above(5.)),
            "inside" => Some(Placement::Visible),
            "below" => Some(Placement::Below(30.)),
            "far-below" => Some(Placement::Below(120.)),
            _ => None,
        });
        let opens = |side: &Side| side.nearest.as_ref().map(|(_, key)| key.id.clone());
        assert_eq!(hint.above.tally, Tally { need: 1, unread: 1 });
        assert_eq!(opens(&hint.above).as_deref(), Some("above"));
        assert_eq!(hint.below.tally, Tally { need: 1, unread: 2 });
        assert_eq!(opens(&hint.below).as_deref(), Some("below"));
    }

    #[test]
    fn everything_in_view_means_no_hint() {
        let notices = [notice("a", 1, 0)];
        assert_eq!(
            hint(&notices, |_| Some(Placement::Visible)),
            Hint::default()
        );
    }

    #[test]
    fn labels_name_only_what_is_there() {
        assert_eq!(Tally { need: 2, unread: 0 }.label(), "2 need you");
        assert_eq!(Tally { need: 0, unread: 1 }.label(), "1 unread");
        assert_eq!(
            Tally { need: 1, unread: 3 }.label(),
            "1 need you · 3 unread"
        );
    }
}
