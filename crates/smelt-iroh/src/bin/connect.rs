//! `smelt-iroh-connect`：验证用的客户端（也是手机侧逻辑的参考实现）。
//!
//! 在本地监听一个 TCP 端口，把每条进来的连接经 iroh 转发给宿主的 EndpointId。
//! 于是 `curl http://127.0.0.1:<local>/?token=...` 就等价于访问那台 Mac 上的
//! 网关——**整条链路走 iroh，不依赖任何公网 URL**。
//!
//! 用法：
//!   smelt-iroh-connect --peer <endpoint-id> --relay relay.example.com
//!     [--relay-token TOKEN] [--listen 127.0.0.1:0]
//!
//! `crates/smelt-mobile` 接 iroh 时要复用的就是 `open_bi` 那几行；差别只是
//! 手机侧不需要本地 TCP 监听，直接把流交给 tungstenite。

use std::net::SocketAddr;

use anyhow::{Context, Result};
use iroh::{EndpointAddr, EndpointId};
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

fn parse_args() -> Result<(EndpointId, SocketAddr, String, String)> {
    let mut peer: Option<String> = None;
    let mut listen = "127.0.0.1:0".to_string();
    let mut relay: Option<String> = None;
    let mut relay_token = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--peer" => peer = args.next(),
            "--relay" => relay = args.next(),
            "--relay-token" => relay_token = args.next().unwrap_or_default(),
            "--listen" => {
                if let Some(v) = args.next() {
                    listen = v;
                }
            }
            "--help" | "-h" => {
                println!(
                    "smelt-iroh-connect --peer <endpoint-id> --relay <domain|url> \
                     [--relay-token <token>] [--listen 127.0.0.1:0]"
                );
                std::process::exit(0);
            }
            other => warn!("忽略未知参数 {other}"),
        }
    }
    let peer = peer.context("必须指定 --peer <endpoint-id>")?;
    let peer: EndpointId = peer
        .parse()
        .with_context(|| format!("不是合法的 EndpointId：{peer}"))?;
    let listen: SocketAddr = listen
        .parse()
        .with_context(|| format!("--listen 不是合法的 host:port：{listen}"))?;
    let relay = relay.context("必须指定 --relay <domain|url>")?;
    Ok((peer, listen, relay, relay_token))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let (peer, listen, relay_address, relay_token) = parse_args()?;
    // 客户端不需要稳定身份，每次随机即可（手机侧同理：认的是宿主的 EndpointId）。
    let secret = iroh::SecretKey::generate();
    let relay = smelt_iroh::RelaySettings::parse(&relay_address, &relay_token)?;
    let endpoint = smelt_iroh::bind_endpoint(secret, vec![], &relay).await?;

    let target = EndpointAddr::new(peer).with_relay_url(relay.url.clone());
    let conn = endpoint
        .connect(target, smelt_iroh::ALPN)
        .await
        .with_context(|| format!("拨号 {peer} 失败"))?;
    info!("已连上宿主 {peer}");

    let listener = TcpListener::bind(listen).await?;
    let local = listener.local_addr()?;
    println!("本地入口：http://{local}");
    println!("（经 iroh 转发到 {peer}）");

    loop {
        let (tcp, _) = listener.accept().await?;
        let conn = conn.clone();
        tokio::spawn(async move {
            if let Err(e) = pump(tcp, conn).await {
                warn!("转发失败：{e:#}");
            }
        });
    }
}

async fn pump(tcp: TcpStream, conn: iroh::endpoint::Connection) -> Result<()> {
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
