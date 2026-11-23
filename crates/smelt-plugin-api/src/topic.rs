//! 运行时话题的订阅：插件登记自己要哪些话题，宿主按所有插件要的并集向每台机器订阅，推来的
//! 原样转成 [`crate::HostEvent::RuntimeTopic`] 广播给所有插件，按 `topic` 认自己要的。
//!
//! 宿主不认识任何话题：订什么全由插件登记决定，没人要的话题不订。核心订阅里的会话状态、
//! 工作区目录不走这里（见 [`crate::Host::session_states`]）。
//!
//! 每个话题向每台机器单独订一条：运行时不认识某个话题（多半是它比这个话题旧）只拒掉那一条，
//! 收到一条 [`crate::TopicMessage::Unavailable`]，别的话题不受影响。订上时带快照的话题先推
//! 一份快照；不带快照的，插件登记后自己读一次初始状态。
//!
//! 别的插件已经订着的话题，新来的插件登记它时宿主重订那一条：先推一条
//! [`crate::TopicMessage::Lagged`]，再是重订上的那份快照（带快照的话题），新来的也拿得到
//! 初始状态；原来订着的按 `Lagged` 处理（重读或等快照），不会漏事件也不会收到重复的。

use std::collections::{BTreeMap, BTreeSet};

use gpui::{App, Global};

use crate::HostEvent;

#[derive(Default)]
struct Topics {
    /// 各来源要的话题。
    owners: Vec<(String, BTreeSet<String>)>,
    /// 并集里每个话题最近一次有来源新要它的序号，见 [`subscribed_topics`]。
    requested: BTreeMap<String, u64>,
    next: u64,
}

impl Global for Topics {}

/// 登记来源 `owner` 要订的话题，整组换掉它原来的那些；空的等于撤下。话题名不能是空的。
/// 宿主要据此增减、重订时发 [`HostEvent::TopicsChanged`]。
pub fn register_topics(
    owner: &str,
    topics: impl IntoIterator<Item = impl Into<String>>,
    cx: &mut App,
) -> Result<(), String> {
    if set_topics(owner, topics.into_iter().map(Into::into).collect(), cx)? {
        crate::emit(HostEvent::TopicsChanged, cx);
    }
    Ok(())
}

/// 换掉来源 `owner` 的话题，返回宿主要不要跟着变：并集变了，或有话题被这个来源新要了。
fn set_topics(owner: &str, topics: BTreeSet<String>, cx: &mut App) -> Result<bool, String> {
    if topics.iter().any(|topic| topic.trim().is_empty()) {
        return Err("话题名不能是空的".to_string());
    }
    let state = cx.default_global::<Topics>();
    let old = state
        .owners
        .iter()
        .find(|(entry, _)| entry == owner)
        .map(|(_, topics)| topics.clone())
        .unwrap_or_default();
    state.owners.retain(|(entry, _)| entry != owner);
    let added: Vec<String> = topics.difference(&old).cloned().collect();
    if !topics.is_empty() {
        state.owners.push((owner.to_string(), topics));
    }
    let mut changed = !added.is_empty();
    for topic in added {
        state.next += 1;
        state.requested.insert(topic, state.next);
    }
    let union: BTreeSet<&String> = state
        .owners
        .iter()
        .flat_map(|(_, topics)| topics.iter())
        .collect();
    let before = state.requested.len();
    state.requested.retain(|topic, _| union.contains(topic));
    changed |= state.requested.len() != before;
    Ok(changed)
}

/// 撤下来源 `owner` 要订的话题。
pub fn unregister_topics(owner: &str, cx: &mut App) {
    let empty: [String; 0] = [];
    // 空集合不会不合法。
    let _ = register_topics(owner, empty, cx);
}

/// 来源 `owner` 有没有登记话题 `topic`。推来的话题只交给登记了它的 JS 插件（Rust 插件
/// 收全部，按 `topic` 自己认）。
#[cfg(test)]
pub(crate) fn wants(owner: &str, topic: &str, cx: &App) -> bool {
    cx.try_global::<Topics>().is_some_and(|topics| {
        topics
            .owners
            .iter()
            .any(|(entry, topics)| entry == owner && topics.contains(topic))
    })
}

/// 宿主订阅时读：所有来源要的话题的并集。值是这个话题最近一次有来源新要它的序号，只增不减：
/// 宿主记下订上时的序号，后来变大了就重订一次（见模块文档）。
pub fn subscribed_topics(cx: &App) -> BTreeMap<String, u64> {
    cx.try_global::<Topics>()
        .map(|topics| topics.requested.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(owner: &str, topics: &[&str], cx: &mut App) -> bool {
        set_topics(
            owner,
            topics.iter().map(|topic| topic.to_string()).collect(),
            cx,
        )
        .unwrap()
    }

    fn topics(cx: &App) -> Vec<String> {
        subscribed_topics(cx).into_keys().collect()
    }

    #[gpui::test]
    fn union_follows_registrations(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert!(set("a", &["x.changed", "y.changed"], cx));
            assert!(set("b", &["y.changed"], cx));
            assert_eq!(topics(cx), ["x.changed", "y.changed"]);

            // 整组替换。
            assert!(set("a", &["z.changed"], cx));
            assert_eq!(topics(cx), ["y.changed", "z.changed"]);

            assert!(set("b", &[], cx));
            assert_eq!(topics(cx), ["z.changed"]);
            assert!(set("a", &[], cx));
            assert!(topics(cx).is_empty());
            // 没登记过的撤下什么都不变。
            assert!(!set("c", &[], cx));

            assert!(set_topics("a", [" ".to_string()].into(), cx).is_err());
            assert!(topics(cx).is_empty());

            set("a", &["x.changed"], cx);
            assert!(wants("a", "x.changed", cx));
            assert!(!wants("b", "x.changed", cx));
            assert!(!wants("a", "y.changed", cx));
        });
    }

    /// 别人已经订着的话题，新来的要了也要让宿主重订一次，它才拿得到快照；同一来源原样再登记
    /// 不用。
    #[gpui::test]
    fn a_newcomer_bumps_the_topic(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert!(set("a", &["x.changed"], cx));
            let first = subscribed_topics(cx)["x.changed"];
            assert!(!set("a", &["x.changed"], cx));
            assert_eq!(subscribed_topics(cx)["x.changed"], first);

            assert!(set("b", &["x.changed"], cx));
            assert!(subscribed_topics(cx)["x.changed"] > first);

            // 还有人要就不撤，序号不回退。
            let second = subscribed_topics(cx)["x.changed"];
            assert!(!set("b", &[], cx));
            assert_eq!(subscribed_topics(cx)["x.changed"], second);
        });
    }
}
