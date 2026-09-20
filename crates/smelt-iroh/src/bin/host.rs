//! `smelt-iroh-host`：Mac 侧宿主。
//!
//! 绑 iroh Endpoint，打印 EndpointId（= 未来配对二维码的内容），
//! 把每条进来的双向流转成一条到本机 `remote_gateway` 的 TCP 连接。
//!
//! 用法：
//!   smelt-iroh-host --gateway 127.0.0.1:9877 --relay relay.example.com
//!     [--secret ~/.smelt/iroh-secret]
//!
//! **注意**：能拨到这个 EndpointId 的人就能访问网关，鉴权仍然靠网关自己的
//! token（隧道只负责把字节送到，不做授权判断）。这与 `gateway.rs` 里
//! 「链接本身就是授权」的既有立场一致。

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

/// Mac 侧 iroh 隧道宿主：把进来的流转发到本机网关。
#[derive(Debug, Parser)]
#[command(name = "smelt-iroh-host", version, about)]
struct Cli {
    /// 本机网关地址，形如 `127.0.0.1:9877`。
    #[arg(long, value_name = "HOST:PORT")]
    gateway: SocketAddr,

    /// 中继，域名或完整 URL。
    #[arg(long, value_name = "DOMAIN|URL")]
    relay: String,

    /// 私钥路径。省略则用 `~/.smelt/iroh-secret`。
    #[arg(long, value_name = "PATH")]
    secret: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cli = Cli::parse();
    let secret_path = match cli.secret {
        Some(p) => p,
        None => smelt_iroh::default_secret_path()?,
    };
    let secret = smelt_iroh::load_or_create_secret(&secret_path)?;
    let relay = smelt_iroh::RelaySettings::parse(&cli.relay)?;
    let endpoint =
        smelt_iroh::bind_endpoint(secret, vec![smelt_iroh::ALPN.to_vec()], &relay).await?;

    println!("smelt iroh 宿主已就绪");
    println!("EndpointId（配对码，重启不变）：{}", endpoint.id());
    println!("转发到本机网关：{}", cli.gateway);

    // Ctrl-C 收摊：让 endpoint 有机会跟对端道别，而不是被硬杀。
    smelt_iroh::serve_tunnel(endpoint, cli.gateway, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn parses_gateway_and_relay() {
        let cli = Cli::try_parse_from([
            "smelt-iroh-host",
            "--gateway",
            "127.0.0.1:9877",
            "--relay",
            "relay.example.com",
        ])
        .expect("合法参数应解析成功");
        assert_eq!(cli.gateway, "127.0.0.1:9877".parse::<SocketAddr>().unwrap());
        assert_eq!(cli.relay, "relay.example.com");
        assert!(cli.secret.is_none());
    }

    #[test]
    fn rejects_missing_required() {
        let err = Cli::try_parse_from(["smelt-iroh-host", "--gateway", "127.0.0.1:9"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn rejects_unknown_args() {
        let err = Cli::try_parse_from([
            "smelt-iroh-host",
            "--gateway",
            "127.0.0.1:9",
            "--relay",
            "relay.example.com",
            "--nope",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn rejects_invalid_gateway() {
        let err = Cli::try_parse_from([
            "smelt-iroh-host",
            "--gateway",
            "not-a-socket",
            "--relay",
            "relay.example.com",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }
}
