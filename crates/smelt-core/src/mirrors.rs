//! 国内镜像：中国大陆的机器常连不上 GitHub 和 npm 官方源（不通，或者建连超时）。
//! 连不上时下载改走国内镜像；用户自己配了源或代理的一律不动。系统包的镜像在安装脚本里
//! （`scripts/install-smeltd.sh` 的 `choose_apt_sources`）。
//!
//! 只试 TCP 建连：连不上的网络在这一步就失败。连得上、只是慢的不算，那种让用户自己配源。

use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// 探测一个地址最多花这么久。解析出的地址逐个试，合计不超过它。
const PROBE_BUDGET: Duration = Duration::from_secs(5);

const NPM_REGISTRY_HOST: &str = "registry.npmjs.org";
const NPM_MIRROR: &str = "https://registry.npmmirror.com/";

/// 这台机器能不能和 `host:port` 建立 TCP 连接。IPv6 不通时 IPv4 可能通，所以解析出的
/// 地址都试，直到预算用完。
pub(crate) fn reachable(host: &str, port: u16) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    let deadline = Instant::now() + PROBE_BUDGET;
    for addr in addrs {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        if TcpStream::connect_timeout(&addr, left).is_ok() {
            return true;
        }
    }
    false
}

/// 给拉 npm 包的子进程（MCP 预设的 `bun x`、agent 跑的 npm）补的源：
/// 官方源连不上、用户又没自己配源或代理时是镜像地址，否则 `None`。进程存活期只探一次。
/// 只在系统设置里配了代理也算配过：子进程会被补上 `HTTPS_PROXY`（见 `system_proxy`），
/// 走代理连官方源，不该再被改到镜像。
/// Bun 和 npm 都认 `NPM_CONFIG_REGISTRY`。
pub(crate) fn npm_registry_fallback() -> Option<&'static str> {
    static FALLBACK: OnceLock<Option<&'static str>> = OnceLock::new();
    *FALLBACK.get_or_init(|| {
        let login = crate::login_env::login_environment();
        let lookup = |name: &str| {
            std::env::var(name)
                .ok()
                .or_else(|| login.get(name).cloned())
                .filter(|value| !value.is_empty())
        };
        let home = dirs::home_dir();
        if user_configured_network(lookup, home.as_deref())
            || crate::system_proxy::has_system_https_proxy()
        {
            return None;
        }
        (!reachable(NPM_REGISTRY_HOST, 443)).then_some(NPM_MIRROR)
    })
}

/// 用户自己配过源或代理：环境变量（小写、大写两种写法都认），或者 `~/.npmrc`、
/// `~/.bunfig.toml`、`$XDG_CONFIG_HOME/.bunfig.toml` 里写了 registry。设了
/// `NPM_CONFIG_REGISTRY` 会盖过这些文件，所以配过就不能设。
fn user_configured_network(lookup: impl Fn(&str) -> Option<String>, home: Option<&Path>) -> bool {
    const VARS: [&str; 4] = [
        "npm_config_registry",
        "bun_config_registry",
        "https_proxy",
        "all_proxy",
    ];
    if VARS.iter().any(|var| {
        lookup(var)
            .or_else(|| lookup(&var.to_ascii_uppercase()))
            .is_some()
    }) {
        return true;
    }
    let xdg = lookup("XDG_CONFIG_HOME");
    let files = [
        home.map(|home| home.join(".npmrc")),
        home.map(|home| home.join(".bunfig.toml")),
        xdg.map(|dir| Path::new(&dir).join(".bunfig.toml")),
    ];
    files.into_iter().flatten().any(|file| {
        std::fs::read_to_string(file).is_ok_and(|text| {
            text.lines()
                .any(|line| line.trim_start().starts_with("registry"))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!("smelt-mirrors-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn configured(vars: &[(&str, &str)], home: &Path) -> bool {
        let lookup = |name: &str| {
            vars.iter()
                .find(|(var, _)| *var == name)
                .map(|(_, value)| value.to_string())
                .filter(|value| !value.is_empty())
        };
        user_configured_network(lookup, Some(home))
    }

    #[test]
    fn registry_or_proxy_in_the_environment_counts_as_configured() {
        let home = temp_home();
        assert!(configured(
            &[("npm_config_registry", "https://npm.example.com/")],
            &home
        ));
        assert!(configured(&[("BUN_CONFIG_REGISTRY", "https://x/")], &home));
        assert!(configured(&[("HTTPS_PROXY", "http://proxy:8080")], &home));
        assert!(
            !configured(&[("HTTPS_PROXY", "")], &home),
            "设成空串等于没设"
        );
        std::fs::remove_dir_all(home).ok();
    }

    /// 复现：用户在 `~/.npmrc` 里配了公司源，我们再设 `NPM_CONFIG_REGISTRY` 会把它盖掉。
    #[test]
    fn registry_in_npmrc_or_bunfig_counts_as_configured() {
        let home = temp_home();
        std::fs::write(home.join(".npmrc"), "@corp:registry=https://corp/\n").unwrap();
        assert!(
            !configured(&[], &home),
            "只给某个 scope 配的源不算，其他包还走官方源"
        );
        std::fs::write(home.join(".npmrc"), "registry=https://corp/\n").unwrap();
        assert!(configured(&[], &home));
        std::fs::remove_file(home.join(".npmrc")).unwrap();

        let xdg = home.join("config");
        std::fs::create_dir_all(&xdg).unwrap();
        std::fs::write(
            xdg.join(".bunfig.toml"),
            "[install]\nregistry = \"https://corp/\"\n",
        )
        .unwrap();
        assert!(configured(
            &[("XDG_CONFIG_HOME", xdg.to_str().unwrap())],
            &home
        ));
        std::fs::remove_dir_all(home).ok();
    }

    // 只用本机回环：不碰 DNS 和外网。fake-ip 代理（Clash / Surge 的 TUN 模式）会把
    // 任何域名都解析成 198.18.x.x 并接下连接，拿 `.invalid` 测「连不上」在那种机器上必挂。

    #[test]
    fn a_closed_port_is_unreachable() {
        // 占一个端口再放掉，此刻没有人在听，建连立即被拒。
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(!reachable("127.0.0.1", port));
    }

    #[test]
    fn a_listening_port_is_reachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(reachable("127.0.0.1", port));
    }
}
