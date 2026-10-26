//! 真实传输：经 iroh `smelt/runtime/1` 连运行时。
//!
//! 整个进程只有一个 [`IrohClient`]（同一把设备密钥同时绑几个 endpoint 会让 relay 上出现
//! 重复节点），所有机器共用。它的形态由**全部登记机器**的 relay 设置决定，见 [`shape_for`]：
//! 都用同一个自建 relay 时只用它、域名交给系统解析器（iOS 开着系统代理也能连）；其余情况
//! 用 n0 预设，指定了 relay 的机器在拨号地址里带上它的 relay（桌面多运行时就是这样）。
//! 形态变了就丢掉旧的，下次拨号时按新形态重建。

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use iroh::SecretKey;
use smelt_iroh::runtime::IrohClient;
use smelt_iroh::{EndpointId, PathKind, PathStatus, RelaySettings};
use smelt_runtime_client::{Connector as _, RuntimeClient};

use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::api::fleet::{LinkMetrics, LinkPath};
use crate::connection::{Dialer, Link, LinkChange, OnLinkChange, Route, Target};
use crate::fleet::Transport;

/// endpoint 的形态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    /// n0 预设：公共 relay 与地址发现。
    Preset,
    /// Minimal + 这个 relay + 系统 DNS。
    Relay(RelaySettings),
}

/// 按全部登记机器的 relay 决定形态：都指定了同一个 relay 时只用它，其余（没有机器、都没
/// 指定、混用）用 n0 预设。
///
/// 混用时自建 relay 的域名走 iroh 自带的解析器，iOS 开着系统代理时可能连不上那台机器的
/// relay：这是混用的已知限制，换来的是一个 endpoint 能同时连所有机器。
pub(crate) fn shape_for(relays: &[Option<RelaySettings>]) -> Shape {
    match relays.first() {
        Some(Some(first)) if relays.iter().all(|relay| relay.as_ref() == Some(first)) => {
            Shape::Relay(first.clone())
        }
        _ => Shape::Preset,
    }
}

pub(crate) struct IrohDialer {
    secret: SecretKey,
    state: Mutex<DialerState>,
}

struct DialerState {
    shape: Shape,
    /// 按 `shape` 建的客户端。形态变了就丢掉，下次拨号时重建。
    client: Option<Arc<IrohClient>>,
}

impl IrohDialer {
    pub(crate) fn new(secret: SecretKey) -> Self {
        Self {
            secret,
            state: Mutex::new(DialerState {
                shape: Shape::Preset,
                client: None,
            }),
        }
    }

    /// 按当前形态拿 IrohClient，没有就建。绑定会起 tokio 运行时并 `block_on`，只能在普通
    /// 线程上调。
    fn client(&self) -> Result<Arc<IrohClient>, SmeltError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(client) = state.client.as_ref() {
            return Ok(Arc::clone(client));
        }
        let relay = match &state.shape {
            Shape::Preset => None,
            Shape::Relay(relay) => Some(relay),
        };
        let client = IrohClient::new(self.secret.clone(), relay)
            .map_err(|e| error(SmeltErrorKind::Other, format!("起不了 iroh：{e:#}")))?;
        state.client = Some(Arc::clone(&client));
        Ok(client)
    }
}

impl Dialer for IrohDialer {
    fn route(&self, target: &Target) -> Result<Arc<dyn Route>, SmeltError> {
        let (endpoint, relay) = validate(&target.endpoint_id, target.relay_url.as_deref())?;
        let client = self.client()?;
        Ok(Arc::new(IrohRoute {
            client,
            endpoint,
            relay,
        }))
    }
}

