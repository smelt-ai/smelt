//! 不进核心订阅的话题单独订一条（为什么分开见 `smelt_event_protocol` 的模块文档）。这些
//! 话题只有事件、没有快照；运行时不认识其中哪个就拒掉这一条，调用方据此知道那项功能在这台
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
    /// 运行时那边积压，丢了事件：订的话题都当作变过。
    Lagged,
}

pub struct TopicEventClient {
    handshake: EventSubscribeRequest,
}

impl TopicEventClient {
    /// `topics` 是协议里登记过的话题名。
    pub fn new(topics: Vec<&str>) -> Self {
        Self {
            handshake: desktop_handshake(topics),
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
            SubscribeMessage::Lag { .. } => Ok(Some(TopicMessage::Lagged)),
            SubscribeMessage::Error { code, message } => {
                Err(DaemonEventClientError::Server { code, message })
            }
            // 这些话题没有快照；比这个客户端新的运行时发的消息类型也跳过。
            SubscribeMessage::Snapshot { .. } | SubscribeMessage::Unknown => Ok(None),
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
