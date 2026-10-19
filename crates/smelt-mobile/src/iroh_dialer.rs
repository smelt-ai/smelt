//! 真实传输：经 iroh `smelt/runtime/1` 连运行时。
//!
//! 整个进程只有一个 [`IrohClient`]（同一把设备密钥同时绑几个 endpoint 会让 relay 上出现
//! 重复节点）。它按 relay 设置构造：给了自建 relay 用 Minimal + 该 relay + 系统 DNS，
//! 没给用 n0 预设。切到 relay 不同的运行时时先丢掉旧的再建新的。

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use iroh::SecretKey;
use smelt_iroh::runtime::IrohClient;
use smelt_iroh::{EndpointId, PathKind, PathStatus, RelaySettings};
use smelt_runtime_client::{Connector as _, RuntimeClient};

use crate::api::connection::{LinkMetrics, LinkPath};
use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::connection::{Dialer, Link, Route, Target};

pub(crate) struct IrohDialer {
    secret: SecretKey,
    client: Mutex<Option<CachedClient>>,
}

struct CachedClient {
    relay: Option<RelaySettings>,
    client: Arc<IrohClient>,
}

impl IrohDialer {
    pub(crate) fn new(secret: SecretKey) -> Self {
        Self {
            secret,
            client: Mutex::new(None),
        }
    }

    /// 按 relay 设置拿 IrohClient：相同就复用，不同就先丢旧的再建。绑定会起 tokio 运行时
    /// 并 `block_on`，只能在普通线程上调。
    fn client_for(&self, relay: Option<&RelaySettings>) -> Result<Arc<IrohClient>, SmeltError> {
        let mut slot = self.client.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = slot.as_ref()
            && cached.relay.as_ref() == relay
        {
            return Ok(Arc::clone(&cached.client));
        }
        // 先放掉自己持有的旧 endpoint。旧会话的探活线程若还拿着它，会在那边退出时释放。
        *slot = None;
        let client = IrohClient::new(self.secret.clone(), relay)
            .map_err(|e| error(SmeltErrorKind::Other, format!("起不了 iroh：{e:#}")))?;
        *slot = Some(CachedClient {
            relay: relay.cloned(),
            client: Arc::clone(&client),
        });
        Ok(client)
    }
}

impl Dialer for IrohDialer {
    fn route(&self, target: &Target) -> Result<Arc<dyn Route>, SmeltError> {
        let (endpoint, relay) = validate(&target.endpoint_id, target.relay_url.as_deref())?;
        let client = self.client_for(relay.as_ref())?;
        Ok(Arc::new(IrohRoute {
            client,
            endpoint,
            relay,
        }))
    }
}

struct IrohRoute {
    client: Arc<IrohClient>,
    endpoint: EndpointId,
    relay: Option<RelaySettings>,
}

impl Route for IrohRoute {
    fn link(&self) -> Link {
        // 每条链路一个新连接器：重连时不复用可能已经僵死的 QUIC 连接。
        let connector = Arc::new(self.client.connector(self.endpoint, self.relay.as_ref()));
        let for_client = Arc::clone(&connector);
        Link {
            client: RuntimeClient::new(move || for_client.connect()),
            metrics: Arc::new(move || link_metrics(connector.path_status())),
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
