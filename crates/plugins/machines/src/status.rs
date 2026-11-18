//! 状态写成字、要不要提醒：只看数据，不碰界面，好测。

use api::model::remote_config::{DeviceInfo, RemoteAccessState};
use api::{Connection, ConnectionLink, ConnectionState, Connections, LinkRoute};
use rust_i18n::t;
use smelt_plugin_api as api;

/// 字的轻重。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Muted,
    Ok,
    Warning,
    Bad,
}

/// 设置页机器一行下面那行字：在线写版本、平台、走哪条路多慢（慢了标黄），别的写为什么。
pub(crate) fn machine_status(connection: &Connection) -> (String, Tone) {
    match &connection.state {
        ConnectionState::Connecting => (t!("machine.connecting").into_owned(), Tone::Muted),
        ConnectionState::Online { version, platform } => {
            let mut text = match platform {
                Some(platform) => format!("{} · {version} · {platform}", t!("online")),
                None => format!("{} · {version}", t!("online")),
            };
            let mut tone = Tone::Muted;
            if let Some(link) = &connection.link {
                text = format!("{text} · {}", link_summary(link));
                if link.slow {
                    tone = Tone::Warning;
                }
            }
            (text, tone)
        }
        ConnectionState::Offline { error } => (
            t!("machine.offline", error = error).into_owned(),
            Tone::Muted,
        ),
        ConnectionState::Incompatible { detail, .. } => (
            t!("machine.incompatible", detail = detail).into_owned(),
            Tone::Muted,
        ),
        ConnectionState::Upgrading { progress } => (
            t!("machine.upgrading", progress = progress).into_owned(),
            Tone::Muted,
        ),
    }
}

/// 走哪条路、多慢。
fn link_summary(link: &ConnectionLink) -> String {
    match &link.route {
        LinkRoute::Direct => t!("link.direct", ms = link.rtt_ms).into_owned(),
        LinkRoute::Relay { relay } => {
            t!("link.relay", relay = relay, ms = link.rtt_ms).into_owned()
        }
        LinkRoute::Other => format!("{}ms", link.rtt_ms),
    }
}

/// 有要你处理的：机器表读不出来，或者哪台离线、版本不对、在线却慢。连接中、升级中是过程，
/// 不算。设置里「远程」页的角标看它。
pub(crate) fn needs_attention(connections: &Connections) -> bool {
    connections.load_error.is_some()
        || connections
            .remotes
            .iter()
            .any(|connection| match &connection.state {
                ConnectionState::Offline { .. } | ConnectionState::Incompatible { .. } => true,
                ConnectionState::Online { .. } => {
                    connection.link.as_ref().is_some_and(|link| link.slow)
                }
                ConnectionState::Connecting | ConnectionState::Upgrading { .. } => false,
            })
}

/// 远程访问开关旁边那行字。
pub(crate) fn access_status(state: &RemoteAccessState) -> (String, Tone) {
    match state {
        RemoteAccessState::Off => (t!("access.off").into_owned(), Tone::Muted),
        RemoteAccessState::Starting => (t!("access.starting").into_owned(), Tone::Muted),
        RemoteAccessState::Running { endpoint_id, relay } => (
            t!(
                "access.running",
                relay = relay
                    .clone()
                    .unwrap_or_else(|| t!("access.public_relay").into_owned()),
                id = short_id(endpoint_id)
            )
            .into_owned(),
            Tone::Ok,
        ),
        RemoteAccessState::Failed { error } => {
            (t!("access.failed", error = error).into_owned(), Tone::Bad)
        }
    }
}

/// 长 id 留头尾各 8 个字。
pub(crate) fn short_id(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    if chars.len() <= 16 {
        return id.to_string();
    }
    let head: String = chars[..8].iter().collect();
    let tail: String = chars[chars.len() - 8..].iter().collect();
    format!("{head}…{tail}")
}

/// 设备上次什么时候连过。`now_ms` 是现在的毫秒时间戳。
pub(crate) fn last_seen_text(device: &DeviceInfo, now_ms: i64) -> String {
    if device.connected {
        return t!("online").into_owned();
    }
    let Some(at) = device.last_seen_at_ms else {
        return t!("seen.never").into_owned();
    };
    let secs = (now_ms - at).max(0) / 1000;
    let text = if secs < 3600 {
        match (secs / 60).max(1) {
            1 => t!("seen.minutes.one"),
            count => t!("seen.minutes.other", count = count),
        }
    } else if secs < 86_400 {
        match secs / 3600 {
            1 => t!("seen.hours.one"),
            count => t!("seen.hours.other", count = count),
        }
    } else {
        match secs / 86_400 {
            1 => t!("seen.days.one"),
            count => t!("seen.days.other", count = count),
        }
    };
    text.into_owned()
}

