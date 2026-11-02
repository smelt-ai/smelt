//! 额度请求共用的 HTTP 客户端和错误。
//!
//! 从访达或程序坞拉起的 smeltd，进程环境里没有用户写在 `.zshrc` 里的代理变量，只有
//! login shell 快照（[`crate::login_env`]）里才有。额度请求带着账户令牌，走错出口有
//! 封号风险，所以这里显式接上代理，并和 Agent CLI 一样以 login shell 为准；代理配置写错时
//! 报错，不退回直连。
//!
//! 额度接口全是 HTTPS，所以只认 `HTTPS_PROXY` 和 `ALL_PROXY`。`HTTP_PROXY` 管不到这些请求，
//! 接上它反而会关掉 reqwest 对系统代理的探测，让带令牌的请求直连。

use std::fmt;
use std::time::Duration;

use reqwest::{ClientBuilder, NoProxy, Proxy, StatusCode};

const QUOTA_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// 额度请求用的客户端：有代理变量时用它，没有时留给 reqwest 探测系统代理。
pub(crate) fn quota_http_client() -> anyhow::Result<reqwest::Client> {
    let login = crate::login_env::login_environment();
    let proxies = ProxyEnv::resolve(|name| login.get(name).cloned())
        .or(ProxyEnv::resolve(|name| std::env::var(name).ok()));
    let builder = reqwest::Client::builder().timeout(QUOTA_REQUEST_TIMEOUT);
    Ok(proxies.apply(builder)?.build()?)
}

/// 生效的代理变量。查不到或是空值的都是 `None`。
#[derive(Debug, Default, PartialEq, Eq)]
struct ProxyEnv {
    https: Option<String>,
    all: Option<String>,
    no_proxy: Option<String>,
}

impl ProxyEnv {
    fn resolve(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let first = |names: [&str; 2]| {
            names.into_iter().find_map(|name| {
                lookup(name)
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
        };
        Self {
            https: first(["HTTPS_PROXY", "https_proxy"]),
            all: first(["ALL_PROXY", "all_proxy"]),
            no_proxy: first(["NO_PROXY", "no_proxy"]),
        }
    }

    /// 逐项回退：`self` 没有的那一项才用 `fallback` 的。
    fn or(self, fallback: Self) -> Self {
        Self {
            https: self.https.or(fallback.https),
            all: self.all.or(fallback.all),
            no_proxy: self.no_proxy.or(fallback.no_proxy),
        }
    }

    /// 没配代理变量时不动 builder，留给 reqwest 探测系统代理。错误里不带变量的值：代理地址
    /// 可能含密码。
    fn apply(&self, mut builder: ClientBuilder) -> anyhow::Result<ClientBuilder> {
        let no_proxy = || self.no_proxy.as_deref().and_then(NoProxy::from_string);
        if let Some(url) = &self.https {
            let proxy = Proxy::https(url).map_err(|_| invalid_proxy("HTTPS_PROXY"))?;
            builder = builder.proxy(proxy.no_proxy(no_proxy()));
        }
        if let Some(url) = &self.all {
            let proxy = Proxy::all(url).map_err(|_| invalid_proxy("ALL_PROXY"))?;
            builder = builder.proxy(proxy.no_proxy(no_proxy()));
        }
        Ok(builder)
    }
}

fn invalid_proxy(name: &str) -> anyhow::Error {
    anyhow::anyhow!("代理变量 {name} 的地址无效，已取消额度查询，没有改走直连")
}

/// 额度接口回的非成功状态。不带响应体：内部接口有时会返回账户或认证细节。
#[derive(Debug)]
pub(crate) struct QuotaEndpointError {
    pub(crate) provider: &'static str,
    pub(crate) status: StatusCode,
}

impl fmt::Display for QuotaEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} quota endpoint returned HTTP {}",
            self.provider, self.status
        )
    }
}

impl std::error::Error for QuotaEndpointError {}

/// 令牌被接口拒绝（多半过期了）。
pub(crate) fn is_unauthorized(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<QuotaEndpointError>()
        .is_some_and(|error| error.status == StatusCode::UNAUTHORIZED)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from(pairs: &[(&str, &str)]) -> ProxyEnv {
        ProxyEnv::resolve(|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        })
    }

    #[test]
    fn reads_upper_and_lower_case_and_ignores_empty_values() {
        let env = env_from(&[
            ("HTTPS_PROXY", "  "),
            ("https_proxy", "http://127.0.0.1:7890"),
            ("ALL_PROXY", "socks5://127.0.0.1:7891"),
            ("no_proxy", "localhost,127.0.0.1"),
        ]);
        assert_eq!(env.https.as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(env.all.as_deref(), Some("socks5://127.0.0.1:7891"));
        assert_eq!(env.no_proxy.as_deref(), Some("localhost,127.0.0.1"));
    }

    /// 只配 `HTTP_PROXY` 不能算配了代理：接上它会关掉系统代理探测，HTTPS 请求就直连了。
    #[test]
    fn http_proxy_alone_leaves_system_proxy_detection_on() {
        let env = env_from(&[("HTTP_PROXY", "http://a:1"), ("http_proxy", "http://a:1")]);
        assert_eq!(env, ProxyEnv::default());
    }

    /// 和 Agent CLI 走同一个出口：同一项以 login shell 为准，大小写不同也一样；它没有的才用
    /// 进程环境的。
    #[test]
    fn login_shell_values_win_item_by_item() {
        let login = env_from(&[("https_proxy", "http://login:1")]);
        let process = env_from(&[
            ("HTTPS_PROXY", "http://process:1"),
            ("ALL_PROXY", "socks5://process:2"),
        ]);
        let env = login.or(process);
        assert_eq!(env.https.as_deref(), Some("http://login:1"));
        assert_eq!(env.all.as_deref(), Some("socks5://process:2"));
    }

    /// Clash、Surge 常见的几种写法都要能建出客户端，SOCKS 依赖 reqwest 的 `socks` 特性。
    #[test]
    fn common_proxy_forms_build_a_client() {
        for url in [
            "http://127.0.0.1:7890",
            "127.0.0.1:7890",
            "socks5://127.0.0.1:7891",
            "socks5h://user:secret@127.0.0.1:7891",
        ] {
            let env = env_from(&[("ALL_PROXY", url), ("NO_PROXY", "localhost")]);
            let builder = env.apply(reqwest::Client::builder()).unwrap();
            assert!(builder.build().is_ok(), "{url}");
        }
    }

    /// 带令牌的请求不能因为代理写错就改走直连；错误里也不能出现代理密码。
    #[test]
    fn an_invalid_proxy_fails_without_leaking_its_value() {
        let env = env_from(&[("HTTPS_PROXY", "http://user:secret@[bad")]);
        let Err(error) = env.apply(reqwest::Client::builder()) else {
            panic!("无效代理不能建出客户端");
        };
        assert!(error.to_string().contains("HTTPS_PROXY"));
        assert!(!error.to_string().contains("secret"));
    }

    #[test]
    fn only_http_401_counts_as_a_rejected_token() {
        let error = |status| {
            anyhow::Error::from(QuotaEndpointError {
                provider: "Claude",
                status,
            })
        };
        assert!(is_unauthorized(&error(StatusCode::UNAUTHORIZED)));
        assert!(!is_unauthorized(&error(StatusCode::FORBIDDEN)));
        assert!(!is_unauthorized(&anyhow::anyhow!("offline")));
        assert_eq!(
            error(StatusCode::UNAUTHORIZED).to_string(),
            "Claude quota endpoint returned HTTP 401 Unauthorized"
        );
    }
}
