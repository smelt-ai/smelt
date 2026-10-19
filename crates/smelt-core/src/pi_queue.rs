//! Pi 输入队列在驱动侧的镜像。
//!
//! Pi 的 `queue_update` 和 `clear_queue` 只报文字。停止时排队的输入要原样还回输入框，
//! 图片只能按文字在这里认回来。
//!
//! 只有图、没有字的输入另有一个上游缺陷要补：Pi 只按非空文字出队
//! （earendil-works/pi#8612），这种输入交给模型以后，它的空串仍挂在 Pi 的队列里，
//! 也不会再报 `queue_update`。这里数着已经交付的空串，对外报的队列和还原都扣掉它们。
//! 拆除条件：升级 Pi 时到已安装的 `agent-session.js` 里看 `message_start` 出队那段，
//! 不再以 `if (messageText)` 为前提时，删掉 `delivered_empty` 和对应测试。

use crate::acp_conn::PromptImage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueLane {
    Steering,
    FollowUp,
}

#[derive(Default)]
pub(crate) struct PiQueueMirror {
    steering: Lane,
    follow_up: Lane,
}

#[derive(Default)]
struct Lane {
    /// Pi 最近一次报的原文，含已交付却卡住的空串。
    reported: Vec<String>,
    inputs: Vec<QueuedInput>,
    /// 已经交给模型、却因 #8612 还留在 Pi 队列里的空串条数。
    delivered_empty: usize,
}

struct QueuedInput {
    text: String,
    images: Vec<PromptImage>,
    /// 还没等到 Pi 的回执。Pi 先报 `queue_update` 再回执，回执之前的队列快照里
    /// 可能还没有它，不能当成已经被吃掉。
    request_id: Option<String>,
}

