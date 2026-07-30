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

use anyhow::{Context, Result, anyhow};
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use once_cell::sync::Lazy;
use tokio::io::AsyncWriteExt as _;
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
}

/// 启动隧道，返回手机本地的入口端口。
///
/// 幂等：对同一个 `endpoint_id` 重复调用会直接返回已有端口，不会重开监听。
/// 换了 `endpoint_id` 则先停掉旧的 —— 让两条隧道并存只会让上层分不清端口归属。
pub async fn start(endpoint_id: &str, relay_address: &str, relay_token: &str) -> Result<u16> {
    let peer: EndpointId = endpoint_id
        .parse()
        .map_err(|_| anyhow!("不是合法的 EndpointId：{endpoint_id}"))?;
    let relay = smelt_iroh::RelaySettings::parse(relay_address, relay_token)?;

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

/// 当前隧道的本地端口，没有则 `None`。
pub async fn port() -> Option<u16> {
    let guard = TUNNEL.lock().await;
    guard
        .as_ref()
        .filter(|t| !t.accept_task.is_finished())
        .map(|t| t.port)
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
    let (mut send, mut recv) = conn.open_bi().await.context("开流失败")?;
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    let up = async {
        tokio::io::copy(&mut tcp_read, &mut send).await?;
        send.finish().map_err(std::io::Error::other)
    };
    let down = async {
        tokio::io::copy(&mut recv, &mut tcp_write).await?;
        tcp_write.shutdown().await
    };

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
    fn rejects_malformed_endpoint_id() {
        // 打错的配对码要在 start 就报错，而不是等 WebSocket 连不上才浮现。
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(start("not-an-endpoint-id")).unwrap_err();
        assert!(
            err.to_string().contains("EndpointId"),
            "错误信息应指明是 EndpointId 有问题：{err}"
        );
    }

    #[test]
    fn port_is_none_before_start() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(rt.block_on(port()), None);
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
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let p = start(&peer).await.expect("隧道应能建立");
            assert_eq!(port().await, Some(p), "port() 应报告刚建立的隧道");
            // 幂等：同一个 peer 再来一次不该换端口。
            assert_eq!(start(&peer).await.unwrap(), p);

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
            assert_eq!(port().await, None, "stop 后不该再报告端口");
        });
    }
}
