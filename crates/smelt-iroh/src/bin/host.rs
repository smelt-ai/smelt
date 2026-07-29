//! `smelt-iroh-host`：Mac 侧宿主。
//!
//! 绑 iroh Endpoint，打印 EndpointId（= 未来配对二维码的内容），
//! 把每条进来的双向流转成一条到本机 `remote_gateway` 的 TCP 连接。
//!
//! 用法：
//!   smelt-iroh-host --gateway 127.0.0.1:9877 [--secret ~/.smelt/iroh-secret]
//!
//! **注意**：能拨到这个 EndpointId 的人就能访问网关，鉴权仍然靠网关自己的
//! token（隧道只负责把字节送到，不做授权判断）。这与 `gateway.rs` 里
//! 「链接本身就是授权」的既有立场一致。

use std::net::SocketAddr;

use anyhow::{Context, Result};
use tracing::warn;
use tracing_subscriber::EnvFilter;

fn parse_args() -> Result<(SocketAddr, Option<String>)> {
    let mut gateway: Option<String> = None;
    let mut secret: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--gateway" => gateway = args.next(),
            "--secret" => secret = args.next(),
            "--help" | "-h" => {
                println!("smelt-iroh-host --gateway <host:port> [--secret <path>]");
                std::process::exit(0);
            }
            other => warn!("忽略未知参数 {other}"),
        }
    }
    let gateway = gateway.context("必须指定 --gateway <host:port>")?;
    let addr = gateway
        .parse()
        .with_context(|| format!("--gateway 不是合法的 host:port：{gateway}"))?;
    Ok((addr, secret))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let (gateway, secret_path) = parse_args()?;
    let secret_path = match secret_path {
        Some(p) => std::path::PathBuf::from(p),
        None => smelt_iroh::default_secret_path()?,
    };
    let secret = smelt_iroh::load_or_create_secret(&secret_path)?;
    let endpoint = smelt_iroh::bind_endpoint(secret, vec![smelt_iroh::ALPN.to_vec()]).await?;

    println!("smelt iroh 宿主已就绪");
    println!("EndpointId（配对码，重启不变）：{}", endpoint.id());
    println!("转发到本机网关：{gateway}");

    // Ctrl-C 收摊：让 endpoint 有机会跟对端道别，而不是被硬杀。
    smelt_iroh::serve_tunnel(endpoint, gateway, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await;
    Ok(())
}
