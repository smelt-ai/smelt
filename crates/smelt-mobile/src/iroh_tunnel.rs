//! 手机侧的 iroh 隧道入口。
//!
//! 设计上刻意**不**把 iroh 流直接交给 WebSocket 客户端，而是在手机本地开一个
//! `127.0.0.1:<port>` 监听、把每条进来的 TCP 连接经 iroh 转发给 Mac 上的网关。
//! 这样 Dart 侧继续用 `web_socket_channel` 连 `ws://127.0.0.1:<port>/acp/ws`，
//! 现有的鉴权、重连、消息解析一行都不用改 —— 和桌面侧「不发明新协议」是同一招。
//!
//! 换句话说，这个模块是 `smelt-iroh-connect` 那个命令行工具的库化版本，
//! 区别只在于生命周期由 Flutter 控制而不是 Ctrl-C。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use once_cell::sync::Lazy;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

/// 专属 runtime。
///
/// 不复用 flutter_rust_bridge 自带的执行器：accept 循环要活得比发起它的那次
/// FFI 调用久得多，把它的存活绑在别人的实现细节上迟早出事。
static RUNTIME: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("构建 iroh runtime 失败")
});

/// 当前隧道。同一时刻只允许一条：手机上同时连两台 Mac 没有意义，
/// 而允许多条会让「本地端口是哪条隧道的」变成一笔糊涂账。
static TUNNEL: Lazy<Mutex<Option<Tunnel>>> = Lazy::new(|| Mutex::new(None));

struct Tunnel {
    peer: EndpointId,
    relay: smelt_iroh::RelaySettings,
    port: u16,
    accept_task: tokio::task::JoinHandle<()>,
    shared: Arc<Shared>,
}

struct Shared {
    endpoint: Endpoint,
    peer: EndpointId,
    target: EndpointAddr,
    /// 缓存的 QUIC 连接。一条 QUIC 连接可以承载任意多条流，所以正常情况下
    /// 只拨号一次；掉线后由下一个请求触发重拨。
    conn: Mutex<Option<Connection>>,
    inbound: InboundActivity,
}

/// 隧道最近一次从桌面收到流数据的时刻。
///
/// 手机的应用层心跳和业务数据走同一条有序流：一份大快照经 relay 传几十秒时，
/// 排在它后面的 pong 回不来，但字节一直在到。判断「连接还活着」要看这个，
/// 而不是 pong——pong 只能说明队列不忙。真断网时 QUIC 自己的保活会在空闲
/// 超时后关掉连接，流跟着断，WebSocket 也就断了。
struct InboundActivity {
    origin: Instant,
    /// 距 `origin` 的毫秒数 + 1；0 表示还没收到过。
    last: AtomicU64,
}

impl InboundActivity {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
            last: AtomicU64::new(0),
        }
    }

    fn mark(&self) {
        let now = self.origin.elapsed().as_millis() as u64 + 1;
        self.last.fetch_max(now, Ordering::Relaxed);
    }

    fn idle(&self) -> Option<Duration> {
        let last = self.last.load(Ordering::Relaxed).checked_sub(1)?;
        Some(
            self.origin
                .elapsed()
                .saturating_sub(Duration::from_millis(last)),
        )
    }
}

#[derive(Clone, Debug)]
pub struct TunnelPathStatus {
    pub kind: String,
    pub rtt_ms: u32,
}

impl Shared {
    /// 取一条可用的 QUIC 连接，必要时重拨。
    ///
    /// 手机会锁屏、切 Wi-Fi/蜂窝，QUIC 连接断掉是常态而非异常，
    /// 所以这里必须能自愈，否则用户得手动重新配对。
    async fn connection(&self) -> Result<Connection> {
        let mut slot = self.conn.lock().await;
        if let Some(conn) = slot.as_ref() {
            // close_reason() 为 None 表示还活着。
            if conn.close_reason().is_none() {
                return Ok(conn.clone());
            }
        }
        let conn = self
            .endpoint
            .connect(self.target.clone(), smelt_iroh::ALPN)
            .await
            .with_context(|| format!("拨号 {} 失败", self.peer))?;
        *slot = Some(conn.clone());
        Ok(conn)
    }

    /// 淘汰一次实际 I/O 已经证明不可用的连接。
    ///
    /// 网络切换后 QUIC 的 `close_reason` 可能暂时仍是 `None`，但开流已经失败。
    /// 只按 stable id 清理，避免并发请求误删另一条刚刚重拨成功的新连接。
    async fn invalidate_connection(&self, failed: &Connection) {
        let mut slot = self.conn.lock().await;
        if slot
            .as_ref()
            .is_some_and(|current| current.stable_id() == failed.stable_id())
        {
            *slot = None;
        }
    }

