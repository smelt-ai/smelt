//! iroh 隧道：手机按**持久 EndpointId** 拨号 Mac，同网 mDNS 直连、跨网打洞、
//! 打洞失败自动回退中继（网络变好还会自己升级回直连）。
//!
//! 刻意**不发明新协议**：一条 iroh 双向流对应一条到本机 `remote_gateway` 的 TCP
//! 连接，逐字节转发。上层 HTTP / WebSocket / token 鉴权原样复用，`/acp/ws` 那套
//! 手机端代码一行都不用改。这也是为什么这里没有任何业务帧的概念——业务在网关里。
//!
//! 这是**唯一**的公网通路。早先还有 Cloudflare quick tunnel 和自建信令 + WebRTC
//! 两条路，都已下线：前者 URL 每次重启都变、二维码活不过一晚，后者要维护信令 +
//! coturn，且只对浏览器面板有意义，而浏览器面板本身也一并去掉了。

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt as _;
use iroh::{Endpoint, RelayMode, RelayUrl, SecretKey, endpoint::Connection};
use iroh_relay::{RelayConfig, RelayMap};
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpStream;
use tracing::{info, warn};

/// 隧道的 ALPN。换协议语义时必须同步改两端，否则 iroh 会直接拒绝握手——
/// 这正是我们要的：宁可连不上，也不要两端对协议理解不一致还硬跑。
pub const ALPN: &[u8] = b"smelt/tunnel/1";

/// 用户配置的 relay。Smelt 不提供默认值，避免在未告知用户时访问第三方基础设施。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelaySettings {
    pub url: RelayUrl,
}

impl RelaySettings {
    /// 接受域名、IP 或完整 URL；省略 scheme 时按生产 relay 补成 HTTPS。
    pub fn parse(address: &str) -> Result<Self> {
        let address = address.trim();
        anyhow::ensure!(!address.is_empty(), "请先填写 iroh relay 地址");
        let normalized = if address.contains("://") {
            address.to_string()
        } else {
            format!("https://{address}")
        };
        let url: RelayUrl = normalized
            .parse()
            .with_context(|| format!("不是合法的 iroh relay 地址：{address}"))?;
        Ok(Self { url })
    }

    fn relay_map(&self) -> RelayMap {
        RelayConfig::from(self.url.clone()).into()
    }
}

/// iroh 当前实际用于发送业务数据的路径类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    Direct,
    Relay,
    Custom,
}

impl fmt::Display for PathKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Direct => "direct",
            Self::Relay => "relay",
            Self::Custom => "custom",
        })
    }
}

/// 一次选中路径的快照。iroh 会随网络变化迁移路径，因此观察者可能收到多次更新。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathStatus {
    pub remote: String,
    pub kind: PathKind,
    pub address: String,
    pub rtt: Duration,
}

pub type PathObserver = Arc<dyn Fn(PathStatus) + Send + Sync + 'static>;

/// 连接事件：移动端设备的连接与断开。
#[derive(Clone, Debug)]
pub enum ConnectionEvent {
    /// 新设备连接。
    Connected {
        /// iroh 节点 ID（公钥的十六进制表示）。
        remote_id: String,
        /// 连接建立的时间戳（Unix 秒）。
        connected_at: u64,
    },
    /// 设备断开连接。
    Disconnected {
        remote_id: String,
    },
}

pub type ConnectionObserver = Arc<dyn Fn(ConnectionEvent) + Send + Sync + 'static>;

/// 默认密钥路径：`~/.smelt/iroh-secret`。
///
/// 密钥必须落盘：EndpointId 是密钥的公钥，**它就是配对二维码的内容**。
/// 每次重启换密钥的话，二维码就会像 Cloudflare Quick Tunnel 的 URL 一样失效，
/// 而「配对码永久有效」正是我们选 iroh 的主要理由之一。
pub fn default_secret_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("找不到 $HOME")?;
    Ok(PathBuf::from(home).join(".smelt").join("iroh-secret"))
}

