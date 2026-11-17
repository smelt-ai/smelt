//! 机器行上的系统图标和连接指示：一盏灯（图标的颜色），悬停名字看完整说明。从旧宿主
//! `runtimes.rs` 的 `runtime_indicator` 一族搬来，改成只读插件接口给的连接表。

use smelt_plugin_api::{
    Connection, ConnectionLink, ConnectionState, Connections, LinkRoute, Machine, RuntimeKey,
};

/// 连接指示灯。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Light {
    /// 在线，延迟正常。
    Good,
    /// 能用但要留意：连接中、升级中、延迟高。
    Degraded,
    /// 用不了：离线、版本不兼容。
    Down,
}

/// 一台机器眼下连得怎样。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MachineLink {
    pub(super) light: Light,
    /// 悬停机器名时的完整说明。
    pub(super) detail: String,
    /// 用不了时会话行尾写的短字；能用是 `None`。
    pub(super) unavailable: Option<&'static str>,
}

/// 按机器表和连接表算一台机器的连接指示。本机不在连接表里，只看 `Machine::unavailable`。
pub(super) fn link(machine: &Machine, connection: Option<&Connection>) -> MachineLink {
    match connection {
        Some(connection) => remote_link(&connection.state, connection.link.as_ref()),
        None => match &machine.unavailable {
            Some(reason) => MachineLink {
                light: Light::Down,
                detail: format!("离线：{reason}"),
                unavailable: Some("离线"),
            },
            None => MachineLink {
                light: Light::Good,
                detail: "在线".to_string(),
                unavailable: None,
            },
        },
    }
}

fn remote_link(state: &ConnectionState, link: Option<&ConnectionLink>) -> MachineLink {
    let light = match state {
        ConnectionState::Online { .. } => match link {
            Some(link) if link.slow => Light::Degraded,
            _ => Light::Good,
        },
        ConnectionState::Connecting | ConnectionState::Upgrading { .. } => Light::Degraded,
        ConnectionState::Offline { .. } | ConnectionState::Incompatible { .. } => Light::Down,
    };
    let unavailable = match state {
        ConnectionState::Online { .. } => None,
        ConnectionState::Incompatible { .. } => Some("版本不兼容"),
        ConnectionState::Upgrading { .. } => Some("升级中"),
        ConnectionState::Connecting | ConnectionState::Offline { .. } => Some("离线"),
    };
    let detail = match state {
        ConnectionState::Connecting => "连接中".to_string(),
        ConnectionState::Offline { error } => format!("离线：{error}"),
        ConnectionState::Upgrading { progress } => progress.clone(),
        ConnectionState::Incompatible { version, detail } => {
            // 连得上，连接路径照样有：也说走哪条路、多慢；悬停还要看得到为什么、该升级哪边。
            let mut text = format!("版本不兼容 · {version}");
            if let Some(link) = link {
                text.push_str(&format!(" · {}", link_summary(link)));
            }
            if !detail.is_empty() {
                text.push('\n');
                text.push_str(detail);
            }
            text
        }
        ConnectionState::Online { .. } => match link {
            None => "在线".to_string(),
            Some(
                link @ ConnectionLink {
                    route: LinkRoute::Other,
                    ..
                },
            ) => format!("在线 · {}ms", link.rtt_ms),
            Some(link) => link_summary(link),
        },
    };
    MachineLink {
        light,
        detail,
        unavailable,
    }
}

/// 走哪条路、多慢。
fn link_summary(link: &ConnectionLink) -> String {
    match &link.route {
        LinkRoute::Direct => format!("直连 · {}ms", link.rtt_ms),
        LinkRoute::Relay { relay } => format!("中继 {relay} · {}ms", link.rtt_ms),
        LinkRoute::Other => format!("{}ms", link.rtt_ms),
    }
}

/// 机器行的系统图标。本机按编译目标；远程按宿主记着的系统，离线也认得；从没连上过的是通用
/// 的笔记本。
pub(super) fn os_icon(machine: &RuntimeKey, connections: &Connections) -> &'static str {
    let os = match machine {
        RuntimeKey::Local => Some(std::env::consts::OS),
        RuntimeKey::Remote(_) => connections
            .remotes
            .iter()
            .find(|connection| connection.machine == *machine)
            .and_then(|connection| connection.os.as_deref()),
    };
    icon_for_os(os)
}

fn icon_for_os(os: Option<&str>) -> &'static str {
    match os.unwrap_or_default().trim().to_ascii_lowercase().as_str() {
        "macos" | "darwin" => "icons/apple.svg",
        "linux" => "smelt-icons/machine-linux.svg",
        "windows" => "smelt-icons/machine-windows.svg",
        _ => "icons/laptop-minimal.svg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link_of(route: LinkRoute, rtt_ms: u32, slow: bool) -> ConnectionLink {
        ConnectionLink {
            route,
            rtt_ms,
            slow,
        }
    }

    #[test]
    fn online_links_say_route_and_latency() {
        let online = ConnectionState::Online {
            version: "1".into(),
            platform: None,
        };
        let direct = remote_link(&online, Some(&link_of(LinkRoute::Direct, 12, false)));
        assert_eq!(direct.light, Light::Good);
        assert_eq!(direct.detail, "直连 · 12ms");
        assert_eq!(direct.unavailable, None);

        let relay = LinkRoute::Relay { relay: "hk".into() };
        let slow = remote_link(&online, Some(&link_of(relay, 480, true)));
        assert_eq!(slow.light, Light::Degraded);
        assert_eq!(slow.detail, "中继 hk · 480ms");

        let other = remote_link(&online, Some(&link_of(LinkRoute::Other, 30, false)));
        assert_eq!(other.detail, "在线 · 30ms");
        assert_eq!(remote_link(&online, None).detail, "在线");
    }

    #[test]
    fn unusable_machines_are_down_with_a_short_label() {
        let offline = remote_link(
            &ConnectionState::Offline {
                error: "超时".into(),
            },
            None,
        );
        assert_eq!(offline.light, Light::Down);
        assert_eq!(offline.detail, "离线：超时");
        assert_eq!(offline.unavailable, Some("离线"));

        let incompatible = remote_link(
            &ConnectionState::Incompatible {
                version: "0.9".into(),
                detail: "请升级远程运行时".into(),
            },
            Some(&link_of(LinkRoute::Direct, 8, false)),
        );
        assert_eq!(incompatible.light, Light::Down);
        assert_eq!(
            incompatible.detail,
            "版本不兼容 · 0.9 · 直连 · 8ms\n请升级远程运行时"
        );
        assert_eq!(incompatible.unavailable, Some("版本不兼容"));

        let upgrading = remote_link(
            &ConnectionState::Upgrading {
                progress: "正在下载".into(),
            },
            None,
        );
        assert_eq!(upgrading.light, Light::Degraded);
        assert_eq!(upgrading.unavailable, Some("升级中"));
    }

    #[test]
    fn os_icons() {
        assert_eq!(icon_for_os(Some("Darwin")), "icons/apple.svg");
        assert_eq!(icon_for_os(Some("macos")), "icons/apple.svg");
        assert_eq!(icon_for_os(Some("linux")), "smelt-icons/machine-linux.svg");
        assert_eq!(
            icon_for_os(Some("windows")),
            "smelt-icons/machine-windows.svg"
        );
        assert_eq!(icon_for_os(None), "icons/laptop-minimal.svg");
    }
}
