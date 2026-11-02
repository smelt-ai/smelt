//! 额度查询读 macOS 钥匙串的规矩。
//!
//! 每次 `security find-generic-password` 都可能弹一个模态授权窗口，而额度每 5 分钟查一轮。
//! 所以这里保证：
//!
//! - 读到的令牌留在守护进程内存里，后台刷新直接复用，不再调 `security`。
//! - 令牌被接口拒绝（多半过期）时才重读，后台最多 [`BACKGROUND_REREAD_AFTER`] 一次。令牌
//!   过期而 CLI 又没在跑时，重读拿到的还是旧令牌，不限频就又成了定时弹窗。
//! - 用户拒绝后这家进入熔断，状态存在运行时库里，守护重启、升级也不会再弹，直到用户手动
//!   刷新。退出码不是「没有这一项」的都按拒绝算：宁可少查一次额度，也不反复打断用户。
//! - 同一时间只读一次：窗口还开着时，再来的刷新直接返回「忙」，不叠第二个窗口，也不算查询
//!   失败（[`KeychainBusy`]），这家上次的额度保持原样。
//!
//! 令牌只在内存里，不写库、不进日志，`CachedToken` 也不实现 `Debug`。

use std::collections::HashMap;
use std::fmt;
use std::process::Command;
use std::sync::{LazyLock, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

/// 令牌被拒绝后，后台两次重读钥匙串的最短间隔。
const BACKGROUND_REREAD_AFTER: Duration = Duration::from_secs(3600);

/// `security` 找不到钥匙串项时的退出码（`errSecItemNotFound` 取低 8 位）。
const SECURITY_EXIT_ITEM_NOT_FOUND: i32 = 44;

/// 这次读凭据是谁发起的，决定能不能弹窗。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialAccess {
    /// 启动和定时刷新：能复用就复用，熔断时不碰钥匙串。
    Background,
    /// 用户点了刷新或重试：重读钥匙串（顺带发现换号），并解除熔断。
    UserInitiated,
}

/// 一次钥匙串读取的结果。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeychainRead {
    Secret(String),
    NotFound,
    Denied,
}

/// 另一轮刷新正在等授权窗口。这一轮没有结果，不是额度失效。
#[derive(Debug)]
pub(crate) struct KeychainBusy;

impl fmt::Display for KeychainBusy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("正在等待钥匙串授权")
    }
}

impl std::error::Error for KeychainBusy {}

/// 查询方拿到的结果。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeychainLookup {
    Token(String),
    NotFound,
    Denied,
    /// 另一轮刷新正在等授权窗口。
    Busy,
}

impl KeychainLookup {
    /// `Denied` / `Busy` 对应的错误；其余返回 `None`。
    pub(crate) fn blocked_error(&self) -> Option<anyhow::Error> {
        match self {
            Self::Denied => Some(anyhow::anyhow!(
                "钥匙串访问被拒绝或不可用，已停止自动读取；在设置里手动重试后恢复"
            )),
            Self::Busy => Some(KeychainBusy.into()),
            Self::Token(_) | Self::NotFound => None,
        }
    }
}

/// 执行 `security find-generic-password -w`。
pub(crate) fn find_generic_password(service: &str, account: Option<&str>) -> KeychainRead {
    let mut command = Command::new("security");
    command.arg("find-generic-password");
    if let Some(account) = account {
        command.args(["-a", account]);
    }
    let Ok(output) = command.args(["-s", service, "-w"]).output() else {
        return KeychainRead::NotFound;
    };
    if output.status.success() {
        return String::from_utf8(output.stdout)
            .map(KeychainRead::Secret)
            .unwrap_or(KeychainRead::NotFound);
    }
    classify_failure(output.status.code())
}

/// 用户点拒绝、按 ESC 是 128（`errSecUserCanceled`），认证失败、不能交互等各有各的码。
/// 除了「没有这一项」，都不该接着每 5 分钟再试。
fn classify_failure(exit_code: Option<i32>) -> KeychainRead {
    match exit_code {
        Some(SECURITY_EXIT_ITEM_NOT_FOUND) => KeychainRead::NotFound,
        _ => KeychainRead::Denied,
    }
}

struct CachedToken {
    token: String,
    read_at: Instant,
}