    async fn path_status(&self) -> Option<TunnelPathStatus> {
        let conn = self.conn.lock().await.as_ref()?.clone();
        if conn.close_reason().is_some() {
            return None;
        }
        let paths = conn.paths();
        let path = paths.iter().find(|path| path.is_selected())?;
        let kind = match path.remote_addr() {
            iroh::TransportAddr::Ip(address) if is_private_ip(address.ip()) => "lan",
            iroh::TransportAddr::Ip(_) => "p2p",
            iroh::TransportAddr::Relay(_) => "relay",
            _ => "p2p",
        };
        Some(TunnelPathStatus {
            kind: kind.to_string(),
            rtt_ms: path.rtt().as_millis().min(u32::MAX as u128) as u32,
        })
    }
}

fn is_private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback() || ip.is_unicast_link_local() || (ip.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

/// 启动隧道，返回手机本地的入口端口。
///
/// 幂等：对同一个 `endpoint_id` 重复调用会直接返回已有端口，不会重开监听。
/// 换了 `endpoint_id` 则先停掉旧的 —— 让两条隧道并存只会让上层分不清端口归属。
pub async fn start(endpoint_id: &str, relay_address: &str) -> Result<u16> {
    let peer: EndpointId = endpoint_id
        .parse()
        .map_err(|_| anyhow!("不是合法的 EndpointId：{endpoint_id}"))?;
    let relay = smelt_iroh::RelaySettings::parse(relay_address)?;

    let mut guard = TUNNEL.lock().await;
    if let Some(existing) = guard.as_ref() {
        if existing.peer == peer && existing.relay == relay && !existing.accept_task.is_finished() {
            return Ok(existing.port);
        }
        shutdown(guard.take().expect("刚判断过非空"));
    }

    // 客户端身份每次随机即可：认的是宿主的 EndpointId，手机自己是谁无所谓，
    // 也就省掉了在手机上安全落盘私钥这摊事。
    let secret = iroh::SecretKey::generate();
    let endpoint_relay = relay.clone();
    let endpoint = RUNTIME
        .spawn(async move { smelt_iroh::bind_endpoint(secret, vec![], &endpoint_relay).await })
        .await
        .map_err(|e| anyhow!("绑定 iroh endpoint 的任务失败：{e}"))??;

    let shared = Arc::new(Shared {
        endpoint,
        peer,
        target: EndpointAddr::new(peer).with_relay_url(relay.url.clone()),
        conn: Mutex::new(None),
        inbound: InboundActivity::new(),
    });

    // 先拨一次：把「对方不在线 / EndpointId 打错」这类错误在 start 就报出来，
    // 而不是等用户点了连接才在 WebSocket 层看到一个语焉不详的失败。
    {
        let shared = shared.clone();
        RUNTIME
            .spawn(async move { shared.connection().await.map(|_| ()) })
            .await
            .map_err(|e| anyhow!("拨号任务失败：{e}"))??;
    }

    // 只绑回环：这个端口是给本 App 自己用的，绑 0.0.0.0 等于把别人的 Mac
    // 网关暴露给同一个 Wi-Fi 下的所有人。
    let listener = RUNTIME
        .spawn(async { TcpListener::bind::<SocketAddr>("127.0.0.1:0".parse().unwrap()).await })
        .await
        .map_err(|e| anyhow!("监听任务失败：{e}"))??;
    let port = listener.local_addr()?.port();

    let accept_shared = shared.clone();
    let accept_task = RUNTIME.spawn(async move {
        loop {
            let (tcp, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("accept 失败，隧道退出：{e}");
                    return;
                }
            };
            let shared = accept_shared.clone();
            tokio::spawn(async move {
                if let Err(e) = pump(tcp, shared).await {
                    log::warn!("转发失败：{e:#}");
                }
            });
        }
    });

    *guard = Some(Tunnel {
        peer,
        relay,
        port,
        accept_task,
        shared,
    });
    Ok(port)
}

/// 停止隧道。没有隧道时是 no-op。
pub async fn stop() {
    let mut guard = TUNNEL.lock().await;
    if let Some(tunnel) = guard.take() {
        shutdown(tunnel);
    }
}

/// 当前实际承载业务数据的 iroh 路径。网络迁移后再次调用会返回新路径。
pub async fn path_status() -> Option<TunnelPathStatus> {
    let shared = {
        let guard = TUNNEL.lock().await;
        guard
            .as_ref()
            .filter(|t| !t.accept_task.is_finished())
            .map(|t| t.shared.clone())?
    };
    shared.path_status().await
}

/// 距离隧道最近一次收到桌面数据过了多久。没有隧道或还没收到过数据时为 None。
pub async fn inbound_idle() -> Option<Duration> {
    let guard = TUNNEL.lock().await;
    guard
        .as_ref()
        .filter(|t| !t.accept_task.is_finished())?
        .shared
        .inbound
        .idle()
}

