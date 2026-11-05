//! 让 Smelt 起的子进程跟随系统代理。
//!
//! smelt-agent 跑在 Bun 上，Bun 的 `fetch` 只认 `HTTP(S)_PROXY` / `NO_PROXY` 环境变量，
//! 不读 Windows「设置 → 代理」、也不读 macOS「网络 → 代理」；Claude Code、Gemini CLI
//! 这类 Node 写的 ACP agent 和 npm 也一样。只在系统里配了代理的用户
//! （公司电脑、代理工具开了系统代理），浏览器能过、agent 却直连：OAuth 令牌兑换被按地区拒绝
//! （`unsupported_country_region_territory`），模型请求同理。额度请求走 reqwest，它自己
//! 会读系统代理，没有这个问题（见 `quota_http`）。
//!
//! 所以起 Pi 之前，把系统代理翻译成环境变量补给它。规则：
//!
//! - **只补，不覆盖**：子进程最终环境里已有任一代理变量（login shell、进程环境、启动配置
//!   都算），说明用户自己配过，什么都不做。
//! - **只认固定代理**：系统代理没开、配了 PAC 自动配置脚本、只有 SOCKS，都不注入。
//!   PAC 和手动代理同时开着时浏览器优先用 PAC，补手动那条会和浏览器走不同出口；
//!   Bun 跑不了 PAC。开了 TUN 的代理工具不需要环境变量也能通。
//! - **入口只有一个**：`login_env::apply_login_environment` 铺完用户环境后调这里，所有走它
//!   起的子进程（Pi、ACP agent、agent 代跑的命令、后台任务、git、安装 CLI）都跟随系统代理，
//!   规则和浏览器一致：绕过列表里的地址直连。不要在调用点单独再调。
//!   用户自己打开的内置终端（smeltd 的 PTY）不走这里，和系统终端 App 保持一致。
//! - **绕过列表一起翻译**，并始终带上回环地址：OAuth 回调服务器、内网地址不能被塞进代理。
//!
//! 每次起进程现读：用户随时会开关代理工具，缓存会读到过期值。读系统代理是一次注册表 /
//! SCDynamicStore 查询，只发生在起进程时，不在 render 路径上。

// 其它平台读不到系统代理，翻译函数只有测试在用。
#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use std::process::Command;

/// 任一个已经有值，就当用户自己配了代理。
const PROXY_VARS: [&str; 6] = [
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
];

const NO_PROXY_VARS: [&str; 2] = ["NO_PROXY", "no_proxy"];

/// 不管系统绕过列表写了什么都要直连的地址。
const LOOPBACK: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// 系统里配的固定代理，已规整成带 scheme 的 URL。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SystemProxy {
    http: Option<String>,
    https: Option<String>,
    /// `NO_PROXY` 格式的条目，不含回环地址。
    bypass: Vec<String>,
}

/// 子进程环境里没有代理变量时，按系统代理补上。要在命令的环境都设完之后调用。
pub(crate) fn apply_system_proxy_fallback(command: &mut Command) {
    if configures_proxy(command, |name| std::env::var_os(name)) {
        return;
    }
    let Some(proxy) = read_system_proxy() else {
        return;
    };
    let keep_no_proxy = has_value(command, &NO_PROXY_VARS, |name| std::env::var_os(name));
    for (name, value) in proxy_env(&proxy, keep_no_proxy) {
        command.env(name, value);
    }
}

