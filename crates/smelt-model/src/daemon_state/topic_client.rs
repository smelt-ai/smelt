//! 不进核心订阅的话题单独订一条（为什么分开见 `smelt_event_protocol` 的模块文档）。带快照的
//! 话题（如 `attention.changed`）订上时和积压丢了事件之后各收一份快照，按协议登记的快照种类
//! 认出是哪个话题的；运行时不认识其中哪个话题就拒掉这一条，调用方据此知道那项功能在这台
//! 机器上不可用，核心订阅不受影响。和 [`super::DaemonStateEventClient`] 一样只吃字符串、
//! 不碰 IO。

use smelt_event_protocol::{EventSource, EventSubscribeRequest, SubscribeMessage};

use super::DaemonEventClientError;
use super::event_client::{desktop_handshake, request_line};

/// 单独订阅收到的一条。
#[derive(Clone, Debug, PartialEq)]
pub enum TopicMessage {
    Event {
        topic: String,
        payload: serde_json::Value,
    },
    /// 带快照的话题的整份状态：订上时一份，积压丢了事件后再一份。
    Snapshot {
        topic: String,
        payload: serde_json::Value,
    },
    /// 运行时那边积压，丢了事件：订的话题都当作变过。带快照的话题后面跟着一份快照。
    Lagged,
}

pub struct TopicEventClient {
    handshake: EventSubscribeRequest,
    /// 订的话题里带快照的：快照种类 → 话题。
    snapshots: Vec<(String, String)>,
}

impl TopicEventClient {
    /// `topics` 是协议里登记过的话题名。
    pub fn new(topics: Vec<&str>) -> Self {
        let snapshots = smelt_event_protocol::core_topic_descriptors()
            .expect("core topic descriptors are valid")
            .into_iter()
            .filter(|descriptor| topics.contains(&descriptor.topic.as_str()))
            .filter_map(|descriptor| {
                let kind = descriptor.snapshot_kind?;
                Some((kind.0, descriptor.topic.as_str().to_string()))
            })
            .collect();
        Self {
            handshake: desktop_handshake(topics),
            snapshots,
        }
    }

    pub fn handshake(&self) -> &EventSubscribeRequest {
        &self.handshake
    }

    pub fn request_line(&self) -> Result<String, DaemonEventClientError> {
        request_line(&self.handshake)
    }