fn shutdown(tunnel: Tunnel) {
    tunnel.accept_task.abort();
    let shared = tunnel.shared;
    // close() 是 async 的，但停隧道不该让 Dart 侧等，扔给 runtime 收尾即可。
    RUNTIME.spawn(async move {
        shared.endpoint.close().await;
    });
}

/// 一条本地 TCP 连接 ⟷ 一条 iroh 双向流，逐字节转发。
async fn pump(tcp: TcpStream, shared: Arc<Shared>) -> Result<()> {
    let conn = shared.connection().await?;
    let result = pump_connection(tcp, &conn, &shared.inbound).await;
    if result.is_err() {
        shared.invalidate_connection(&conn).await;
    }
    result
}

async fn pump_connection(
    tcp: TcpStream,
    conn: &Connection,
    inbound: &InboundActivity,
) -> Result<()> {
    let (mut send, mut recv) = conn.open_bi().await.context("开流失败")?;
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    let up = async {
        tokio::io::copy(&mut tcp_read, &mut send).await?;
        send.finish().map_err(std::io::Error::other)
    };
    let down = forward_inbound(&mut recv, &mut tcp_write, inbound);

    tokio::select! {
        r = up => r?,
        r = down => r?,
    }
    Ok(())
}

/// 桌面 → 手机方向的转发，每收到一块就记一次活动。
async fn forward_inbound(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    activity: &InboundActivity,
) -> std::io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = from.read(&mut buf).await?;
        if read == 0 {
            return to.shutdown().await;
        }
        activity.mark();
        to.write_all(&buf[..read]).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_forwarding_records_activity_while_bytes_flow() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let activity = InboundActivity::new();
            assert_eq!(activity.idle(), None, "没收到过数据就没有活动证据");

            let (mut desktop, tunnel_in) = tokio::io::duplex(1024);
            let (tunnel_out, mut app) = tokio::io::duplex(1024);
            let forward = forward_inbound(tunnel_in, tunnel_out, &activity);
            let drive = async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                desktop.write_all(b"chunk").await.unwrap();
                let mut got = [0u8; 5];
                app.read_exact(&mut got).await.unwrap();
                assert_eq!(&got, b"chunk");
                let idle = activity.idle().expect("收到数据后要有活动");
                assert!(idle < Duration::from_millis(50), "刚收到数据：{idle:?}");

                tokio::time::sleep(Duration::from_millis(120)).await;
                let idle = activity.idle().unwrap();
                assert!(idle >= Duration::from_millis(120), "之后没再收到：{idle:?}");
                drop(desktop);
                let mut rest = Vec::new();
                app.read_to_end(&mut rest).await.unwrap();
                assert!(rest.is_empty());
            };
            let (forwarded, ()) = tokio::join!(forward, drive);
            forwarded.unwrap();
        });
    }

    #[test]
    fn rejects_malformed_endpoint_id() {
        // 打错的配对码要在 start 就报错，而不是等 WebSocket 连不上才浮现。
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(start("not-an-endpoint-id", "https://relay.invalid"))
            .unwrap_err();
        assert!(
            err.to_string().contains("EndpointId"),
            "错误信息应指明是 EndpointId 有问题：{err}"
        );
    }

    #[test]
    fn classifies_private_and_public_ips() {
        assert!(is_private_ip("192.168.1.20".parse().unwrap()));
        assert!(is_private_ip("fd00::1".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
    }

    /// 端到端：需要一个活着的宿主。
    ///
    /// 跑法：
    ///   smelt-iroh-host --gateway 127.0.0.1:<某个 HTTP 服务>
    ///   SMELT_IROH_TEST_PEER=<EndpointId> cargo test -p smelt-mobile -- --ignored
    #[test]
    #[ignore = "需要一个活着的 smelt-iroh-host，见函数文档"]
    fn tunnels_http_to_a_live_host() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let peer = std::env::var("SMELT_IROH_TEST_PEER")
            .expect("请设置 SMELT_IROH_TEST_PEER=<EndpointId>");
        let relay = std::env::var("SMELT_IROH_TEST_RELAY")
            .expect("请设置 SMELT_IROH_TEST_RELAY=<Relay URL>");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let p = start(&peer, &relay).await.expect("隧道应能建立");
            // 幂等：同一个 peer 再来一次不该换端口。
            assert_eq!(start(&peer, &relay).await.unwrap(), p);

            let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", p))
                .await
                .expect("应能连上本地隧道口");
            tcp.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut buf = Vec::new();
            tcp.read_to_end(&mut buf).await.unwrap();
            let head = String::from_utf8_lossy(&buf);
            assert!(head.starts_with("HTTP/"), "应拿到 HTTP 响应：{head:.120}");
            assert!(head.contains(" 200 "), "应是 200：{head:.120}");

            stop().await;
            assert!(path_status().await.is_none(), "stop 后不该再报告传输路径");
        });
    }
}
