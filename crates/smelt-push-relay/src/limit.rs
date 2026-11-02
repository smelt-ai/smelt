//! 按设备令牌限流（令牌桶）：句柄泄露时最多只能拿来低频骚扰，刷不了屏。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// 一台手机允许连着发这么多条。
const BURST: f64 = 10.0;
/// 之后每分钟补这么多条。正常使用远低于它：提醒本身有去重和延迟窗口。
const PER_MINUTE: f64 = 6.0;
/// 记的手机超过这么多就清掉已经补满的桶（它们和新来的没区别）。
const MAX_TRACKED: usize = 100_000;

struct Bucket {
    tokens: f64,
    updated: Instant,
}

#[derive(Default)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn allow(&self, key: &str, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() >= MAX_TRACKED && !buckets.contains_key(key) {
            buckets.retain(|_, bucket| refilled(bucket, now) < BURST);
        }
        let bucket = buckets.entry(key.to_string()).or_insert(Bucket {
            tokens: BURST,
            updated: now,
        });
        bucket.tokens = refilled(bucket, now);
        bucket.updated = now;
        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }
}

fn refilled(bucket: &Bucket, now: Instant) -> f64 {
    let minutes = now.saturating_duration_since(bucket.updated).as_secs_f64() / 60.0;
    (bucket.tokens + minutes * PER_MINUTE).min(BURST)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bursts_then_refills_per_device() {
        let limiter = RateLimiter::default();
        let start = Instant::now();
        for _ in 0..BURST as usize {
            assert!(limiter.allow("phone", start));
        }
        assert!(!limiter.allow("phone", start));
        assert!(limiter.allow("other", start), "各算各的");
        assert!(limiter.allow("phone", start + Duration::from_secs(10)));
        assert!(!limiter.allow("phone", start + Duration::from_secs(10)));
    }
}