impl Transport for IrohDialer {
    fn configure(&self, relays: &[Option<String>]) -> Result<bool, SmeltError> {
        let relays = relays
            .iter()
            .map(|relay| {
                relay
                    .as_deref()
                    .map(RelaySettings::parse)
                    .transpose()
                    .map_err(|e| error(SmeltErrorKind::InvalidPairing, format!("{e:#}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let shape = shape_for(&relays);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.shape == shape {
            return Ok(false);
        }
        log::info!("iroh endpoint 换形态：{:?} → {shape:?}", state.shape);
        state.shape = shape;
        // 先放掉自己持有的旧 endpoint。各节点的旧会话若还拿着它，会在重新 connect 换代、
        // 旧会话退出时释放。
        state.client = None;
        Ok(true)
    }
}

struct IrohRoute {
    client: Arc<IrohClient>,
    endpoint: EndpointId,
    relay: Option<RelaySettings>,
}

impl Route for IrohRoute {
    fn link(&self) -> Link {
        // 每条链路一个新连接器：重连时不复用可能已经僵死的 QUIC 连接。拨号地址带上这台
        // 机器自己的 relay：endpoint 是 n0 预设时靠它找到用自建 relay 的机器。
        let connector = Arc::new(self.client.connector(self.endpoint, self.relay.as_ref()));
        let for_client = Arc::clone(&connector);
        let for_watch = Arc::clone(&connector);
        Link {
            client: RuntimeClient::new(move || for_client.connect()),
            metrics: Arc::new(move || link_metrics(connector.path_status())),
            watch: Arc::new(move |on_change: OnLinkChange| -> Box<dyn Send> {
                Box::new(for_watch.watch(move |change| {
                    on_change(match change {
                        smelt_iroh::runtime::LinkChange::PathSelected => LinkChange::Path,
                        smelt_iroh::runtime::LinkChange::Closed => LinkChange::Closed,
                    })
                }))
            }),
        }
    }
}

/// 校验 endpoint id 与 relay 地址。
pub(crate) fn validate(
    endpoint_id: &str,
    relay_url: Option<&str>,
) -> Result<(EndpointId, Option<RelaySettings>), SmeltError> {
    let endpoint = endpoint_id.trim().parse::<EndpointId>().map_err(|_| {
        error(
            SmeltErrorKind::InvalidPairing,
            format!("运行时的 endpoint id 不合法：{endpoint_id}"),
        )
    })?;
    let relay = relay_url
        .map(RelaySettings::parse)
        .transpose()
        .map_err(|e| error(SmeltErrorKind::InvalidPairing, format!("{e:#}")))?;
    Ok((endpoint, relay))
}

/// 把 iroh 选中的路径归成界面上的几类。
pub(crate) fn link_metrics(status: Option<PathStatus>) -> LinkMetrics {
    let Some(status) = status else {
        return LinkMetrics {
            path: LinkPath::Unknown,
            rtt_ms: None,
        };
    };
    let path = match status.kind {
        PathKind::Relay => LinkPath::Relay,
        PathKind::Custom => LinkPath::Unknown,
        PathKind::Direct => match direct_ip(&status.address) {
            Some(ip) if is_local(ip) => LinkPath::Lan,
            _ => LinkPath::PeerToPeer,
        },
    };
    LinkMetrics {
        path,
        rtt_ms: Some(status.rtt.as_millis().min(u32::MAX as u128) as u32),
    }
}

/// `PathStatus::address` 是 iroh `TransportAddr` 的 Display，直连形如 `ip:192.168.1.2:7777`。
fn direct_ip(address: &str) -> Option<IpAddr> {
    let address = address.strip_prefix("ip:").unwrap_or(address);
    address
        .parse::<SocketAddr>()
        .map(|socket| socket.ip())
        .or_else(|_| address.parse::<IpAddr>())
        .ok()
}

/// 私网、链路本地、回环（含 IPv6 ULA 与 IPv4 映射地址）。
fn is_local(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            ip.is_loopback() || ip.is_unicast_link_local() || (ip.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn status(kind: PathKind, address: &str) -> Option<PathStatus> {
        Some(PathStatus {
            remote: "peer".to_string(),
            kind,
            address: address.to_string(),
            rtt: Duration::from_millis(12),
        })
    }

    #[test]
    fn classifies_selected_paths() {
        let lan = link_metrics(status(PathKind::Direct, "ip:192.168.1.20:7777"));
        assert_eq!(lan.path, LinkPath::Lan);
        assert_eq!(lan.rtt_ms, Some(12));
        for address in [
            "ip:10.0.0.2:1",
            "ip:127.0.0.1:1",
            "ip:169.254.3.4:1",
            "ip:[fd00::1]:1",
            "ip:[fe80::1]:1",
            "ip:[::ffff:192.168.0.9]:1",
        ] {
            assert_eq!(
                link_metrics(status(PathKind::Direct, address)).path,
                LinkPath::Lan,
                "{address}"
            );
        }
        assert_eq!(
            link_metrics(status(PathKind::Direct, "ip:8.8.8.8:1")).path,
            LinkPath::PeerToPeer
        );
        assert_eq!(
            link_metrics(status(PathKind::Direct, "ip:[2001:db8::1]:1")).path,
            LinkPath::PeerToPeer
        );
        assert_eq!(
            link_metrics(status(PathKind::Relay, "relay:https://r.example/")).path,
            LinkPath::Relay
        );
        assert_eq!(
            link_metrics(None),
            LinkMetrics {
                path: LinkPath::Unknown,
                rtt_ms: None
            }
        );
    }

    fn relay(address: &str) -> Option<RelaySettings> {
        Some(RelaySettings::parse(address).unwrap())
    }

    #[test]
    fn shape_follows_the_relays_of_all_runtimes() {
        assert_eq!(shape_for(&[]), Shape::Preset, "没有机器");
        assert_eq!(shape_for(&[None, None]), Shape::Preset, "都没指定 relay");
        assert_eq!(
            shape_for(&[relay("relay.example"), relay("https://relay.example")]),
            Shape::Relay(RelaySettings::parse("relay.example").unwrap()),
            "都指定了同一个 relay（写法不同也算同一个）"
        );
        assert_eq!(
            shape_for(&[relay("relay.example")]),
            Shape::Relay(RelaySettings::parse("relay.example").unwrap())
        );
        assert_eq!(
            shape_for(&[relay("a.example"), relay("b.example")]),
            Shape::Preset,
            "不同 relay"
        );
        assert_eq!(
            shape_for(&[relay("a.example"), None]),
            Shape::Preset,
            "有的指定有的没指定"
        );
        assert_eq!(shape_for(&[None, relay("a.example")]), Shape::Preset);
    }

    #[test]
    fn configure_reports_shape_changes_only() {
        let dialer = IrohDialer::new(SecretKey::generate());
        assert!(!dialer.configure(&[]).unwrap(), "起始就是 n0 预设");
        assert!(!dialer.configure(&[None]).unwrap());
        assert!(
            dialer
                .configure(&[Some("relay.example".to_string())])
                .unwrap()
        );
        assert!(
            !dialer
                .configure(&[
                    Some("relay.example".to_string()),
                    Some("https://relay.example/".to_string())
                ])
                .unwrap()
        );
        assert!(
            dialer
                .configure(&[Some("relay.example".to_string()), None])
                .unwrap()
        );
        let bad = dialer
            .configure(&[Some("http://[::1".to_string())])
            .unwrap_err();
        assert_eq!(bad.kind, SmeltErrorKind::InvalidPairing);
    }

    #[test]
    fn validate_rejects_bad_targets() {
        let endpoint = SecretKey::generate().public().to_string();
        let (parsed, relay) = validate(&endpoint, Some("relay.example.test")).unwrap();
        assert_eq!(parsed.to_string(), endpoint);
        assert_eq!(
            relay.unwrap().url.to_string(),
            "https://relay.example.test/"
        );

        let bad = validate("not-an-endpoint", None).unwrap_err();
        assert_eq!(bad.kind, SmeltErrorKind::InvalidPairing);
        let bad = validate(&endpoint, Some("http://[::1")).unwrap_err();
        assert_eq!(bad.kind, SmeltErrorKind::InvalidPairing);
    }
}