/// 按「账户 + 钥匙串项」存的令牌。一个账户可能有多项（Copilot 的多个登录用户）。
#[derive(Default)]
struct TokenCache {
    tokens: HashMap<(String, String), CachedToken>,
}

impl TokenCache {
    fn lookup(
        &mut self,
        provider: &str,
        item: &str,
        denied: bool,
        access: CredentialAccess,
        now: Instant,
        read: impl FnOnce() -> KeychainRead,
    ) -> KeychainLookup {
        if access == CredentialAccess::Background {
            if denied {
                return KeychainLookup::Denied;
            }
            if let Some(cached) = self.tokens.get(&cache_key(provider, item)) {
                return KeychainLookup::Token(cached.token.clone());
            }
        }
        self.apply_read(provider, item, now, read())
    }

    /// 令牌 `rejected` 被接口拒绝后要一个新的。`Token` 只会是和它不同的令牌；没有新令牌
    /// （还没到重读时间、重读后还是它、项没了）都是 `NotFound`。
    fn after_rejection(
        &mut self,
        provider: &str,
        item: &str,
        rejected: &str,
        now: Instant,
        read: impl FnOnce() -> KeychainRead,
    ) -> KeychainLookup {
        let Some(cached) = self.tokens.get(&cache_key(provider, item)) else {
            return KeychainLookup::NotFound;
        };
        if cached.token != rejected {
            return KeychainLookup::Token(cached.token.clone());
        }
        if now.saturating_duration_since(cached.read_at) < BACKGROUND_REREAD_AFTER {
            return KeychainLookup::NotFound;
        }
        match self.apply_read(provider, item, now, read()) {
            KeychainLookup::Token(token) if token == rejected => KeychainLookup::NotFound,
            other => other,
        }
    }

    fn apply_read(
        &mut self,
        provider: &str,
        item: &str,
        now: Instant,
        read: KeychainRead,
    ) -> KeychainLookup {
        match read {
            KeychainRead::Secret(token) => {
                self.tokens.insert(
                    cache_key(provider, item),
                    CachedToken {
                        token: token.clone(),
                        read_at: now,
                    },
                );
                KeychainLookup::Token(token)
            }
            KeychainRead::NotFound => {
                self.tokens.remove(&cache_key(provider, item));
                KeychainLookup::NotFound
            }
            KeychainRead::Denied => {
                self.forget(provider);
                KeychainLookup::Denied
            }
        }
    }

    fn forget(&mut self, provider: &str) {
        self.tokens.retain(|(owner, _), _| owner != provider);
    }
}

fn cache_key(provider: &str, item: &str) -> (String, String) {
    (provider.to_string(), item.to_string())
}

static TOKEN_CACHE: LazyLock<Mutex<TokenCache>> = LazyLock::new(Mutex::default);

