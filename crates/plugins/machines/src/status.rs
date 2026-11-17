//! 状态写成字、要不要提醒：只看数据，不碰界面，好测。

use api::model::remote_config::{DeviceInfo, RemoteAccessState};
use api::{Connection, ConnectionLink, ConnectionState, Connections, LinkRoute};
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
        ConnectionState::Connecting => ("正在连接…".to_string(), Tone::Muted),
        ConnectionState::Online { version, platform } => {
            let mut text = match platform {
                Some(platform) => format!("在线 · {version} · {platform}"),
                None => format!("在线 · {version}"),
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
        ConnectionState::Offline { error } => (format!("离线：{error}"), Tone::Muted),
        ConnectionState::Incompatible { detail, .. } => {
            (format!("版本不兼容：{detail}"), Tone::Muted)
        }
        ConnectionState::Upgrading { progress } => (format!("升级中：{progress}"), Tone::Muted),
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
        RemoteAccessState::Off => ("已关闭".to_string(), Tone::Muted),
        RemoteAccessState::Starting => ("正在开启…".to_string(), Tone::Muted),
        RemoteAccessState::Running { endpoint_id, relay } => (
            format!(
                "已开启 · {} · {}",
                relay.as_deref().unwrap_or("公共 relay"),
                short_id(endpoint_id)
            ),
            Tone::Ok,
        ),
        RemoteAccessState::Failed { error } => (format!("开不起来，正在重试：{error}"), Tone::Bad),
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
        return "在线".to_string();
    }
    let Some(at) = device.last_seen_at_ms else {
        return "还没连过".to_string();
    };
    let secs = (now_ms - at).max(0) / 1000;
    if secs < 3600 {
        format!("{} 分钟前连过", (secs / 60).max(1))
    } else if secs < 86_400 {
        format!("{} 小时前连过", secs / 3600)
    } else {
        format!("{} 天前连过", secs / 86_400)
    }
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
            ("在线 · 2026.1116.0 · linux/x86_64".into(), Tone::Muted)
        );
        assert_eq!(
            machine_status(&machine(online(), relay(437, true))),
            (
                "在线 · 2026.1116.0 · linux/x86_64 · 中继 n0 usw1-1 · 437ms".into(),
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
            "在线 · 2026.1116.0 · linux/x86_64 · 直连 · 12ms"
        );
        assert_eq!(
            machine_status(&machine(
                ConnectionState::Offline {
                    error: "connection refused".into()
                },
                None
            ))
            .0,
            "离线：connection refused"
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
        assert_eq!(last_seen_text(&device(true, None), now), "在线");
        assert_eq!(last_seen_text(&device(false, None), now), "还没连过");
        assert_eq!(
            last_seen_text(&device(false, Some(now - 5_000)), now),
            "1 分钟前连过"
        );
        assert_eq!(
            last_seen_text(&device(false, Some(now - 2 * 3_600_000)), now),
            "2 小时前连过"
        );
        assert_eq!(
            last_seen_text(&device(false, Some(now - 3 * 86_400_000)), now),
            "3 天前连过"
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
