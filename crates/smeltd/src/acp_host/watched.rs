//! 改了就自动通知的锁。
//!
//! 等相位或配置落定的人要在「值变了」上醒来。以前靠约定：改完 `reduced` 都要经
//! 快照推送顺手响一次 Condvar。漏掉一处，等待就白白耗到超时。这里把通知做进锁守卫：
//! 经可变借用碰过值的守卫，放锁之后自动 `notify_all`，改值的地方不必记得通知。

use std::ops::{Deref, DerefMut};
use std::sync::{Condvar, LockResult, Mutex, MutexGuard};
use std::time::Duration;

/// 一把锁加一个「值变了」的 Condvar。只读的守卫不通知，可变借用过的守卫放锁后通知。
pub(crate) struct Watched<T> {
    value: Mutex<T>,
    changed: Condvar,
}

impl<T> Watched<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            value: Mutex::new(value),
            changed: Condvar::new(),
        }
    }

    /// 和 `SessionLock::lock` 一样：不把 poison 交给调用方，遇到就取出值并清掉标志。
    pub(crate) fn lock(&self) -> WatchedGuard<'_, T> {
        WatchedGuard::new(self, self.unpoisoned(self.value.lock()))
    }

    /// `condition` 为真就一直等，每次值被改过都醒来重查，最多等 `timeout`。
    /// 返回那时的守卫和是否超时。超时只是上限，不靠它轮询。
    pub(crate) fn wait_timeout_while(
        &self,
        timeout: Duration,
        mut condition: impl FnMut(&T) -> bool,
    ) -> (WatchedGuard<'_, T>, bool) {
        let guard = self.unpoisoned(self.value.lock());
        let (guard, result) = self.unpoisoned(self.changed.wait_timeout_while(
            guard,
            timeout,
            |value| condition(&*value),
        ));
        (WatchedGuard::new(self, guard), result.timed_out())
    }

    /// 单测用来确认场景真的让锁中毒了，或者某条线程没有持锁 panic。
    #[cfg(test)]
    pub(crate) fn is_poisoned(&self) -> bool {
        self.value.is_poisoned()
    }

    fn unpoisoned<G>(&self, result: LockResult<G>) -> G {
        result.unwrap_or_else(|poisoned| {
            self.value.clear_poison();
            poisoned.into_inner()
        })
    }
}

/// `Watched` 的锁守卫。经 `DerefMut` 借过就记成改过，析构时先放锁再通知等待者。
pub(crate) struct WatchedGuard<'a, T> {
    owner: &'a Watched<T>,
    /// 只在析构时取走，好让放锁先于通知。其余时候一定是 `Some`。
    guard: Option<MutexGuard<'a, T>>,
    dirty: bool,
}

impl<'a, T> WatchedGuard<'a, T> {
    fn new(owner: &'a Watched<T>, guard: MutexGuard<'a, T>) -> Self {
        Self {
            owner,
            guard: Some(guard),
            dirty: false,
        }
    }
}

impl<T> Deref for WatchedGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.guard.as_deref().expect("守卫只在析构时放锁")
    }
}

impl<T> DerefMut for WatchedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.dirty = true;
        self.guard.as_deref_mut().expect("守卫只在析构时放锁")
    }
}

impl<T> Drop for WatchedGuard<'_, T> {
    fn drop(&mut self) {
        // 先放锁再叫醒，醒来的人不用马上又堵在这把锁上。
        drop(self.guard.take());
        if self.dirty {
            self.owner.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// 足够宽的上限：正常情况下改动一来就醒，到这个上限只说明通知丢了。
    const GENEROUS: Duration = Duration::from_secs(30);

    #[test]
    fn mutation_through_guard_wakes_waiter_without_explicit_notify() {
        let watched = Arc::new(Watched::new(0_u32));
        let mut writer = None;
        // 写线程在等待者第一次查条件时才起。条件是持锁查的，写线程要等等待者进了
        // `wait` 放锁才拿得到锁，所以叫醒它的只能是写完放锁时的那次通知。
        let (value, timed_out) = watched.wait_timeout_while(GENEROUS, |value| {
            if writer.is_none() {
                let watched = watched.clone();
                writer = Some(thread::spawn(move || {
                    *watched.lock() = 1;
                }));
            }
            *value == 0
        });
        assert!(!timed_out, "改值后守卫放锁应当叫醒等待者");
        assert_eq!(*value, 1);
        drop(value);
        writer.take().unwrap().join().unwrap();
    }

    #[test]
    fn read_only_guard_does_not_count_as_change() {
        let watched = Watched::new(0_u32);
        assert!(!watched.lock().dirty);
        let read = watched.lock();
        assert_eq!(*read, 0);
        assert!(!read.dirty, "只经 Deref 读不算改过");
        drop(read);
        let mut written = watched.lock();
        *written += 1;
        assert!(written.dirty, "经 DerefMut 写要记成改过");
    }
}
