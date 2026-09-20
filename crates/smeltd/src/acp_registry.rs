use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

pub(crate) struct AcpSlot<T> {
    pub(crate) lifecycle: Mutex<()>,
    pub(crate) value: T,
}

struct RegistryState<T> {
    sessions: HashMap<String, Arc<AcpSlot<T>>>,
    /// provider session id -> Smelt ACP session id。
    resume_owners: HashMap<String, String>,
    /// 一个 provider 会话可能同时有 canonical history id 与当前 runtime id；
    /// 反向索引保留该 Smelt 会话的全部等价身份，释放时必须一起处理。
    session_resumes: HashMap<String, HashSet<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResumeAcquireError {
    SessionMissing,
    Owned { resume_id: String, owner: String },
}

impl std::fmt::Display for ResumeAcquireError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionMissing => formatter.write_str("Smelt ACP session 不存在"),
            Self::Owned { resume_id, owner } => {
                write!(
                    formatter,
                    "provider session {resume_id} 已被会话 {owner} 占用"
                )
            }
        }
    }
}

/// 新会话的 runtime 落在哪里。显式存进注册表，而不是在 spawn 点反问构建配置：
/// 那样测试里那条分支按构造永远不可达，等于没有覆盖，而且将来谁多建一个注册表
/// 也看不出它会不会去起进程。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcpSpawnPolicy {
    /// prod：每个会话一个独立的 smeltd 宿主进程。
    HostedProcess,
    /// 同进程直连 driver。只有单测注册表会选它，所以 prod 构建里没有构造点；
    /// 判定逻辑本身两种构建都编译，这正是这次改动要保住的东西。
    #[cfg_attr(not(test), allow(dead_code))]
    SameProcess,
}

pub(crate) struct AcpRegistry<T> {
    /// 会话表和 provider 所有权必须在同一个临界区内变更。拆成两把锁会让
    /// reserve/remove 与 acquire/release 之间出现可观察的半状态。
    state: Mutex<RegistryState<T>>,
    spawn_gate: Arc<RwLock<()>>,
    spawn_policy: AcpSpawnPolicy,
}

impl<T> AcpRegistry<T> {
    pub(crate) fn new(spawn_gate: Arc<RwLock<()>>) -> Self {
        Self::with_policy(spawn_gate, AcpSpawnPolicy::HostedProcess)
    }

    /// 同进程 driver 的注册表。只有单测该用它。
    #[cfg(test)]
    pub(crate) fn new_same_process(spawn_gate: Arc<RwLock<()>>) -> Self {
        Self::with_policy(spawn_gate, AcpSpawnPolicy::SameProcess)
    }

    fn with_policy(spawn_gate: Arc<RwLock<()>>, spawn_policy: AcpSpawnPolicy) -> Self {
        Self {
            state: Mutex::new(RegistryState {
                sessions: HashMap::new(),
                resume_owners: HashMap::new(),
                session_resumes: HashMap::new(),
            }),
            spawn_gate,
            spawn_policy,
        }
    }

    /// 原子地给 `acp_sid` 增加一组 provider 身份别名。任一 id 已被其他会话
    /// 持有时整组不变；canonical 与 runtime id 因而不会出现只登记一半的状态。
    pub(crate) fn try_acquire_resumes(
        &self,
        resume_ids: &[String],
        acp_sid: &str,
    ) -> Result<(), ResumeAcquireError> {
        let mut state = self.state.lock().unwrap();
        if !state.sessions.contains_key(acp_sid) {
            return Err(ResumeAcquireError::SessionMissing);
        }
        let resume_ids = resume_ids
            .iter()
            .filter(|resume_id| !resume_id.trim().is_empty())
            .cloned()
            .collect::<HashSet<_>>();
        for resume_id in &resume_ids {
            if let Some(owner) = state.resume_owners.get(resume_id)
                && owner != acp_sid
            {
                return Err(ResumeAcquireError::Owned {
                    resume_id: resume_id.clone(),
                    owner: owner.clone(),
                });
            }
        }
        for resume_id in &resume_ids {
            state
                .resume_owners
                .insert(resume_id.clone(), acp_sid.to_string());
        }
        state
            .session_resumes
            .entry(acp_sid.to_string())
            .or_default()
            .extend(resume_ids);
        Ok(())
    }

    /// 旧 provider 进程已经停止后，只保留本次 relaunch 使用的身份。调用方必须
    /// 先通过 `try_acquire_resumes` 占住新身份，避免“先释放、后启动”窗口。
    pub(crate) fn retain_resumes_for(&self, acp_sid: &str, keep: &[String]) {
        let mut state = self.state.lock().unwrap();
        let keep = keep
            .iter()
            .filter(|resume_id| !resume_id.trim().is_empty())
            .cloned()
            .collect::<HashSet<_>>();
        let Some(held) = state.session_resumes.get_mut(acp_sid) else {
            return;
        };
        let released = held
            .iter()
            .filter(|resume_id| !keep.contains(*resume_id))
            .cloned()
            .collect::<Vec<_>>();
        held.retain(|resume_id| keep.contains(resume_id));
        let remove_reverse_entry = held.is_empty();
        for resume_id in released {
            if state.resume_owners.get(&resume_id).map(String::as_str) == Some(acp_sid) {
                state.resume_owners.remove(&resume_id);
            }
        }
        if remove_reverse_entry {
            state.session_resumes.remove(acp_sid);
        }
    }