/// 读取密钥；不存在就生成并落盘（0600）。
pub fn load_or_create_secret(path: &Path) -> Result<SecretKey> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let raw = data_encoding::HEXLOWER
                .decode(trimmed.as_bytes())
                .with_context(|| format!("{} 不是合法的十六进制密钥", path.display()))?;
            let bytes: [u8; 32] = raw
                .as_slice()
                .try_into()
                .with_context(|| format!("{} 密钥长度不对（要 32 字节）", path.display()))?;
            return Ok(SecretKey::from_bytes(&bytes));
        }
    }

    let secret = SecretKey::generate();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("建不了目录 {}", dir.display()))?;
    }
    let encoded = data_encoding::HEXLOWER.encode(&secret.to_bytes());
    write_private(path, &format!("{encoded}\n"))
        .with_context(|| format!("写不了 {}", path.display()))?;
    Ok(secret)
}

/// 落盘私钥。unix 下**创建时**就带 0600，而不是先写再 chmod——
/// 后者中间有一小段时间文件是 0644，同机其他用户能读走私钥。
#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

/// 用用户明确配置的 relay 起一个 iroh Endpoint。
///
/// `Minimal` 不安装 n0 relay 或 n0 DNS 地址发现；双方的配对码显式携带同一个
/// relay URL，仍可借它交换地址、打洞，并在直连失败时中继。
pub async fn bind_endpoint(
    secret: SecretKey,
    alpns: Vec<Vec<u8>>,
    relay: &RelaySettings,
) -> Result<Endpoint> {
    let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(secret)
        .alpns(alpns)
        .relay_mode(RelayMode::Custom(relay.relay_map()))
        .bind()
        .await
        .context("iroh Endpoint 绑定失败")?;
    Ok(endpoint)
}

/// 跑转发循环直到 `shutdown` 完成：每条进来的 iroh 双向流对应一条到 `gateway`
/// 的 TCP 连接。
///
/// 提到 lib 里是因为有两个调用方——`smelt-iroh-host` 命令行和 smeltd 的
/// `iroh_start` op。两份实现迟早会漂移，转发逻辑这种地方漂移了很难查。
pub async fn serve_tunnel(
    endpoint: Endpoint,
    gateway: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    serve_tunnel_inner(endpoint, gateway, shutdown, None, None).await;
}

/// 与 [`serve_tunnel`] 相同，但在 iroh 选中或切换传输路径时通知观察者。
pub async fn serve_tunnel_with_observer(
    endpoint: Endpoint,
    gateway: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send,
    observer: PathObserver,
) {
    serve_tunnel_inner(endpoint, gateway, shutdown, Some(observer), None).await;
}

/// 与 [`serve_tunnel_with_observer`] 相同，但同时监听连接事件（设备连接/断开）。
pub async fn serve_tunnel_with_observers(
    endpoint: Endpoint,
    gateway: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send,
    path_observer: PathObserver,
    conn_observer: ConnectionObserver,
) {
    serve_tunnel_inner(endpoint, gateway, shutdown, Some(path_observer), Some(conn_observer)).await;
}

async fn serve_tunnel_inner(
    endpoint: Endpoint,
    gateway: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send,
    path_observer: Option<PathObserver>,
    conn_observer: Option<ConnectionObserver>,
) {
    tokio::pin!(shutdown);
    loop {
        let incoming = tokio::select! {
            _ = &mut shutdown => break,
            incoming = endpoint.accept() => match incoming {
                Some(i) => i,
                None => break,
            },
        };
        let path_observer = path_observer.clone();
        let conn_observer = conn_observer.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_conn(incoming, gateway, path_observer, conn_observer).await {
                warn!("iroh 连接处理失败：{e:#}");
            }
        });
    }
    // 通知对端「是我主动关的」，否则手机侧只能靠超时才发现，会白等一轮重连。
    endpoint.close().await;
}

