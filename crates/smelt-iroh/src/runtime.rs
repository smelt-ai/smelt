//! 运行时链路：客户端经 iroh 直接说运行时的协议（设计文档 D2）。
//!
//! 一条 QUIC 双向流就是一条连接，和 unix socket 上的一条连接等价。两端都把流桥接成
//! `UnixStream::pair()` 的一端：运行时把另一端交给现有分发器，客户端把它交给
//! `RuntimeClient`。上层的阻塞读写、超时、`try_clone` 对两种传输完全一样。
//!
//! 鉴权不在这里：服务端把对端的 endpoint id 连同流一起交出去，由运行时按已配对设备
//! 判断（D3）。

use std::collections::HashMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use tokio::io::AsyncWriteExt as _;
use tracing::{info, warn};

use crate::{ConnectionEvent, ConnectionObserver, PathObserver, RelaySettings};

/// 运行时协议的 ALPN。协议语义变了就换版本号，两端对不上 iroh 直接拒绝握手。
pub const RUNTIME_ALPN: &[u8] = b"smelt/runtime/1";

/// 运行时一侧接收桥接好的流。在 tokio 线程上被调用，不能阻塞：处理放到自己的线程。
pub trait StreamAcceptor: Send + Sync + 'static {
    /// `remote` 是对端的 endpoint id。
    fn accept(&self, remote: &str, stream: UnixStream);
}

/// 活着的连接，按对端 endpoint id 分组。撤销设备时据此立即断开它的全部连接。
#[derive(Clone, Default)]
pub struct ConnectionRegistry {
    inner: Arc<Mutex<HashMap<String, Vec<Connection>>>>,
}

impl ConnectionRegistry {
    fn insert(&self, remote: &str, conn: Connection) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.entry(remote.to_string()).or_default().push(conn);
    }

    fn remove(&self, remote: &str, conn: &Connection) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(conns) = inner.get_mut(remote) {
            conns.retain(|existing| existing.stable_id() != conn.stable_id());
            if conns.is_empty() {
                inner.remove(remote);
            }
        }
    }

    /// 断开某台设备的全部连接。返回断开了几条。
    pub fn close(&self, remote: &str) -> usize {
        let conns = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(remote)
            .unwrap_or_default();
        for conn in &conns {
            conn.close(1u32.into(), b"revoked");
        }
        conns.len()
    }

    /// 当前连着的设备。
    pub fn connected(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}

/// 用户配置了自建 relay 就只用它（不做公共地址发现）；没配就用 n0 的公共 relay 和 DNS
/// 地址发现，只凭 endpoint id 就能找到对方，地址变了也能重新找到。
pub async fn bind_runtime_endpoint(
    secret: SecretKey,
    alpns: Vec<Vec<u8>>,
    relay: Option<&RelaySettings>,
) -> Result<Endpoint> {
    match relay {
        Some(relay) => crate::bind_endpoint(secret, alpns, relay).await,
        None => Endpoint::builder(iroh::endpoint::presets::N0)
            .secret_key(secret)
            .alpns(alpns)
            .bind()
            .await
            .context("iroh Endpoint 绑定失败"),
    }
}

/// 接收运行时连接直到 `shutdown` 完成。每条双向流桥接后交给 `acceptor`。
pub async fn serve_runtime(
    endpoint: Endpoint,
    acceptor: Arc<dyn StreamAcceptor>,
    registry: ConnectionRegistry,
    shutdown: impl std::future::Future<Output = ()> + Send,
    path_observer: Option<PathObserver>,
    conn_observer: Option<ConnectionObserver>,
) {
    tokio::pin!(shutdown);
    loop {
        let incoming = tokio::select! {
            _ = &mut shutdown => break,
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => incoming,
                None => break,
            },
        };
        let acceptor = Arc::clone(&acceptor);
        let registry = registry.clone();
        let path_observer = path_observer.clone();
        let conn_observer = conn_observer.clone();
        tokio::spawn(async move {
            if let Err(error) =
                serve_connection(incoming, acceptor, registry, path_observer, conn_observer).await
            {
                warn!("iroh 运行时连接处理失败：{error:#}");
            }
        });
    }
    endpoint.close().await;
}

