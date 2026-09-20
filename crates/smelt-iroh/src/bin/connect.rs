//! `smelt-iroh-connect`：验证用的客户端（也是手机侧逻辑的参考实现）。
//!
//! 在本地监听一个 TCP 端口，把每条进来的连接经 iroh 转发给宿主的 EndpointId。
//! 于是 `curl http://127.0.0.1:<local>/?token=...` 就等价于访问那台 Mac 上的
//! 网关——**整条链路走 iroh，不依赖任何公网 URL**。
//!
//! 用法：
//! ```text
//! smelt-iroh-connect --peer <endpoint-id> --relay relay.example.com
//!   [--listen 127.0.0.1:0]
//! ```
//!
//! `crates/smelt-mobile` 接 iroh 时要复用的就是 `open_bi` 那几行；差别只是
//! 手机侧不需要本地 TCP 监听，直接把流交给 tungstenite。

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use iroh::{EndpointAddr, EndpointId};
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// 经 iroh 拨到宿主，并在本机开一个 TCP 入口做验证。
#[derive(Debug, Parser)]
#[command(name = "smelt-iroh-connect", version, about)]
struct Cli {
    /// 宿主 EndpointId。
    #[arg(long, value_name = "ENDPOINT-ID")]
    peer: EndpointId,

    /// 中继，域名或完整 URL。
    #[arg(long, value_name = "DOMAIN|URL")]
    relay: String,

    /// 本地 TCP 入口。默认 `127.0.0.1:0`（内核挑端口）。
    #[arg(long, value_name = "HOST:PORT", default_value = "127.0.0.1:0")]
    listen: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cli = Cli::parse();
    // 客户端不需要稳定身份，每次随机即可（手机侧同理：认的是宿主的 EndpointId）。
    let secret = iroh::SecretKey::generate();
    let relay = smelt_iroh::RelaySettings::parse(&cli.relay)?;
    let endpoint = smelt_iroh::bind_endpoint(secret, vec![], &relay).await?;

    let target = EndpointAddr::new(cli.peer).with_relay_url(relay.url.clone());
    let conn = endpoint
        .connect(target, smelt_iroh::ALPN)
        .await
        .with_context(|| format!("拨号 {} 失败", cli.peer))?;
    info!("已连上宿主 {}", cli.peer);

    let listener = TcpListener::bind(cli.listen).await?;
    let local = listener.local_addr()?;
    println!("本地入口：http://{local}");
    println!("（经 iroh 转发到 {}）", cli.peer);

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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn rejects_missing_required() {
        let err = Cli::try_parse_from(["smelt-iroh-connect", "--relay", "relay.example.com"])
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn rejects_unknown_args() {
        let err = Cli::try_parse_from(["smelt-iroh-connect", "--nope"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn rejects_invalid_peer() {
        let err = Cli::try_parse_from([
            "smelt-iroh-connect",
            "--peer",
            "not-an-id",
            "--relay",
            "relay.example.com",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }
}
