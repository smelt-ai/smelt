//! 插件之间的事件：一个插件广播，关心的插件订。和 [`crate::Command`] 一样带类型：id 是
//! `<来源>/<名字>`，载荷 `P` 的形状两头由编译器查；JS 插件按同样形状的 JSON 收发。
//!
//! 发出去是 [`HostEvent::Plugin`]，订阅照常用 `cx.subscribe(&events(cx), …)`，拿
//! [`Event::parse`] 认出自己关心的那条。事件只报此刻的变化，不留存：后订的收不到以前的，
//! 要现状就问发的插件（命令）。
//!
//! 只发自己来源下的事件。JS 插件发的由宿主查前缀；Rust 插件编进 App，分不出是谁在调，
//! 靠约定。

use std::marker::PhantomData;

use gpui::App;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::HostEvent;

/// 一种带类型的插件事件：id，以及载荷 `P` 的形状。
pub struct Event<P> {
    id: &'static str,
    types: PhantomData<fn(P)>,
}

impl<P> Clone for Event<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P> Copy for Event<P> {}

impl<P> Event<P> {
    /// `id` 是 `<来源>/<名字>`，来源是发它的插件。
    pub const fn new(id: &'static str) -> Self {
        Self {
            id,
            types: PhantomData,
        }
    }

    pub const fn id(&self) -> &'static str {
        self.id
    }
}

impl<P: Serialize> Event<P> {
    /// 广播一条，订阅 [`crate::events`] 的都收得到。载荷转不成 JSON 是写错了类型，记进日志、
    /// 不发。
    pub fn emit(&self, payload: &P, cx: &mut App) {
        match serde_json::to_value(payload) {
            Ok(payload) => crate::emit(
                HostEvent::Plugin {
                    name: self.id.into(),
                    payload,
                },
                cx,
            ),
            Err(error) => crate::app_log::error(
                "plugin-event",
                &format!("事件「{}」的载荷转不成 JSON：{error}", self.id),
            ),
        }
    }
}

impl<P: DeserializeOwned> Event<P> {
    /// 这条宿主事件是不是它；是就把载荷转回 `P`。名字对上、载荷认不出（发的一方改了形状）
    /// 也是 `None`。
    pub fn parse(&self, event: &HostEvent) -> Option<P> {
        match event {
            HostEvent::Plugin { name, payload } if name.as_str() == self.id => {
                serde_json::from_value(payload.clone()).ok()
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Moved {
        at: u32,
    }

    const MOVED: Event<Moved> = Event::new("probe/moved");
    const OTHER: Event<Moved> = Event::new("probe/other");

    #[test]
    fn parse_only_takes_its_own_name_and_shape() {
        let event = HostEvent::Plugin {
            name: MOVED.id().into(),
            payload: serde_json::json!({ "at": 3 }),
        };
        assert_eq!(MOVED.parse(&event), Some(Moved { at: 3 }));
        assert_eq!(OTHER.parse(&event), None);
        let malformed = HostEvent::Plugin {
            name: MOVED.id().into(),
            payload: serde_json::json!({ "at": "x" }),
        };
        assert_eq!(MOVED.parse(&malformed), None);
        assert_eq!(MOVED.parse(&HostEvent::WindowActivated), None);
    }
}