async fn serve_conn(
    incoming: iroh::endpoint::Incoming,
    gateway: SocketAddr,
    path_observer: Option<PathObserver>,
    conn_observer: Option<ConnectionObserver>,
) -> Result<()> {
    let conn = incoming.await.context("握手失败")?;
    let remote = conn.remote_id();
    let remote_id = remote.to_string();
    info!(%remote, "iroh 已接受连接");

    // 通知连接事件
    if let Some(ref obs) = conn_observer {
        let connected_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        obs(ConnectionEvent::Connected {
            remote_id: remote_id.clone(),
            connected_at,
        });
    }

    observe_selected_paths(conn.clone(), remote_id.clone(), path_observer);

    // 一条连接可以开多条流（手机上多个会话各占一条），每条流独立转发。
    let result = loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(pair) => pair,
            // 对端正常关闭时 accept_bi 会返回错误，这里不算异常。
            Err(e) => {
                info!(%remote, "iroh 连接结束：{e}");
                break Ok(());
            }
        };
        tokio::spawn(async move {
            if let Err(e) = pump(send, recv, gateway).await {
                warn!("iroh 流转发失败：{e:#}");
            }
        });
    };

    // 通知断开事件
    if let Some(obs) = conn_observer {
        obs(ConnectionEvent::Disconnected { remote_id });
    }

    result
}

fn observe_selected_paths(conn: Connection, remote: String, observer: Option<PathObserver>) {
    tokio::spawn(async move {
        let mut events = conn.path_events();
        let mut last_address = None;

        report_selected_path(&conn, &remote, observer.as_ref(), &mut last_address);
        while let Some(event) = events.next().await {
            match event {
                iroh::endpoint::PathEvent::Selected { .. }
                | iroh::endpoint::PathEvent::Lagged { .. } => {
                    report_selected_path(&conn, &remote, observer.as_ref(), &mut last_address);
                }
                _ => {}
            }
        }
    });
}

fn report_selected_path(
    conn: &Connection,
    remote: &str,
    observer: Option<&PathObserver>,
    last_address: &mut Option<String>,
) {
    let paths = conn.paths();
    let Some(path) = paths.iter().find(|path| path.is_selected()) else {
        return;
    };
    let address = path.remote_addr().to_string();
    if last_address.as_ref() == Some(&address) {
        return;
    }
    *last_address = Some(address.clone());

    let kind = if path.is_ip() {
        PathKind::Direct
    } else if path.is_relay() {
        PathKind::Relay
    } else {
        PathKind::Custom
    };
    let status = PathStatus {
        remote: remote.to_string(),
        kind,
        address,
        rtt: path.rtt(),
    };
    info!(
        remote = %status.remote,
        path = %status.kind,
        address = %status.address,
        rtt_ms = status.rtt.as_millis(),
        "iroh 已选择传输路径"
    );
    if let Some(observer) = observer {
        observer(status);
    }
}

async fn pump(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    gateway: SocketAddr,
) -> Result<()> {
    let tcp = TcpStream::connect(gateway)
        .await
        .with_context(|| format!("连不上本机网关 {gateway}"))?;
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    let up = async {
        tokio::io::copy(&mut recv, &mut tcp_write).await?;
        tcp_write.shutdown().await
    };
    let down = async {
        tokio::io::copy(&mut tcp_read, &mut send).await?;
        send.finish().map_err(std::io::Error::other)
    };

    // 任一方向结束就收摊：HTTP/WS 半关闭的语义交给网关和客户端自己处理，
    // 隧道这层只保证不泄漏 task。
    tokio::select! {
        r = up => r?,
        r = down => r?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_settings_adds_https_to_domain_or_ip() {
        let domain = RelaySettings::parse("relay.example.test").unwrap();
        assert_eq!(domain.url.to_string(), "https://relay.example.test/");

        let ip = RelaySettings::parse("192.0.2.10:8443").unwrap();
        assert_eq!(ip.url.to_string(), "https://192.0.2.10:8443/");
    }

    #[test]
    fn relay_settings_rejects_empty_address() {
        assert!(RelaySettings::parse("  ").is_err());
    }

    #[test]
    fn secret_roundtrips_and_is_stable() {
        let dir = std::env::temp_dir().join(format!("smelt-iroh-test-{}", std::process::id()));
        let path = dir.join("iroh-secret");
        let first = load_or_create_secret(&path).expect("首次应生成");
        let second = load_or_create_secret(&path).expect("再次应读回");
        // EndpointId 就是二维码内容：重启后必须一模一样，否则配对码会失效。
        assert_eq!(first.public(), second.public());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_malformed_secret() {
        let dir = std::env::temp_dir().join(format!("smelt-iroh-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("iroh-secret");
        std::fs::write(&path, "not-hex\n").unwrap();
        // 损坏的密钥要报错，不能悄悄换一把新的——那等于二维码无声失效。
        assert!(load_or_create_secret(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
