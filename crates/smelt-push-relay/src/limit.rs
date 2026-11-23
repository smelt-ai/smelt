//! 按设备令牌限流（GCRA）：句柄泄露时最多只能拿来低频骚扰，刷不了屏。

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use governor::Quota;
use governor::clock::{Clock, DefaultClock, Reference};
use governor::middleware::NoOpMiddleware;
use governor::nanos::Nanos;
use governor::state::StateStore;
use governor::state::keyed::{DashMapStateStore, ShrinkableKeyedStateStore};

/// 一台手机允许连着发这么多条。
const BURST: u32 = 10;
/// 之后每分钟补这么多条。正常使用远低于它：提醒本身有去重和延迟窗口。
const PER_MINUTE: u32 = 6;
/// 记的手机上限：满额时清理已补满的桶；仍满额则拒绝新 key，防止活跃 key 洪泛打爆内存。
const MAX_TRACKED: usize = 100_000;
/// 两次全表清理之间的最短冷却间隔，避免满表洪泛时每次请求都做 O(N) 扫描。
const PRUNE_COOLDOWN: Duration = Duration::from_secs(1);
const NEVER_PRUNED: u64 = u64::MAX;

/// 共享给 `governor::RateLimiter` 与外层容量检查的分片状态存储。
#[derive(Clone, Default)]
struct SharedDashMap(Arc<DashMapStateStore<String>>);

impl StateStore for SharedDashMap {
    type Key = String;

    fn measure_and_replace<T, F, E>(&self, key: &Self::Key, f: F) -> Result<T, E>
    where
        F: Fn(Option<Nanos>) -> Result<(T, Nanos), E>,
    {
        self.0.measure_and_replace(key, f)
    }
}

impl ShrinkableKeyedStateStore<String> for SharedDashMap {
    fn retain_recent(&self, drop_below: Nanos) {
        ShrinkableKeyedStateStore::retain_recent(&*self.0, drop_below);
    }

    fn shrink_to_fit(&self) {
        ShrinkableKeyedStateStore::shrink_to_fit(&*self.0);
    }

    fn len(&self) -> usize {
        ShrinkableKeyedStateStore::len(&*self.0)
    }

    fn is_empty(&self) -> bool {
        ShrinkableKeyedStateStore::is_empty(&*self.0)
    }
}

pub struct RateLimiter<C: Clock = DefaultClock> {
    inner: governor::RateLimiter<String, SharedDashMap, C, NoOpMiddleware<C::Instant>>,
    store: Arc<DashMapStateStore<String>>,
    clock: C,
    start: C::Instant,
    max_tracked: usize,
    last_prune_ns: AtomicU64,
}

impl Default for RateLimiter<DefaultClock> {
    fn default() -> Self {
        Self::with_clock(MAX_TRACKED, DefaultClock::default())
    }
}

impl<C: Clock + Clone> RateLimiter<C> {
    fn with_clock(max_tracked: usize, clock: C) -> Self {
        let quota = Quota::per_minute(NonZeroU32::new(PER_MINUTE).expect("PER_MINUTE > 0"))
            .allow_burst(NonZeroU32::new(BURST).expect("BURST > 0"));
        let store = Arc::new(DashMapStateStore::default());
        let start = clock.now();
        let inner =
            governor::RateLimiter::new(quota, SharedDashMap(Arc::clone(&store)), clock.clone());
        Self {
            inner,
            store,
            clock,
            start,
            max_tracked,
            last_prune_ns: AtomicU64::new(NEVER_PRUNED),
        }
    }

    pub fn allow(&self, key: &str) -> bool {
        if !self.store.contains_key(key) && self.store.len() >= self.max_tracked {
            self.prune_if_cooled();
            if self.store.len() >= self.max_tracked {
                return false;
            }
        }
        self.inner.check_key(&key.to_owned()).is_ok()
    }

    fn prune_if_cooled(&self) {
        let now_ns = Duration::from(self.clock.now().duration_since(self.start)).as_nanos() as u64;
        let cooldown_ns = PRUNE_COOLDOWN.as_nanos() as u64;
        let prev = self.last_prune_ns.load(Ordering::Acquire);
        if prev != NEVER_PRUNED && now_ns.saturating_sub(prev) < cooldown_ns {
            return;
        }
        if self
            .last_prune_ns
            .compare_exchange(prev, now_ns, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            self.inner.retain_recent();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use governor::clock::FakeRelativeClock;

    #[test]
    fn bursts_then_refills_per_device() {
        let clock = FakeRelativeClock::default();
        let limiter = RateLimiter::with_clock(MAX_TRACKED, clock.clone());
        for _ in 0..BURST {
            assert!(limiter.allow("phone"));
        }
        assert!(!limiter.allow("phone"));
        assert!(limiter.allow("other"), "各算各的");
        clock.advance(Duration::from_secs(10));
        assert!(limiter.allow("phone"));
        assert!(!limiter.allow("phone"));
    }

    #[test]
    fn active_key_flood_never_grows_unbounded() {
        let clock = FakeRelativeClock::default();
        let cap = 64;
        let limiter = RateLimiter::with_clock(cap, clock);
        for i in 0..cap {
            assert!(limiter.allow(&format!("active-{i}")));
        }
        for i in 0..200 {
            assert!(
                !limiter.allow(&format!("flood-{i}")),
                "满额且全为活跃 key 时应拒绝新 key，防止内存无界增长"
            );
        }
        assert!(
            limiter.inner.len() <= cap,
            "活跃 key 洪泛后表长 {} 超过了上限 {cap}",
            limiter.inner.len()
        );
        // 既有活跃设备不受洪泛挤出，仍按原配额继续计量。
        for _ in 1..BURST {
            assert!(limiter.allow("active-0"));
        }
        assert!(!limiter.allow("active-0"));
    }

    #[test]
    fn recovers_capacity_after_stale_keys_expire() {
        let clock = FakeRelativeClock::default();
        let cap = 16;
        let limiter = RateLimiter::with_clock(cap, clock.clone());
        for i in 0..cap {
            assert!(limiter.allow(&format!("device-{i}")));
        }
        assert!(!limiter.allow("blocked-now"));

        // 推进超过满额恢复期，旧桶失效后应自动清理并恢复接纳新设备。
        clock.advance(Duration::from_secs(120));
        assert!(limiter.allow("new-device"));
        assert_eq!(limiter.inner.len(), 1);
    }
}