/// 配对串编成二维码 PNG：白底黑块，至少 160 见方。编不了（内容太长）是 `None`。
pub(crate) fn qr_png(text: &str) -> Option<Vec<u8>> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let luma = code
        .render::<image::Luma<u8>>()
        .dark_color(image::Luma([0u8]))
        .light_color(image::Luma([255u8]))
        .min_dimensions(160, 160)
        .quiet_zone(true)
        .build();
    let rgb = image::DynamicImage::ImageLuma8(luma).into_rgb8();
    let mut png = Vec::new();
    rgb.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::RuntimeKey;

    fn machine(state: ConnectionState, link: Option<ConnectionLink>) -> Connection {
        Connection {
            machine: RuntimeKey::Remote("far".into()),
            name: "far".into(),
            relay: None,
            os: None,
            state,
            link,
            upgrade_note: None,
            can_upgrade: false,
            can_reconnect: false,
        }
    }

    fn online() -> ConnectionState {
        ConnectionState::Online {
            version: "2026.1116.0".into(),
            platform: Some("linux/x86_64".into()),
        }
    }

    fn relay(rtt_ms: u32, slow: bool) -> Option<ConnectionLink> {
        Some(ConnectionLink {
            route: LinkRoute::Relay {
                relay: "n0 usw1-1".into(),
            },
            rtt_ms,
            slow,
        })
    }

    /// 在线写全：版本、平台、走哪条路多慢；慢了标黄。
    #[test]
    fn online_machines_spell_out_the_link() {
        assert_eq!(
            machine_status(&machine(online(), None)),
            ("Online · 2026.1116.0 · linux/x86_64".into(), Tone::Muted)
        );
        assert_eq!(
            machine_status(&machine(online(), relay(437, true))),
            (
                "Online · 2026.1116.0 · linux/x86_64 · relay n0 usw1-1 · 437ms".into(),
                Tone::Warning
            )
        );
        let direct = Some(ConnectionLink {
            route: LinkRoute::Direct,
            rtt_ms: 12,
            slow: false,
        });
        assert_eq!(
            machine_status(&machine(online(), direct)).0,
            "Online · 2026.1116.0 · linux/x86_64 · direct · 12ms"
        );
        assert_eq!(
            machine_status(&machine(
                ConnectionState::Offline {
                    error: "connection refused".into()
                },
                None
            ))
            .0,
            "Offline: connection refused"
        );
    }

    /// 离线、版本不对、在线却慢要提醒；连接中、升级中、在线不慢不提醒；表读不出来也提醒。
    #[test]
    fn attention_follows_what_needs_a_hand() {
        let of = |remotes| Connections {
            remotes,
            load_error: None,
        };
        assert!(!needs_attention(&of(vec![])));
        assert!(!needs_attention(&of(vec![machine(
            online(),
            relay(60, false)
        )])));
        assert!(!needs_attention(&of(vec![machine(
            ConnectionState::Connecting,
            None
        )])));
        assert!(!needs_attention(&of(vec![machine(
            ConnectionState::Upgrading {
                progress: "正在升级".into()
            },
            None
        )])));
        assert!(needs_attention(&of(vec![machine(
            online(),
            relay(628, true)
        )])));
        assert!(needs_attention(&of(vec![machine(
            ConnectionState::Offline {
                error: "断了".into()
            },
            None
        )])));
        assert!(needs_attention(&of(vec![machine(
            ConnectionState::Incompatible {
                version: "0.9.0".into(),
                detail: String::new()
            },
            None
        )])));
        assert!(needs_attention(&Connections {
            remotes: Vec::new(),
            load_error: Some("客户端库比这个程序新".into()),
        }));
    }

    #[test]
    fn long_ids_keep_head_and_tail() {
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(&"a".repeat(16)), "a".repeat(16));
        assert_eq!(
            short_id("0123456789abcdef0123456789abcdef"),
            "01234567…89abcdef"
        );
    }

    #[test]
    fn last_seen_rounds_to_the_largest_unit() {
        let device = |connected, last_seen_at_ms| DeviceInfo {
            endpoint_id: "e".into(),
            name: "手机".into(),
            paired_at_ms: 0,
            last_seen_at_ms,
            connected,
        };
        let now = 10 * 86_400_000;
        assert_eq!(last_seen_text(&device(true, None), now), "Online");
        assert_eq!(last_seen_text(&device(false, None), now), "Never connected");
        assert_eq!(
            last_seen_text(&device(false, Some(now - 5_000)), now),
            "Last seen 1 minute ago"
        );
        assert_eq!(
            last_seen_text(&device(false, Some(now - 2 * 3_600_000)), now),
            "Last seen 2 hours ago"
        );
        assert_eq!(
            last_seen_text(&device(false, Some(now - 3 * 86_400_000)), now),
            "Last seen 3 days ago"
        );
    }

    /// endpoint id 有 64 个十六进制字符，钉住「配对串确实能编成二维码」：超长内容会让
    /// `QrCode::new` 失败。
    #[test]
    fn pairing_uri_renders_to_a_qr() {
        let uri = format!(
            "smelt+iroh://{}/?code=ABCD-EFGH&relay=https%3A%2F%2Frelay.example.test",
            "a".repeat(64)
        );
        let png = qr_png(&uri).expect("配对串必须能生成二维码");
        assert!(!png.is_empty());
    }
}
