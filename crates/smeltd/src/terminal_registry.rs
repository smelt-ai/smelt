use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A terminal ID is reserved before its PTY exists. This makes `Starting` visible to a
/// concurrent open/kill without publishing a half-created runtime as a live session.
pub(crate) struct TerminalSlot<T> {
    pub(crate) lifecycle: Mutex<()>,
    value: Mutex<Option<Arc<T>>>,
}

pub(crate) struct TerminalRegistry<T> {
    state: Mutex<HashMap<String, Arc<TerminalSlot<T>>>>,
}

impl<T> Default for TerminalRegistry<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TerminalRegistry<T> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Atomically reserve an ID. A caller which receives `created = true` owns the Starting
    /// transition after it acquires `slot.lifecycle`; all other callers wait for that transition.
    pub(crate) fn reserve(&self, id: &str) -> (Arc<TerminalSlot<T>>, bool) {
        use std::collections::hash_map::Entry;

        let mut state = self.state.lock().unwrap();
        match state.entry(id.to_string()) {
            Entry::Occupied(entry) => (Arc::clone(entry.get()), false),
            Entry::Vacant(entry) => {
                let slot = Arc::new(TerminalSlot {
                    lifecycle: Mutex::new(()),
                    value: Mutex::new(None),
                });
                entry.insert(Arc::clone(&slot));
                (slot, true)
            }
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<TerminalSlot<T>>> {
        self.state.lock().unwrap().get(id).cloned()
    }

    pub(crate) fn is_current(&self, id: &str, expected: &Arc<TerminalSlot<T>>) -> bool {
        self.state
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
    }

    pub(crate) fn live(&self, id: &str) -> Option<Arc<T>> {
        let slot = self.get(id)?;
        self.live_in_slot(&slot)
    }

    pub(crate) fn live_in_slot(&self, slot: &Arc<TerminalSlot<T>>) -> Option<Arc<T>> {
        slot.value.lock().unwrap().clone()
    }

    /// Run one operation against the currently published runtime for `id`. The callback runs
    /// while that slot's lifecycle lock is held and must not reacquire it.
    pub(crate) fn with_live<R>(&self, id: &str, operation: impl FnOnce(&Arc<T>) -> R) -> Option<R> {
        let mut operation = Some(operation);
        loop {
            let slot = self.get(id)?;
            let _lifecycle = slot.lifecycle.lock().unwrap();
            if !self.is_current(id, &slot) {
                continue;
            }
            let value = self.live_in_slot(&slot)?;
            return Some(operation.take().unwrap()(&value));
        }
    }

    /// Run one operation against an exact slot captured by an earlier lookup. The callback runs
    /// while `expected.lifecycle` is held and must not reacquire that same lifecycle lock.
    pub(crate) fn with_current<R>(
        &self,
        id: &str,
        expected: &Arc<TerminalSlot<T>>,
        operation: impl FnOnce(&Arc<T>) -> R,
    ) -> Option<R> {
        let _lifecycle = expected.lifecycle.lock().unwrap();
        if !self.is_current(id, expected) {
            return None;
        }
        let value = self.live_in_slot(expected)?;
        Some(operation(&value))
    }

    /// Caller must hold `slot.lifecycle`. A stale creator can never publish into a replacement.
    pub(crate) fn commit_if_current(
        &self,
        id: &str,
        slot: &Arc<TerminalSlot<T>>,
        value: Arc<T>,
    ) -> bool {
        if !self.is_current(id, slot) {
            return false;
        }
        let mut current = slot.value.lock().unwrap();
        if current.is_some() {
            return false;
        }
        *current = Some(value);
        true
    }

    /// Caller must hold `slot.lifecycle`. Only the exact instance may remove its registry entry.
    pub(crate) fn remove_if_same(
        &self,
        id: &str,
        expected: &Arc<TerminalSlot<T>>,
    ) -> Option<Arc<T>> {
        let mut state = self.state.lock().unwrap();
        let current = state.get(id)?;
        if !Arc::ptr_eq(current, expected) {
            return None;
        }
        state.remove(id);
        drop(state);
        expected.value.lock().unwrap().take()
    }

    pub(crate) fn snapshot(&self) -> Vec<(String, Arc<T>)> {
        self.snapshot_slots()
            .into_iter()
            .filter_map(|(id, slot)| self.live_in_slot(&slot).map(|value| (id, value)))
            .collect()
    }

    /// 返回所有 slot（包括尚未提交 runtime 的 Starting slot）。启动对账需要先
    /// 锁住这些 slot 的生命周期，再读取 value，避免 PTY 泵在快照和目录绑定之间
    /// 退出而把已死 runtime 重新登记成 Active。
    pub(crate) fn snapshot_slots(&self) -> Vec<(String, Arc<TerminalSlot<T>>)> {
        self.state
            .lock()
            .unwrap()
            .iter()
            .map(|(id, slot)| (id.clone(), Arc::clone(slot)))
            .collect::<Vec<_>>()
    }

    pub(crate) fn len(&self) -> usize {
        self.snapshot().len()
    }

    #[cfg(test)]
    pub(crate) fn insert_for_test(&self, id: &str, value: Arc<T>) {
        let (slot, created) = self.reserve(id);
        assert!(created, "test registry already contains {id}");
        let _lifecycle = slot.lifecycle.lock().unwrap();
        assert!(self.commit_if_current(id, &slot, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn same_id_reserves_one_starting_slot() {
        let registry = Arc::new(TerminalRegistry::<usize>::new());
        let mut threads = Vec::new();
        for _ in 0..8 {
            let registry = Arc::clone(&registry);
            threads.push(std::thread::spawn(move || registry.reserve("same")));
        }

        let reservations = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            reservations.iter().filter(|(_, created)| *created).count(),
            1
        );
        assert!(
            reservations
                .windows(2)
                .all(|pair| Arc::ptr_eq(&pair[0].0, &pair[1].0))
        );
    }

    #[test]
    fn old_slot_cannot_remove_replacement() {
        let registry = TerminalRegistry::new();
        let (old, _) = registry.reserve("id");
        let _guard = old.lifecycle.lock().unwrap();
        assert!(registry.commit_if_current("id", &old, Arc::new(1usize)));
        assert_eq!(*registry.remove_if_same("id", &old).unwrap(), 1);
        drop(_guard);

        let (replacement, created) = registry.reserve("id");
        assert!(created);
        let _replacement_guard = replacement.lifecycle.lock().unwrap();
        assert!(registry.commit_if_current("id", &replacement, Arc::new(2usize)));
        assert!(registry.remove_if_same("id", &old).is_none());
        assert_eq!(*registry.live("id").unwrap(), 2);
    }

    #[test]
    fn current_operation_waits_for_the_slot_lifecycle() {
        let registry = Arc::new(TerminalRegistry::new());
        registry.insert_for_test("id", Arc::new(1usize));
        let slot = registry.get("id").unwrap();
        let lifecycle = slot.lifecycle.lock().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (ran_tx, ran_rx) = mpsc::channel();
        let worker_registry = Arc::clone(&registry);
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            worker_registry.with_live("id", |_| ran_tx.send(()).unwrap())
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            ran_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "副作用必须等待 lifecycle，不能只凭 id -> Arc 直接执行"
        );
        drop(lifecycle);
        assert!(worker.join().unwrap().is_some());
        ran_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}