/// 读钥匙串时一直占着锁，所以拿不到锁就是有窗口开着。
fn try_token_cache() -> Option<MutexGuard<'static, TokenCache>> {
    match TOKEN_CACHE.try_lock() {
        Ok(cache) => Some(cache),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// 取 `provider` 在钥匙串项 `item` 上的令牌。`provider` 是 `QuotaTarget::id()`。
pub(crate) fn keychain_token(
    provider: &str,
    item: &str,
    access: CredentialAccess,
    read: impl FnOnce() -> KeychainRead,
) -> KeychainLookup {
    let Some(mut cache) = try_token_cache() else {
        return KeychainLookup::Busy;
    };
    let denied = keychain_denied(provider);
    let result = cache.lookup(provider, item, denied, access, Instant::now(), read);
    record_denial_change(provider, denied, &result);
    result
}

/// 令牌 `rejected` 被接口拒绝后取新令牌。没有新令牌时返回 `None`，调用方照常报原来的错。
pub(crate) fn keychain_token_after_rejection(
    provider: &str,
    item: &str,
    rejected: &str,
    read: impl FnOnce() -> KeychainRead,
) -> Option<String> {
    let mut cache = try_token_cache()?;
    let denied = keychain_denied(provider);
    if denied {
        return None;
    }
    let result = cache.after_rejection(provider, item, rejected, Instant::now(), read);
    record_denial_change(provider, denied, &result);
    match result {
        KeychainLookup::Token(token) => Some(token),
        KeychainLookup::NotFound | KeychainLookup::Denied | KeychainLookup::Busy => None,
    }
}

/// 关掉一家的额度查询时丢掉它的令牌。窗口开着拿不到锁也不要紧：关掉之后不会再用，重新
/// 打开走用户手动刷新，会重读。
pub(crate) fn forget_keychain_tokens(provider: &str) {
    if let Some(mut cache) = try_token_cache() {
        cache.forget(provider);
    }
}

fn keychain_denied(provider: &str) -> bool {
    crate::sqlite_state::default_sqlite_store()
        .ok()
        .and_then(|store| store.quota_provider_prefs().ok())
        .is_some_and(|prefs| {
            prefs
                .iter()
                .any(|pref| pref.provider_id == provider && pref.keychain_denied)
        })
}

/// 拒绝状态只存在库里一处。读到了（或确认没有这一项）就解除，读取被拒就记下。
fn record_denial_change(provider: &str, denied_before: bool, result: &KeychainLookup) {
    let denied_now = match result {
        KeychainLookup::Denied => true,
        KeychainLookup::Token(_) | KeychainLookup::NotFound => false,
        KeychainLookup::Busy => denied_before,
    };
    if denied_now == denied_before {
        return;
    }
    if let Ok(store) = crate::sqlite_state::default_sqlite_store() {
        let _ = store.set_quota_keychain_denied(provider, denied_now);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    use CredentialAccess::{Background, UserInitiated};

    /// 记下读了几次钥匙串。
    fn counted(reads: &Cell<u32>, result: KeychainRead) -> impl FnOnce() -> KeychainRead + '_ {
        move || {
            reads.set(reads.get() + 1);
            result
        }
    }

    fn secret(token: &str) -> KeychainRead {
        KeychainRead::Secret(token.to_string())
    }

    fn token(token: &str) -> KeychainLookup {
        KeychainLookup::Token(token.to_string())
    }

    fn cache_with(token_value: &str, read_at: Instant) -> TokenCache {
        let mut cache = TokenCache::default();
        cache.apply_read("claude", "item", read_at, secret(token_value));
        cache
    }

    #[test]
    fn exit_44_is_a_missing_item_and_everything_else_stops_retrying() {
        assert_eq!(classify_failure(Some(44)), KeychainRead::NotFound);
        for code in [Some(128), Some(51), Some(36), Some(1), None] {
            assert_eq!(classify_failure(code), KeychainRead::Denied, "{code:?}");
        }
    }

    /// 授权过之后，后台每 5 分钟一轮都不再调 `security`。
    #[test]
    fn background_refresh_reads_the_keychain_only_once() {
        let mut cache = TokenCache::default();
        let reads = Cell::new(0);
        let now = Instant::now();
        for _ in 0..3 {
            assert_eq!(
                cache.lookup(
                    "claude",
                    "item",
                    false,
                    Background,
                    now,
                    counted(&reads, secret("t1"))
                ),
                token("t1")
            );
        }
        assert_eq!(reads.get(), 1);
    }

    /// 手动刷新总是重读：用户可能刚换了号。
    #[test]
    fn user_refresh_rereads_and_picks_up_a_new_token() {
        let now = Instant::now();
        let mut cache = cache_with("old", now);
        let reads = Cell::new(0);
        assert_eq!(
            cache.lookup(
                "claude",
                "item",
                false,
                UserInitiated,
                now,
                counted(&reads, secret("new"))
            ),
            token("new")
        );
        assert_eq!(reads.get(), 1);
    }

    /// 熔断后后台永远不碰钥匙串，只有用户手动才读。
    #[test]
    fn a_denied_provider_is_only_read_on_user_request() {
        let mut cache = TokenCache::default();
        let reads = Cell::new(0);
        let now = Instant::now();
        assert_eq!(
            cache.lookup(
                "claude",
                "item",
                true,
                Background,
                now,
                counted(&reads, secret("t1"))
            ),
            KeychainLookup::Denied
        );
        assert_eq!(reads.get(), 0);
        assert_eq!(
            cache.lookup(
                "claude",
                "item",
                true,
                UserInitiated,
                now,
                counted(&reads, secret("t1"))
            ),
            token("t1")
        );
        assert_eq!(reads.get(), 1);
    }

    /// 拒绝后这家的令牌全部作废，别的账户不受影响。
    #[test]
    fn denial_drops_the_tokens_of_that_provider_only() {
        let now = Instant::now();
        let mut cache = cache_with("t1", now);
        cache.apply_read("claude", "other", now, secret("t2"));
        cache.apply_read("copilot", "item", now, secret("t3"));
        assert_eq!(
            cache.lookup("claude", "item", false, UserInitiated, now, || {
                KeychainRead::Denied
            }),
            KeychainLookup::Denied
        );
        assert_eq!(cache.tokens.len(), 1);
        assert!(cache.tokens.contains_key(&cache_key("copilot", "item")));
    }

    /// 没有这一项不弹窗，不缓存：用户之后登录，下一轮就能读到。
    #[test]
    fn a_missing_item_is_not_cached() {
        let mut cache = TokenCache::default();
        let reads = Cell::new(0);
        let now = Instant::now();
        for _ in 0..2 {
            assert_eq!(
                cache.lookup(
                    "claude",
                    "item",
                    false,
                    Background,
                    now,
                    counted(&reads, KeychainRead::NotFound)
                ),
                KeychainLookup::NotFound
            );
        }
        assert_eq!(reads.get(), 2);
    }

    /// CLI 没在跑时令牌过期后一直是 401：不能每 5 分钟重读一次，那又成了定时弹窗。
    #[test]
    fn a_rejected_token_is_not_reread_within_the_window() {
        let start = Instant::now();
        let mut cache = cache_with("t1", start);
        let reads = Cell::new(0);
        let soon = start + BACKGROUND_REREAD_AFTER - Duration::from_secs(1);
        assert_eq!(
            cache.after_rejection("claude", "item", "t1", soon, counted(&reads, secret("t2"))),
            KeychainLookup::NotFound
        );
        assert_eq!(reads.get(), 0);
    }

    #[test]
    fn a_rejected_token_is_reread_after_the_window() {
        let start = Instant::now();
        let mut cache = cache_with("t1", start);
        let later = start + BACKGROUND_REREAD_AFTER;
        assert_eq!(
            cache.after_rejection("claude", "item", "t1", later, || secret("t2")),
            token("t2")
        );
    }

    /// 重读还是旧令牌：没有新令牌，而且读取时间刷新，下一次重读又要等一个窗口。
    #[test]
    fn rereading_the_same_token_restarts_the_window() {
        let start = Instant::now();
        let mut cache = cache_with("t1", start);
        let reads = Cell::new(0);
        let later = start + BACKGROUND_REREAD_AFTER;
        assert_eq!(
            cache.after_rejection("claude", "item", "t1", later, counted(&reads, secret("t1"))),
            KeychainLookup::NotFound
        );
        assert_eq!(
            cache.after_rejection(
                "claude",
                "item",
                "t1",
                later + Duration::from_secs(300),
                counted(&reads, secret("t1"))
            ),
            KeychainLookup::NotFound
        );
        assert_eq!(reads.get(), 1);
    }

    /// 缓存里已经是别的令牌（另一轮刷新换过了）：直接用，不读钥匙串。
    #[test]
    fn a_rejection_of_an_outdated_token_uses_the_cached_one() {
        let now = Instant::now();
        let mut cache = cache_with("t2", now);
        let reads = Cell::new(0);
        assert_eq!(
            cache.after_rejection("claude", "item", "t1", now, counted(&reads, secret("t3"))),
            token("t2")
        );
        assert_eq!(reads.get(), 0);
    }

    #[test]
    fn denial_while_rereading_a_rejected_token_trips_the_breaker() {
        let start = Instant::now();
        let mut cache = cache_with("t1", start);
        assert_eq!(
            cache.after_rejection(
                "claude",
                "item",
                "t1",
                start + BACKGROUND_REREAD_AFTER,
                || KeychainRead::Denied
            ),
            KeychainLookup::Denied
        );
        assert!(cache.tokens.is_empty());
    }

    #[test]
    fn only_denied_and_busy_lookups_block_the_query() {
        assert!(KeychainLookup::Denied.blocked_error().is_some());
        let busy = KeychainLookup::Busy.blocked_error().unwrap();
        assert!(busy.downcast_ref::<KeychainBusy>().is_some());
        assert!(KeychainLookup::NotFound.blocked_error().is_none());
        assert!(token("t").blocked_error().is_none());
    }
}
