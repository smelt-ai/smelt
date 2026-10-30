//! 推送登记：把这台手机的推送设置同步到每台机器（`push.register` / `push.unregister`）。
//!
//! 运行时把登记存进库，手机断开后照样按它推送；所以只在连上一台机器、或推送设置变了的时候
//! 同步一次，不定时重发。Dart 每次启动都会设一次（推送句柄可能换了），之前不知道设置时什么
//! 都不发。旧运行时没有这两个方法：记成不支持，机器详情里提示升级。
//!
//! 这里只是状态，挂在机队的视图上，在视图锁里改；真正的网络调用在机队起的线程上做。

use std::collections::HashMap;
use std::time::Duration;

use smelt_model::remote_config::PushRegisterParams;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    pub call_timeout: Duration,
}

impl Timing {
    pub(crate) const PRODUCTION: Self = Self {
        call_timeout: Duration::from_secs(10),
    };
}

/// 手机想要的推送设置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Desired {
    /// Dart 还没设：这次启动还不知道开没开，不动运行时上的登记。
    #[default]
    Unknown,
    Off,
    On(PushRegisterParams),
}

/// 一次同步要做的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyncJob {
    pub runtime_id: String,
    /// 节点序号：节点被替换后，旧节点的同步结果作废。
    pub serial: u64,
    pub version: u64,
    pub desired: Desired,
}

/// 一次同步的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyncResult {
    Done,
    /// 运行时没有 `push.*`：太旧。
    Unsupported,
    /// 网络等失败：等下次连上再同步。
    Failed,
}

#[derive(Default)]
struct NodeState {
    serial: u64,
    connected: bool,
    /// 已经同步到运行时的设置版本。
    synced: Option<u64>,
    in_flight: bool,
    /// 运行时支不支持推送；还没问过是 `None`。
    supported: Option<bool>,
}

#[derive(Default)]
pub(crate) struct PushState {
    desired: Desired,
    version: u64,
    nodes: HashMap<String, NodeState>,
}

impl PushState {
    /// 改设置。返回这时就该同步的机器。
    pub(crate) fn set(&mut self, desired: Desired) -> Vec<SyncJob> {
        if self.desired == desired {
            return Vec::new();
        }
        self.desired = desired;
        self.version += 1;
        let ids: Vec<String> = self.nodes.keys().cloned().collect();
        ids.into_iter()
            .filter_map(|runtime_id| self.claim(&runtime_id))
            .collect()
    }

    /// 一台机器的连接状态变了。新连上时（含重连）重新同步一次：运行时可能重装过、登记被清过。
    pub(crate) fn on_connection(
        &mut self,
        runtime_id: &str,
        serial: u64,
        connected: bool,
    ) -> Option<SyncJob> {
        let node = self.nodes.entry(runtime_id.to_string()).or_default();
        if node.serial != serial {
            *node = NodeState {
                serial,
                ..NodeState::default()
            };
        }
        let newly = connected && !node.connected;
        node.connected = connected;
        if newly {
            node.synced = None;
        }
        self.claim(runtime_id)
    }

    /// 同步做完。设置在此期间又变了就返回下一次要做的。
    pub(crate) fn finish(&mut self, job: &SyncJob, result: SyncResult) -> Option<SyncJob> {
        let node = self.nodes.get_mut(&job.runtime_id)?;
        if node.serial != job.serial {
            return None;
        }
        node.in_flight = false;
        match result {
            SyncResult::Done => {
                node.synced = Some(job.version);
                node.supported = Some(true);
            }
            SyncResult::Unsupported => {
                // 设置再变也没用，等重连（可能升级过）再问，见 `claim`。
                node.synced = Some(job.version);
                node.supported = Some(false);
            }
            SyncResult::Failed => {
                // 不立刻重试：断开重连时会再同步。失败的这一版记成已试过，免得同一版反复打。
                node.synced = Some(job.version);
            }
        }
        self.claim(&job.runtime_id)
    }

    pub(crate) fn remove(&mut self, runtime_id: &str) {
        self.nodes.remove(runtime_id);
    }

    pub(crate) fn desired(&self) -> &Desired {
        &self.desired
    }

    pub(crate) fn supported(&self, runtime_id: &str) -> Option<bool> {
        self.nodes.get(runtime_id).and_then(|node| node.supported)
    }

    fn claim(&mut self, runtime_id: &str) -> Option<SyncJob> {
        if self.desired == Desired::Unknown {
            return None;
        }
        let node = self.nodes.get_mut(runtime_id)?;
        if !node.connected || node.in_flight || node.synced == Some(self.version) {
            return None;
        }
        // 问过不支持的运行时，只在重新连上（`synced` 清空）时再问一次。
        if node.supported == Some(false) && node.synced.is_some() {
            return None;
        }
        node.in_flight = true;
        Some(SyncJob {
            runtime_id: runtime_id.to_string(),
            serial: node.serial,
            version: self.version,
            desired: self.desired.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Desired {
        Desired::On(PushRegisterParams {
            handle: "h".into(),
            relay_url: "https://push.example.test".into(),
            approval: true,
            input: true,
            failure: true,
        })
    }

    #[test]
    fn nothing_is_sent_before_dart_sets_the_preference() {
        let mut state = PushState::default();
        assert_eq!(state.on_connection("a", 1, true), None);
        let jobs = state.set(on());
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].runtime_id, "a");
        assert_eq!(jobs[0].desired, on());
    }

    #[test]
    fn connecting_syncs_once_and_reconnecting_syncs_again() {
        let mut state = PushState::default();
        state.set(on());
        let job = state.on_connection("a", 1, true).unwrap();
        assert_eq!(state.on_connection("a", 1, true), None, "同步中不重复");
        assert_eq!(state.finish(&job, SyncResult::Done), None);
        assert_eq!(state.on_connection("a", 1, true), None, "仍连着，不再同步");
        assert_eq!(state.supported("a"), Some(true));

        assert_eq!(state.on_connection("a", 1, false), None);
        assert!(state.on_connection("a", 1, true).is_some(), "重连后再同步");
    }

    #[test]
    fn a_change_during_a_sync_runs_after_it() {
        let mut state = PushState::default();
        state.set(on());
        let first = state.on_connection("a", 1, true).unwrap();
        assert!(state.set(Desired::Off).is_empty(), "等进行中的那次做完");
        let next = state.finish(&first, SyncResult::Done).unwrap();
        assert_eq!(next.desired, Desired::Off);
        assert_eq!(state.finish(&next, SyncResult::Done), None);
    }

    #[test]
    fn old_runtimes_are_marked_unsupported_until_they_reconnect() {
        let mut state = PushState::default();
        state.set(on());
        let job = state.on_connection("a", 1, true).unwrap();
        assert_eq!(state.finish(&job, SyncResult::Unsupported), None);
        assert_eq!(state.supported("a"), Some(false));
        assert!(state.set(Desired::Off).is_empty(), "不支持的不再打");
        state.on_connection("a", 1, false);
        assert!(state.on_connection("a", 1, true).is_some());
    }

    #[test]
    fn failures_wait_for_the_next_connection_and_replaced_nodes_drop_results() {
        let mut state = PushState::default();
        state.set(on());
        let job = state.on_connection("a", 1, true).unwrap();
        assert_eq!(state.finish(&job, SyncResult::Failed), None);
        assert_eq!(state.supported("a"), None);

        let job = state.set(Desired::Off).pop().unwrap();
        assert!(state.on_connection("a", 2, false).is_none(), "节点换了");
        assert_eq!(state.finish(&job, SyncResult::Done), None);
        assert_eq!(state.supported("a"), None);
    }
}