    /// 会话移除时释放它持有的全部 resume 占用。
    pub(crate) fn release_resume_for(&self, acp_sid: &str) {
        let mut state = self.state.lock().unwrap();
        if let Some(resume_ids) = state.session_resumes.remove(acp_sid) {
            for resume_id in resume_ids {
                if state.resume_owners.get(&resume_id).map(String::as_str) == Some(acp_sid) {
                    state.resume_owners.remove(&resume_id);
                }
            }
        }
    }

    pub(crate) fn reserve_with(
        &self,
        id: &str,
        create: impl FnOnce() -> T,
    ) -> (Arc<AcpSlot<T>>, bool) {
        use std::collections::hash_map::Entry;

        match self.state.lock().unwrap().sessions.entry(id.to_string()) {
            Entry::Occupied(entry) => (Arc::clone(entry.get()), false),
            Entry::Vacant(entry) => {
                let slot = Arc::new(AcpSlot {
                    lifecycle: Mutex::new(()),
                    value: create(),
                });
                entry.insert(Arc::clone(&slot));
                (slot, true)
            }
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<AcpSlot<T>>> {
        self.state.lock().unwrap().sessions.get(id).cloned()
    }

    pub(crate) fn is_current(&self, id: &str, expected: &Arc<AcpSlot<T>>) -> bool {
        self.state
            .lock()
            .unwrap()
            .sessions
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
    }

    /// Run one operation against an exact ACP slot captured by an earlier lookup. The callback
    /// runs while `expected.lifecycle` is held and must not reacquire that same lifecycle lock.
    pub(crate) fn with_current<R>(
        &self,
        id: &str,
        expected: &Arc<AcpSlot<T>>,
        operation: impl FnOnce(&T) -> R,
    ) -> Option<R> {
        let _lifecycle = expected.lifecycle.lock().unwrap();
        if !self.is_current(id, expected) {
            return None;
        }
        Some(operation(&expected.value))
    }

    pub(crate) fn snapshot(&self) -> Vec<(String, Arc<AcpSlot<T>>)> {
        self.state
            .lock()
            .unwrap()
            .sessions
            .iter()
            .map(|(id, slot)| (id.clone(), Arc::clone(slot)))
            .collect()
    }

    pub(crate) fn remove_if_same(
        &self,
        id: &str,
        expected: &Arc<AcpSlot<T>>,
    ) -> Option<Arc<AcpSlot<T>>> {
        let mut state = self.state.lock().unwrap();
        let current = state.sessions.get(id)?;
        if !Arc::ptr_eq(current, expected) {
            return None;
        }
        let removed = state.sessions.remove(id);
        if let Some(resume_ids) = state.session_resumes.remove(id) {
            for resume_id in resume_ids {
                if state.resume_owners.get(&resume_id).map(String::as_str) == Some(id) {
                    state.resume_owners.remove(&resume_id);
                }
            }
        }
        removed
    }

    pub(crate) fn spawn_gate(&self) -> Arc<RwLock<()>> {
        Arc::clone(&self.spawn_gate)
    }

    pub(crate) fn spawn_policy(&self) -> AcpSpawnPolicy {
        self.spawn_policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl<T> AcpRegistry<T> {
        /// 单个身份的 `try_acquire_resumes`。prod 一律走复数版（canonical + runtime
        /// id 必须原子地一起占），这个只是用例里的书写便利。
        fn try_acquire_resume(
            &self,
            resume_id: &str,
            acp_sid: &str,
        ) -> Result<(), ResumeAcquireError> {
            self.try_acquire_resumes(&[resume_id.to_string()], acp_sid)
        }

        /// 当前持有 `resume_id` 的 ACP 会话 sid（无主时 None）。读私有 `resume_owners`
        /// 是为了断言占用归属；prod 逻辑只需要 try/retain/release 三个动作。
        fn resume_owner(&self, resume_id: &str) -> Option<String> {
            self.state
                .lock()
                .unwrap()
                .resume_owners
                .get(resume_id)
                .cloned()
        }
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, RwLock};
    use std::time::Duration;

    #[test]
    fn concurrent_reserve_returns_one_entry() {
        let registry = Arc::new(AcpRegistry::<usize>::new(Arc::new(RwLock::new(()))));
        let created = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();

        for _ in 0..8 {
            let registry = Arc::clone(&registry);
            let created = Arc::clone(&created);
            threads.push(std::thread::spawn(move || {
                registry.reserve_with("same", || {
                    created.fetch_add(1, Ordering::SeqCst);
                    7
                })
            }));
        }

        let entries: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(created.load(Ordering::SeqCst), 1);
        assert!(
            entries
                .windows(2)
                .all(|pair| Arc::ptr_eq(&pair[0].0, &pair[1].0))
        );
    }

    #[test]
    fn remove_if_same_does_not_delete_a_replacement() {
        let registry = AcpRegistry::new(Arc::new(RwLock::new(())));
        let (old, _) = registry.reserve_with("id", || 1usize);
        assert!(registry.remove_if_same("id", &old).is_some());
        let (replacement, _) = registry.reserve_with("id", || 2usize);

        assert!(registry.remove_if_same("id", &old).is_none());
        assert!(Arc::ptr_eq(&registry.get("id").unwrap(), &replacement));
        assert!(registry.with_current("id", &old, |_| ()).is_none());
    }

    #[test]
    fn current_operation_waits_for_the_slot_lifecycle() {
        let registry = Arc::new(AcpRegistry::new(Arc::new(RwLock::new(()))));
        let (slot, _) = registry.reserve_with("id", || 1usize);
        let lifecycle = slot.lifecycle.lock().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (ran_tx, ran_rx) = mpsc::channel();
        let worker_registry = Arc::clone(&registry);
        let worker_slot = Arc::clone(&slot);
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            worker_registry.with_current("id", &worker_slot, |_| ran_tx.send(()).unwrap())
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            ran_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "ACP 副作用必须等待 lifecycle，不能在退休实例上继续执行"
        );
        drop(lifecycle);
        assert!(worker.join().unwrap().is_some());
        ran_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn resume_owner_is_exclusive_and_released_on_session_removal() {
        let registry = AcpRegistry::<usize>::new(Arc::new(RwLock::new(())));
        registry.reserve_with("acp-a", || 1);
        registry.reserve_with("acp-b", || 2);
        // 无主：第一个会话登记成功。
        assert!(
            registry
                .try_acquire_resume("provider-sess-1", "acp-a")
                .is_ok()
        );
        assert_eq!(
            registry.resume_owner("provider-sess-1").as_deref(),
            Some("acp-a")
        );
        // 其他会话抢占失败。
        assert!(
            registry
                .try_acquire_resume("provider-sess-1", "acp-b")
                .is_err()
        );
        // 自身续持（restart 自己的历史）成功。
        assert!(
            registry
                .try_acquire_resume("provider-sess-1", "acp-a")
                .is_ok()
        );
    }

    #[test]
    fn release_resume_for_frees_only_that_sessions_holdings() {
        let registry = AcpRegistry::<usize>::new(Arc::new(RwLock::new(())));
        registry.reserve_with("acp-a", || 1);
        registry.reserve_with("acp-b", || 2);
        assert!(registry.try_acquire_resume("provider-1", "acp-a").is_ok());
        // canonical history id 与 runtime id 同时归同一 Smelt 会话所有。
        assert!(registry.try_acquire_resume("provider-2", "acp-a").is_ok());
        assert_eq!(
            registry.resume_owner("provider-1").as_deref(),
            Some("acp-a")
        );
        assert!(registry.try_acquire_resume("provider-3", "acp-b").is_ok());

        registry.release_resume_for("acp-a");
        assert!(registry.resume_owner("provider-1").is_none());
        assert!(registry.resume_owner("provider-2").is_none());
        // 其他会话的占用不受影响。
        assert_eq!(
            registry.resume_owner("provider-3").as_deref(),
            Some("acp-b")
        );

        // 释放后其他会话可以接管。
        assert!(registry.try_acquire_resume("provider-1", "acp-b").is_ok());
    }

    #[test]
    fn removing_session_releases_ownership_in_the_same_registry_transaction() {
        let registry = AcpRegistry::<usize>::new(Arc::new(RwLock::new(())));
        let (slot, _) = registry.reserve_with("acp-a", || 1);
        registry.reserve_with("acp-b", || 2);
        assert!(registry.try_acquire_resume("provider-1", "acp-a").is_ok());

        assert!(registry.remove_if_same("acp-a", &slot).is_some());
        assert!(registry.resume_owner("provider-1").is_none());
        assert!(registry.try_acquire_resume("provider-1", "acp-b").is_ok());
    }

    #[test]
    fn retaining_new_resume_ids_releases_old_aliases_only_after_switch() {
        let registry = AcpRegistry::<usize>::new(Arc::new(RwLock::new(())));
        registry.reserve_with("acp-a", || 1);
        registry.reserve_with("acp-b", || 2);
        registry
            .try_acquire_resumes(&["history-1".into(), "runtime-1".into()], "acp-a")
            .unwrap();
        registry.try_acquire_resume("history-2", "acp-a").unwrap();

        // 新身份先被占住时，旧身份仍不可被另一会话抢占。
        assert!(registry.try_acquire_resume("history-1", "acp-b").is_err());
        registry.retain_resumes_for("acp-a", &["history-2".into()]);

        assert!(registry.try_acquire_resume("history-1", "acp-b").is_ok());
        assert!(registry.try_acquire_resume("runtime-1", "acp-b").is_ok());
        assert_eq!(registry.resume_owner("history-2").as_deref(), Some("acp-a"));
    }
}