fn configures_proxy(
    command: &Command,
    process_env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> bool {
    has_value(command, &PROXY_VARS, process_env)
}

/// 子进程最终会不会拿到 `names` 里某个非空变量：命令上显式设的优先，显式删掉的算没有，
/// 其余继承父进程。Windows 环境变量名不分大小写，其它平台按原样比较。
fn has_value(
    command: &Command,
    names: &[&str],
    process_env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> bool {
    names.iter().any(|name| {
        let explicit = command
            .get_envs()
            .filter(|(key, _)| same_var(&key.to_string_lossy(), name))
            .last();
        match explicit {
            Some((_, value)) => value.is_some_and(|value| !value.is_empty()),
            None => process_env(name).is_some_and(|value| !value.is_empty()),
        }
    })
}

fn same_var(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// 要设的变量。大小写两份都设：curl 只认小写的 `http_proxy`，有的工具只认大写；
/// Windows 上同名不分大小写，后一份覆盖前一份，值相同。
fn proxy_env(proxy: &SystemProxy, keep_no_proxy: bool) -> Vec<(&'static str, String)> {
    let mut env = Vec::new();
    if let Some(http) = &proxy.http {
        env.push(("HTTP_PROXY", http.clone()));
        env.push(("http_proxy", http.clone()));
    }
    if let Some(https) = &proxy.https {
        env.push(("HTTPS_PROXY", https.clone()));
        env.push(("https_proxy", https.clone()));
    }
    if !keep_no_proxy {
        let mut entries: Vec<String> = LOOPBACK.iter().map(|entry| entry.to_string()).collect();
        for entry in &proxy.bypass {
            if !entries
                .iter()
                .any(|known| known.eq_ignore_ascii_case(entry))
            {
                entries.push(entry.clone());
            }
        }
        let no_proxy = entries.join(",");
        env.push(("NO_PROXY", no_proxy.clone()));
        env.push(("no_proxy", no_proxy));
    }
    env
}

/// `host:port` 补成 `http://host:port`；已带 scheme 的原样保留。
/// 系统代理设置里的地址是 HTTP 代理（HTTPS 目标走 CONNECT），所以默认 scheme 是 http。
fn proxy_url(address: &str) -> Option<String> {
    let address = address.trim();
    if address.is_empty() {
        return None;
    }
    if address.contains("://") {
        Some(address.to_string())
    } else {
        Some(format!("http://{address}"))
    }
}

/// 把系统绕过列表的一条翻译成 `NO_PROXY` 条目。
///
/// - `*.corp.com` → `.corp.com`（`NO_PROXY` 的后缀写法）
/// - `10.*`、`192.168.*` → CIDR
/// - Windows 的 `<local>`（不带点的主机名）`NO_PROXY` 表达不了，丢掉
fn bypass_entry(entry: &str) -> Option<String> {
    let entry = entry.trim();
    if entry.is_empty() || entry.eq_ignore_ascii_case("<local>") {
        return None;
    }
    if let Some(cidr) = ipv4_wildcard_to_cidr(entry) {
        return Some(cidr);
    }
    if let Some(suffix) = entry.strip_prefix("*.") {
        return Some(format!(".{suffix}"));
    }
    if entry == "*" {
        return Some("*".to_string());
    }
    // 其余带通配符的写法（`*corp.com`、`192.168.*.1`）NO_PROXY 不认，丢掉免得误配。
    if entry.contains('*') {
        return None;
    }
    Some(entry.to_string())
}

fn ipv4_wildcard_to_cidr(entry: &str) -> Option<String> {
    let parts: Vec<&str> = entry.split('.').collect();
    let wildcard = parts.iter().position(|part| *part == "*")?;
    if parts.len() > 4 || wildcard == 0 || parts[wildcard..].iter().any(|part| *part != "*") {
        return None;
    }
    let mut octets = [0u8; 4];
    for (index, part) in parts[..wildcard].iter().enumerate() {
        octets[index] = part.parse().ok()?;
    }
    Some(format!(
        "{}.{}.{}.{}/{}",
        octets[0],
        octets[1],
        octets[2],
        octets[3],
        wildcard * 8
    ))
}

fn bypass_entries<'a>(entries: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in entries.into_iter().filter_map(bypass_entry) {
        if !out.iter().any(|known| known.eq_ignore_ascii_case(&entry)) {
            out.push(entry);
        }
    }
    out
}

/// 解析 Windows 注册表里的 `ProxyServer`。
///
/// 两种写法：`host:port`（所有协议共用），或 `http=h:p;https=h:p;socks=h:p`（按协议）。
/// 按协议写时 https 只取 `https=`：WinINet 对没列出的协议是直连，这里保持一致。
/// SOCKS Bun 用不了，不取。
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_windows_proxy_server(value: &str) -> (Option<String>, Option<String>) {
    let value = value.trim();
    if !value.contains('=') {
        let url = proxy_url(value);
        return (url.clone(), url);
    }
    let mut http = None;
    let mut https = None;
    for part in value.split(';') {
        let Some((scheme, address)) = part.split_once('=') else {
            continue;
        };
        match scheme.trim().to_ascii_lowercase().as_str() {
            "http" => http = proxy_url(address),
            "https" => https = proxy_url(address),
            _ => {}
        }
    }
    (http, https)
}

fn finish(proxy: SystemProxy) -> Option<SystemProxy> {
    (proxy.http.is_some() || proxy.https.is_some()).then_some(proxy)
}

#[cfg(windows)]
fn read_system_proxy() -> Option<SystemProxy> {
    let settings = windows_registry::CURRENT_USER
        .open("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .ok()?;
    if settings.get_u32("ProxyEnable").unwrap_or(0) == 0 {
        return None;
    }
    // 配了 PAC：浏览器按脚本走，手动代理只是兜底，不跟。
    if settings
        .get_string("AutoConfigURL")
        .is_ok_and(|url| !url.trim().is_empty())
    {
        return None;
    }
    let (http, https) = parse_windows_proxy_server(&settings.get_string("ProxyServer").ok()?);
    let bypass = settings
        .get_string("ProxyOverride")
        .map(|value| bypass_entries(value.split(';')))
        .unwrap_or_default();
    finish(SystemProxy {
        http,
        https,
        bypass,
    })
}

#[cfg(target_os = "macos")]
fn read_system_proxy() -> Option<SystemProxy> {
    use system_configuration::core_foundation::array::CFArray;
    use system_configuration::core_foundation::base::{CFType, CFTypeRef, TCFType as _};
    use system_configuration::core_foundation::dictionary::CFDictionary;
    use system_configuration::core_foundation::number::CFNumber;
    use system_configuration::core_foundation::string::{CFString, CFStringRef};
    use system_configuration::dynamic_store::SCDynamicStoreBuilder;
    use system_configuration::sys::schema_definitions::{
        kSCPropNetProxiesExceptionsList, kSCPropNetProxiesHTTPEnable, kSCPropNetProxiesHTTPPort,
        kSCPropNetProxiesHTTPProxy, kSCPropNetProxiesHTTPSEnable, kSCPropNetProxiesHTTPSPort,
        kSCPropNetProxiesHTTPSProxy, kSCPropNetProxiesProxyAutoConfigEnable,
    };

    fn key(raw: CFStringRef) -> CFString {
        // SAFETY: 系统框架导出的常量字符串，进程存活期内一直有效。
        unsafe { CFString::wrap_under_get_rule(raw) }
    }

    fn setting(
        proxies: &CFDictionary<CFString, CFType>,
        enable: CFStringRef,
        host: CFStringRef,
        port: CFStringRef,
    ) -> Option<String> {
        let enabled = proxies
            .find(key(enable))
            .and_then(|flag| flag.downcast::<CFNumber>())
            .and_then(|flag| flag.to_i32())
            .unwrap_or(0)
            == 1;
        if !enabled {
            return None;
        }
        let host = proxies
            .find(key(host))
            .and_then(|host| host.downcast::<CFString>())
            .map(|host| host.to_string())
            .filter(|host| !host.trim().is_empty())?;
        let port = proxies
            .find(key(port))
            .and_then(|port| port.downcast::<CFNumber>())
            .and_then(|port| port.to_i32());
        match port {
            Some(port) => proxy_url(&format!("{host}:{port}")),
            None => proxy_url(&host),
        }
    }

    let store = SCDynamicStoreBuilder::new("smelt").build()?;
    let proxies = store.get_proxies()?;
    // 配了 PAC：系统按脚本选代理，手动那条不跟。
    let pac_enabled = proxies
        .find(key(unsafe { kSCPropNetProxiesProxyAutoConfigEnable }))
        .and_then(|flag| flag.downcast::<CFNumber>())
        .and_then(|flag| flag.to_i32())
        == Some(1);
    if pac_enabled {
        return None;
    }
    // SAFETY: 同上，读取系统框架导出的常量。
    let (http, https) = unsafe {
        (
            setting(
                &proxies,
                kSCPropNetProxiesHTTPEnable,
                kSCPropNetProxiesHTTPProxy,
                kSCPropNetProxiesHTTPPort,
            ),
            setting(
                &proxies,
                kSCPropNetProxiesHTTPSEnable,
                kSCPropNetProxiesHTTPSProxy,
                kSCPropNetProxiesHTTPSPort,
            ),
        )
    };
    let exceptions: Vec<String> = proxies
        .find(key(unsafe { kSCPropNetProxiesExceptionsList }))
        .and_then(|list| list.downcast::<CFArray>())
        .map(|list| {
            list.iter()
                // SAFETY: 数组元素是有效的 CF 对象，按 get 规则包装只加引用计数。
                .map(|item| unsafe { CFType::wrap_under_get_rule(*item as CFTypeRef) })
                .filter_map(|item| item.downcast::<CFString>())
                .map(|item| item.to_string())
                .collect()
        })
        .unwrap_or_default();
    finish(SystemProxy {
        http,
        https,
        bypass: bypass_entries(exceptions.iter().map(String::as_str)),
    })
}

/// 其它平台没有统一的系统代理设置（Linux 桌面各家不同），只认环境变量。
#[cfg(not(any(windows, target_os = "macos")))]
fn read_system_proxy() -> Option<SystemProxy> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn no_process_env(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn single_windows_proxy_server_serves_both_schemes() {
        assert_eq!(
            parse_windows_proxy_server("proxy.nioint.com:8080"),
            (
                Some("http://proxy.nioint.com:8080".to_string()),
                Some("http://proxy.nioint.com:8080".to_string())
            )
        );
    }

    #[test]
    fn per_protocol_windows_proxy_server_keeps_each_scheme_and_drops_socks() {
        assert_eq!(
            parse_windows_proxy_server("http=a:1;https=b:2;socks=c:3"),
            (
                Some("http://a:1".to_string()),
                Some("http://b:2".to_string())
            )
        );
        // 只列了 http：https 和 WinINet 一样直连，不能挪用 http 那条。
        assert_eq!(
            parse_windows_proxy_server("http=a:1"),
            (Some("http://a:1".to_string()), None)
        );
        assert_eq!(parse_windows_proxy_server("socks=c:3"), (None, None));
    }

    #[test]
    fn proxy_server_with_scheme_is_kept() {
        assert_eq!(
            proxy_url("http://user:pw@proxy:8080").as_deref(),
            Some("http://user:pw@proxy:8080")
        );
        assert_eq!(proxy_url("  "), None);
    }

    #[test]
    fn bypass_list_is_translated_to_no_proxy_entries() {
        let entries = bypass_entries(
            "<local>;*.nioint.com;nevint.com;10.*;192.168.*;192.168.*.1;*corp;; 172.16.0.1"
                .split(';'),
        );
        assert_eq!(
            entries,
            [
                ".nioint.com",
                "nevint.com",
                "10.0.0.0/8",
                "192.168.0.0/16",
                "172.16.0.1"
            ]
        );
    }

    #[test]
    fn no_proxy_always_starts_with_loopback_and_dedupes() {
        let proxy = SystemProxy {
            http: Some("http://p:1".to_string()),
            https: Some("http://p:1".to_string()),
            bypass: vec!["localhost".to_string(), ".corp".to_string()],
        };
        let env = proxy_env(&proxy, false);
        let no_proxy = env
            .iter()
            .find(|(name, _)| *name == "NO_PROXY")
            .map(|(_, value)| value.as_str());
        assert_eq!(no_proxy, Some("localhost,127.0.0.1,::1,.corp"));
        assert!(env.contains(&("https_proxy", "http://p:1".to_string())));
    }

    #[test]
    fn user_no_proxy_is_left_alone() {
        let proxy = SystemProxy {
            https: Some("http://p:1".to_string()),
            ..SystemProxy::default()
        };
        let env = proxy_env(&proxy, true);
        assert!(
            env.iter()
                .all(|(name, _)| !name.eq_ignore_ascii_case("NO_PROXY"))
        );
        assert!(
            env.iter()
                .all(|(name, _)| !name.eq_ignore_ascii_case("HTTP_PROXY"))
        );
    }

    #[test]
    fn explicit_proxy_on_command_counts_as_configured() {
        let mut command = Command::new("x");
        command.env("https_proxy", "http://mine:1");
        assert!(configures_proxy(&command, no_process_env));
    }

    #[test]
    fn inherited_proxy_counts_as_configured() {
        let command = Command::new("x");
        assert!(configures_proxy(&command, |name| {
            (name == "ALL_PROXY").then(|| OsString::from("socks5://mine:1"))
        }));
    }

    #[test]
    fn removed_or_empty_proxy_does_not_count() {
        let mut command = Command::new("x");
        command.env_remove("HTTPS_PROXY").env("HTTP_PROXY", "");
        assert!(!configures_proxy(&command, |name| {
            (name == "HTTPS_PROXY").then(|| OsString::from("http://inherited:1"))
        }));
    }
}