async fn serve_connection(
    incoming: iroh::endpoint::Incoming,
    acceptor: Arc<dyn StreamAcceptor>,
    registry: ConnectionRegistry,
    path_observer: Option<PathObserver>,
    conn_observer: Option<ConnectionObserver>,
) -> Result<()> {
    let conn = incoming.await.context("握手失败")?;
    let connection_id = conn.stable_id();
    let remote = conn.remote_id().to_string();
    info!(%remote, "iroh 已接受运行时连接");
    registry.insert(&remote, conn.clone());
    if let Some(observer) = conn_observer.as_ref() {
        let connected_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        observer(ConnectionEvent::Connected {
            connection_id,
            remote_id: remote.clone(),
            connected_at,
        });
    }
    crate::observe_selected_paths(conn.clone(), remote.clone(), path_observer);

    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(pair) => pair,
            Err(error) => {
                info!(%remote, "iroh 运行时连接结束：{error}");
                break;
            }
        };
        match bridge(send, recv) {
            Ok(stream) => acceptor.accept(&remote, stream),
            Err(error) => warn!(%remote, "桥接 iroh 流失败：{error}"),
        }
    }

    registry.remove(&remote, &conn);
    if let Some(observer) = conn_observer {
        observer(ConnectionEvent::Disconnected {
            connection_id,
            remote_id: remote,
        });
    }
    Ok(())
}

/// 把一条 QUIC 双向流接到 `UnixStream::pair()` 的一端，返回另一端。必须在 tokio
/// 运行时里调用：搬运任务挂在当前运行时上。
pub fn bridge(send: SendStream, recv: RecvStream) -> io::Result<UnixStream> {
    let (ours, theirs) = UnixStream::pair()?;
    ours.set_nonblocking(true)?;
    let ours = tokio::net::UnixStream::from_std(ours)?;
    tokio::spawn(pump(send, recv, ours));
    Ok(theirs)
}

/// 两个方向分别搬到对端 EOF。对端只是写完（半关闭）时，另一方向继续，回包还要回去；
/// 任一方向出错，或本地一端关闭，就整条收摊，避免长流在对端消失后挂着。
async fn pump(mut send: SendStream, mut recv: RecvStream, unix: tokio::net::UnixStream) {
    let (mut unix_read, mut unix_write) = unix.into_split();
    let up = async {
        tokio::io::copy(&mut recv, &mut unix_write).await?;
        unix_write.shutdown().await
    };
    let down = async {
        tokio::io::copy(&mut unix_read, &mut send).await?;
        send.finish().map_err(io::Error::other)
    };
    tokio::pin!(up, down);
    let mut up_done = false;
    loop {
        tokio::select! {
            result = &mut up, if !up_done => match result {
                Ok(()) => up_done = true,
                Err(_) => return,
            },
            _ = &mut down => return,
        }
    }
}

/// 客户端：经 iroh 连一个远程运行时。实现 [`smelt_runtime_client::Connector`]，每次
/// `connect` 在同一条 QUIC 连接上新开一条流；连接断了就重拨。
pub struct RuntimeConnector {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
    target: EndpointAddr,
    conn: tokio::sync::Mutex<Option<Connection>>,
}

impl RuntimeConnector {
    /// `secret` 是这台客户端自己的身份，运行时按它的公钥认设备，所以必须持久。
    pub fn new(
        secret: SecretKey,
        target: EndpointId,
        relay: Option<RelaySettings>,
    ) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("smelt-iroh-client")
            .enable_all()
            .build()
            .context("起不了 iroh 客户端的 tokio 运行时")?;
        let endpoint =
            runtime.block_on(bind_runtime_endpoint(secret, Vec::new(), relay.as_ref()))?;
        let mut addr = EndpointAddr::new(target);
        if let Some(relay) = relay {
            addr = addr.with_relay_url(relay.url);
        }
        Ok(Self {
            runtime,
            endpoint,
            target: addr,
            conn: tokio::sync::Mutex::new(None),
        })
    }

    async fn connection(&self) -> io::Result<Connection> {
        let mut slot = self.conn.lock().await;
        if let Some(conn) = slot.as_ref()
            && conn.close_reason().is_none()
        {
            return Ok(conn.clone());
        }
        let conn = self
            .endpoint
            .connect(self.target.clone(), RUNTIME_ALPN)
            .await
            .map_err(|error| io::Error::new(io::ErrorKind::ConnectionRefused, error))?;
        *slot = Some(conn.clone());
        Ok(conn)
    }

    async fn forget(&self, failed: &Connection) {
        let mut slot = self.conn.lock().await;
        if slot
            .as_ref()
            .is_some_and(|current| current.stable_id() == failed.stable_id())
        {
            *slot = None;
        }
    }
}

impl smelt_runtime_client::Connector for RuntimeConnector {
    fn connect(&self) -> io::Result<UnixStream> {
        self.runtime.block_on(async {
            let conn = self.connection().await?;
            match conn.open_bi().await {
                Ok((send, recv)) => bridge(send, recv),
                // 网络切换后旧连接可能还没察觉自己死了：丢掉重拨一次。
                Err(_) => {
                    self.forget(&conn).await;
                    let conn = self.connection().await?;
                    let (send, recv) = conn.open_bi().await.map_err(io::Error::other)?;
                    bridge(send, recv)
                }
            }
        })
    }
}

impl Drop for RuntimeConnector {
    fn drop(&mut self) {
        let endpoint = self.endpoint.clone();
        self.runtime.block_on(async move { endpoint.close().await });
    }
}
