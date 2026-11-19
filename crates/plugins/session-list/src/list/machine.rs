//! 机器行上的系统图标和连接指示：一盏灯（图标的颜色），悬停名字看完整说明。从旧宿主
//! `runtimes.rs` 的 `runtime_indicator` 一族搬来，改成只读插件接口给的连接表。

use gpui::SharedString;
use rust_i18n::t;
use smelt_plugin_api::ui::{IconName, ui_theme};
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

impl Light {
    /// 灯的颜色：系统图标就画成这个色。
    pub(super) fn color(self) -> u32 {
        match self {
            Light::Good => ui_theme::ok(),
            Light::Degraded => ui_theme::need(),
            Light::Down => ui_theme::bad(),
        }
    }
}

/// 一台机器眼下连得怎样。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MachineLink {
    pub(super) light: Light,
    /// 悬停机器名时的完整说明。
    pub(super) detail: String,
    /// 用不了时会话行尾写的短字；能用是 `None`。
    pub(super) unavailable: Option<SharedString>,
}

/// 按机器表和连接表算一台机器的连接指示。本机不在连接表里，只看 `Machine::unavailable`。
pub(super) fn link(machine: &Machine, connection: Option<&Connection>) -> MachineLink {
    match connection {
        Some(connection) => remote_link(&connection.state, connection.link.as_ref()),
        None => match &machine.unavailable {
            Some(reason) => MachineLink {
                light: Light::Down,
                detail: t!("machine.offline_detail", reason = reason).into_owned(),
                unavailable: Some(t!("machine.offline").into()),
            },
            None => MachineLink {
                light: Light::Good,
                detail: t!("machine.online").into_owned(),
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
        ConnectionState::Incompatible { .. } => Some(t!("machine.incompatible").into()),
        ConnectionState::Upgrading { .. } => Some(t!("machine.upgrading").into()),
        ConnectionState::Connecting | ConnectionState::Offline { .. } => {
            Some(t!("machine.offline").into())
        }
    };
    let detail = match state {
        ConnectionState::Connecting => t!("machine.connecting").into_owned(),
        ConnectionState::Offline { error } => {
            t!("machine.offline_detail", reason = error).into_owned()
        }
        ConnectionState::Upgrading { progress } => progress.clone(),
        ConnectionState::Incompatible { version, detail } => {
            // 连得上，连接路径照样有：也说走哪条路、多慢；悬停还要看得到为什么、该升级哪边。
            let mut text = t!("machine.incompatible_detail", version = version).into_owned();
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
            None => t!("machine.online").into_owned(),
            Some(
                link @ ConnectionLink {
                    route: LinkRoute::Other,
                    ..
                },
            ) => t!("machine.online_rtt", rtt = link.rtt_ms).into_owned(),
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
        LinkRoute::Direct => t!("machine.direct", rtt = link.rtt_ms).into_owned(),
        LinkRoute::Relay { relay } => {
            t!("machine.relay", relay = relay, rtt = link.rtt_ms).into_owned()
        }
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
        "macos" | "darwin" => IconName::Apple.asset_path(),
        "linux" => IconName::Linux.asset_path(),
        "windows" => IconName::Windows.asset_path(),
        _ => IconName::Laptop.asset_path(),
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
        assert_eq!(direct.detail, "Direct · 12ms");
        assert_eq!(direct.unavailable, None);

        let relay = LinkRoute::Relay { relay: "hk".into() };
        let slow = remote_link(&online, Some(&link_of(relay, 480, true)));
        assert_eq!(slow.light, Light::Degraded);
        assert_eq!(slow.detail, "Relay hk · 480ms");

        let other = remote_link(&online, Some(&link_of(LinkRoute::Other, 30, false)));
        assert_eq!(other.detail, "Online · 30ms");
        assert_eq!(remote_link(&online, None).detail, "Online");
    }

    #[test]
    fn unusable_machines_are_down_with_a_short_label() {
        let offline = remote_link(
            &ConnectionState::Offline {
                error: "timed out".into(),
            },
            None,
        );
        assert_eq!(offline.light, Light::Down);
        assert_eq!(offline.detail, "Offline: timed out");
        assert_eq!(offline.unavailable.as_deref(), Some("Offline"));

        let incompatible = remote_link(
            &ConnectionState::Incompatible {
                version: "0.9".into(),
                detail: "upgrade the remote runtime".into(),
            },
            Some(&link_of(LinkRoute::Direct, 8, false)),
        );
        assert_eq!(incompatible.light, Light::Down);
        assert_eq!(
            incompatible.detail,
            "Incompatible version · 0.9 · Direct · 8ms\nupgrade the remote runtime"
        );
        assert_eq!(
            incompatible.unavailable.as_deref(),
            Some("Incompatible Version")
        );

        let upgrading = remote_link(
            &ConnectionState::Upgrading {
                progress: "downloading".into(),
            },
            None,
        );
        assert_eq!(upgrading.light, Light::Degraded);
        assert_eq!(upgrading.unavailable.as_deref(), Some("Upgrading"));
    }

    #[test]
    fn os_icons() {
        assert_eq!(icon_for_os(Some("Darwin")), IconName::Apple.asset_path());
        assert_eq!(icon_for_os(Some("macos")), IconName::Apple.asset_path());
        assert_eq!(icon_for_os(Some("linux")), IconName::Linux.asset_path());
        assert_eq!(icon_for_os(Some("windows")), IconName::Windows.asset_path());
        assert_eq!(icon_for_os(None), IconName::Laptop.asset_path());
    }
}