    /// 运行时拒掉订阅（比如不认识话题）是 [`DaemonEventClientError::Server`]。
    pub fn decode_line(&self, line: &str) -> Result<Option<TopicMessage>, DaemonEventClientError> {
        let message = serde_json::from_str::<SubscribeMessage<serde_json::Value>>(line)
            .map_err(|error| DaemonEventClientError::Protocol(error.to_string()))?;
        match message {
            SubscribeMessage::Event { envelope, .. } => {
                if envelope.source != EventSource::Core {
                    return Err(DaemonEventClientError::Protocol(
                        "subscribed event did not come from core".to_string(),
                    ));
                }
                Ok(Some(TopicMessage::Event {
                    topic: envelope.topic.as_str().to_string(),
                    payload: envelope.payload,
                }))
            }
            SubscribeMessage::Snapshot { kind, payload, .. } => {
                let topic = self
                    .snapshots
                    .iter()
                    .find(|(snapshot, _)| *snapshot == kind.0)
                    .map(|(_, topic)| topic.clone())
                    .ok_or_else(|| {
                        DaemonEventClientError::Protocol(format!(
                            "snapshot {} does not belong to a subscribed topic",
                            kind.0
                        ))
                    })?;
                Ok(Some(TopicMessage::Snapshot { topic, payload }))
            }
            SubscribeMessage::Lag { .. } => Ok(Some(TopicMessage::Lagged)),
            SubscribeMessage::Error { code, message } => {
                Err(DaemonEventClientError::Server { code, message })
            }
            // 比这个客户端新的运行时发的消息类型跳过。
            SubscribeMessage::Unknown => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_event_protocol::{
        CORE_TOPIC_PI_CONFIG_CHANGED, CorrelationId, EventEnvelope, EventId, PiConfigChanged,
        SubscribeErrorCode, Topic,
    };

    fn client() -> TopicEventClient {
        TopicEventClient::new(vec![CORE_TOPIC_PI_CONFIG_CHANGED])
    }

    fn line(message: SubscribeMessage<serde_json::Value>) -> String {
        serde_json::to_string(&message).unwrap()
    }

    fn pi_config_changed() -> SubscribeMessage<serde_json::Value> {
        SubscribeMessage::Event {
            sequence: Some(1),
            envelope: EventEnvelope {
                event_id: EventId::new("event-1").unwrap(),
                topic: Topic::new(CORE_TOPIC_PI_CONFIG_CHANGED).unwrap(),
                schema_version: 1,
                occurred_at_ms: 1,
                aggregate: None,
                aggregate_revision: None,
                source: EventSource::Core,
                correlation_id: CorrelationId::new("correlation-1").unwrap(),
                causation_id: None,
                hop_count: 0,
                payload: serde_json::to_value(PiConfigChanged {}).unwrap(),
            },
        }
    }

    #[test]
    fn handshake_subscribes_only_the_given_topics() {
        let client = client();
        let topics = &client.handshake().request.subscription.topics;
        assert_eq!(
            topics.iter().map(Topic::as_str).collect::<Vec<_>>(),
            vec![CORE_TOPIC_PI_CONFIG_CHANGED]
        );
        let request = client.request_line().unwrap();
        assert!(request.ends_with('\n'));
        let value: serde_json::Value = serde_json::from_str(request.trim_end()).unwrap();
        assert_eq!(value["op"], "event_subscribe");
    }

    #[test]
    fn decodes_events_and_lag() {
        let client = client();
        assert_eq!(
            client.decode_line(&line(pi_config_changed())).unwrap(),
            Some(TopicMessage::Event {
                topic: CORE_TOPIC_PI_CONFIG_CHANGED.to_string(),
                payload: serde_json::json!({}),
            })
        );
        let lag = SubscribeMessage::Lag {
            dropped: 3,
            resume_after: Some(1),
        };
        assert_eq!(
            client.decode_line(&line(lag)).unwrap(),
            Some(TopicMessage::Lagged)
        );
        assert_eq!(
            client
                .decode_line(r#"{"type":"some_future_message","anything":1}"#)
                .unwrap(),
            None
        );
    }

    #[test]
    fn snapshots_are_attributed_to_their_topic() {
        use smelt_event_protocol::{
            AttentionSnapshot, CORE_SNAPSHOT_ATTENTION, CORE_TOPIC_ATTENTION_CHANGED, SnapshotKind,
        };
        let snapshot = |kind: &str| SubscribeMessage::Snapshot {
            kind: SnapshotKind(kind.to_string()),
            watermark: 0,
            revision: 3,
            payload: serde_json::to_value(AttentionSnapshot {
                revision: 3,
                sessions: Vec::new(),
            })
            .unwrap(),
        };
        let attention = TopicEventClient::new(vec![
            CORE_TOPIC_PI_CONFIG_CHANGED,
            CORE_TOPIC_ATTENTION_CHANGED,
        ]);
        assert_eq!(
            attention
                .decode_line(&line(snapshot(CORE_SNAPSHOT_ATTENTION)))
                .unwrap(),
            Some(TopicMessage::Snapshot {
                topic: CORE_TOPIC_ATTENTION_CHANGED.to_string(),
                payload: serde_json::json!({ "revision": 3, "sessions": [] }),
            })
        );
        // 没订那个话题，运行时却发来它的快照：协议出错。
        assert!(
            client()
                .decode_line(&line(snapshot(CORE_SNAPSHOT_ATTENTION)))
                .is_err()
        );
    }

    /// 旧运行时不认识话题：拒掉这一条，调用方据此停订。
    #[test]
    fn rejection_is_a_server_error() {
        let rejected = SubscribeMessage::Error {
            code: SubscribeErrorCode::Rejected,
            message: "unknown topic".to_string(),
        };
        assert!(matches!(
            client().decode_line(&line(rejected)),
            Err(DaemonEventClientError::Server {
                code: SubscribeErrorCode::Rejected,
                ..
            })
        ));
    }
}