impl Lane {
    /// 对外的队列：扣掉最前面那几个已经交付的空串。
    fn visible(&self) -> Vec<String> {
        let mut skip = self.delivered_empty;
        self.reported
            .iter()
            .filter(|text| {
                if skip > 0 && text.is_empty() {
                    skip -= 1;
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect()
    }

    /// 按对外队列重排镜像。Pi 的队列先进先出，被吃掉的总在前头，所以同文字的
    /// 输入从后往前认，留下的是后发的那几条。
    fn reconcile(&mut self) {
        let empties = self.reported.iter().filter(|text| text.is_empty()).count();
        self.delivered_empty = self.delivered_empty.min(empties);
        let mut old = std::mem::take(&mut self.inputs);
        let mut next: Vec<QueuedInput> = self
            .visible()
            .into_iter()
            .rev()
            .map(
                |text| match old.iter().rposition(|input| input.text == text) {
                    Some(ix) => old.remove(ix),
                    None => QueuedInput {
                        text,
                        images: Vec::new(),
                        request_id: None,
                    },
                },
            )
            .collect();
        next.reverse();
        next.extend(old.into_iter().filter(|input| input.request_id.is_some()));
        self.inputs = next;
    }

    /// 还回输入框的 (文字, 附图)，然后清空。
    fn take_restore(&mut self, reported: Vec<String>) -> Vec<(String, Vec<PromptImage>)> {
        self.reported = reported;
        let visible = self.visible();
        let mut inputs = std::mem::take(&mut self.inputs);
        *self = Lane::default();
        visible
            .into_iter()
            .map(|text| {
                let images = inputs
                    .iter()
                    .position(|input| input.text == text)
                    .map(|ix| inputs.remove(ix).images)
                    .unwrap_or_default();
                (text, images)
            })
            .collect()
    }
}

impl PiQueueMirror {
    fn lane(&mut self, lane: QueueLane) -> &mut Lane {
        match lane {
            QueueLane::Steering => &mut self.steering,
            QueueLane::FollowUp => &mut self.follow_up,
        }
    }

    fn visible(&self) -> (Vec<String>, Vec<String>) {
        (self.steering.visible(), self.follow_up.visible())
    }

    /// 刚写出一条 steer / follow_up。
    pub(crate) fn sent(
        &mut self,
        lane: QueueLane,
        request_id: String,
        text: String,
        images: Vec<PromptImage>,
    ) {
        self.lane(lane).inputs.push(QueuedInput {
            text,
            images,
            request_id: Some(request_id),
        });
    }

    /// Pi 的回执。没进队列（失败、被输入处理器接走）的那条不再等。
    pub(crate) fn acknowledged(&mut self, request_id: &str, queued: bool) {
        for lane in [&mut self.steering, &mut self.follow_up] {
            if let Some(ix) = lane
                .inputs
                .iter()
                .position(|input| input.request_id.as_deref() == Some(request_id))
            {
                if queued {
                    lane.inputs[ix].request_id = None;
                } else {
                    lane.inputs.remove(ix);
                }
                return;
            }
        }
    }

    /// Pi 报了新队列。返回对外的两条队列。
    pub(crate) fn reported(
        &mut self,
        steering: Vec<String>,
        follow_up: Vec<String>,
    ) -> (Vec<String>, Vec<String>) {
        self.steering.reported = steering;
        self.follow_up.reported = follow_up;
        self.steering.reconcile();
        self.follow_up.reconcile();
        self.visible()
    }

    /// Pi 把一条用户消息交给了模型。是只有图、因 #8612 留在队列里的那条时，
    /// 返回扣掉它之后的队列，调用方照 `queue_update` 转发。
    pub(crate) fn user_message_started(
        &mut self,
        text: &str,
        has_image: bool,
    ) -> Option<(Vec<String>, Vec<String>)> {
        if !text.is_empty() || !has_image {
            return None;
        }
        let lane = [QueueLane::Steering, QueueLane::FollowUp]
            .into_iter()
            .find(|lane| {
                let lane = self.lane(*lane);
                lane.reported.iter().filter(|text| text.is_empty()).count() > lane.delivered_empty
            })?;
        let lane = self.lane(lane);
        lane.delivered_empty += 1;
        if let Some(ix) = lane.inputs.iter().position(|input| input.text.is_empty()) {
            lane.inputs.remove(ix);
        }
        lane.reconcile();
        Some(self.visible())
    }

    /// `clear_queue` 回执里 Pi 清掉的原文。返回要还回输入框的 (文字, 附图)，
    /// 先 steering 后 follow-up。镜像随之清空。
    pub(crate) fn cleared(
        &mut self,
        steering: Vec<String>,
        follow_up: Vec<String>,
    ) -> Vec<(String, Vec<PromptImage>)> {
        let mut restore = self.steering.take_restore(steering);
        restore.extend(self.follow_up.take_restore(follow_up));
        restore
    }

    /// Pi 换了会话（回退）。旧队列一并作废。
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(data: &str) -> PromptImage {
        PromptImage::new("image/png", data)
    }

    #[test]
    fn clear_returns_each_queued_input_with_its_images() {
        let mut mirror = PiQueueMirror::default();
        mirror.sent(
            QueueLane::Steering,
            "s1".into(),
            "看图".into(),
            vec![image("a")],
        );
        mirror.sent(
            QueueLane::FollowUp,
            "f1".into(),
            "然后".into(),
            vec![image("b")],
        );
        mirror.reported(vec!["看图".into()], Vec::new());
        mirror.acknowledged("s1", true);
        mirror.reported(vec!["看图".into()], vec!["然后".into()]);
        mirror.acknowledged("f1", true);

        let restore = mirror.cleared(vec!["看图".into()], vec!["然后".into()]);

        assert_eq!(
            restore,
            vec![
                ("看图".to_string(), vec![image("a")]),
                ("然后".to_string(), vec![image("b")]),
            ]
        );
        assert!(mirror.cleared(Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn a_snapshot_taken_before_pi_reads_the_steer_does_not_drop_it() {
        let mut mirror = PiQueueMirror::default();
        mirror.sent(
            QueueLane::Steering,
            "s1".into(),
            "第一条".into(),
            Vec::new(),
        );
        mirror.reported(vec!["第一条".into()], Vec::new());
        mirror.acknowledged("s1", true);
        mirror.sent(
            QueueLane::Steering,
            "s2".into(),
            "第二条".into(),
            vec![image("b")],
        );
        // Pi 吃掉第一条时还没读到第二条。
        mirror.reported(Vec::new(), Vec::new());
        mirror.reported(vec!["第二条".into()], Vec::new());
        mirror.acknowledged("s2", true);

        let restore = mirror.cleared(vec!["第二条".into()], Vec::new());

        assert_eq!(restore, vec![("第二条".to_string(), vec![image("b")])]);
    }

    #[test]
    fn same_text_keeps_the_images_of_the_later_input() {
        let mut mirror = PiQueueMirror::default();
        mirror.sent(
            QueueLane::Steering,
            "s1".into(),
            "再看".into(),
            vec![image("a")],
        );
        mirror.sent(
            QueueLane::Steering,
            "s2".into(),
            "再看".into(),
            vec![image("b")],
        );
        mirror.acknowledged("s1", true);
        mirror.acknowledged("s2", true);
        mirror.reported(vec!["再看".into(), "再看".into()], Vec::new());
        mirror.reported(vec!["再看".into()], Vec::new());

        let restore = mirror.cleared(vec!["再看".into()], Vec::new());

        assert_eq!(restore, vec![("再看".to_string(), vec![image("b")])]);
    }

    #[test]
    fn an_input_pi_did_not_queue_is_not_restored() {
        let mut mirror = PiQueueMirror::default();
        mirror.sent(
            QueueLane::Steering,
            "s1".into(),
            "/handled".into(),
            Vec::new(),
        );
        mirror.acknowledged("s1", false);

        assert!(mirror.cleared(Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn a_delivered_image_only_input_leaves_the_reported_queue() {
        let mut mirror = PiQueueMirror::default();
        mirror.sent(
            QueueLane::Steering,
            "s1".into(),
            String::new(),
            vec![image("a")],
        );
        mirror.sent(
            QueueLane::Steering,
            "s2".into(),
            "补一句".into(),
            Vec::new(),
        );
        assert_eq!(
            mirror.reported(vec![String::new(), "补一句".into()], Vec::new()),
            (vec![String::new(), "补一句".to_string()], Vec::new())
        );
        mirror.acknowledged("s1", true);
        mirror.acknowledged("s2", true);

        assert_eq!(mirror.user_message_started("补一句", false), None);
        assert_eq!(
            mirror.user_message_started("", true),
            Some((vec!["补一句".to_string()], Vec::new())),
            "交给模型的纯图片插话不能继续挂在队列里"
        );
        // Pi 吃掉「补一句」后报的队列仍带着那个空串。
        assert_eq!(
            mirror.reported(vec![String::new()], Vec::new()),
            (Vec::new(), Vec::new())
        );
        assert_eq!(
            mirror.user_message_started("", true),
            None,
            "没有排着的空串时，纯图片消息不是从队列来的"
        );
        assert!(mirror.cleared(vec![String::new()], Vec::new()).is_empty());
    }

    #[test]
    fn a_queued_image_only_input_comes_back_with_its_images() {
        let mut mirror = PiQueueMirror::default();
        mirror.sent(
            QueueLane::FollowUp,
            "f1".into(),
            String::new(),
            vec![image("a")],
        );
        mirror.sent(
            QueueLane::FollowUp,
            "f2".into(),
            String::new(),
            vec![image("b")],
        );
        mirror.reported(Vec::new(), vec![String::new(), String::new()]);
        mirror.acknowledged("f1", true);
        mirror.acknowledged("f2", true);
        mirror.user_message_started("", true);

        let restore = mirror.cleared(Vec::new(), vec![String::new(), String::new()]);

        assert_eq!(restore, vec![(String::new(), vec![image("b")])]);
    }
}
