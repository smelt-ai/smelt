//! smelt 远程操作网关——独立进程版（见 docs/remote-ops-roadmap.md）。
//!
//! 实际的路由/handler 都在 `smelt_remote_gateway`（这个文件和 smeltd 内嵌的
//! `remote_start` op 共用同一份，见那边的模块注释）。这个文件只负责命令行启动：
//! 解析 `--bind`/`--port`、生成 token、绑端口、打印分享链接。
//!
//! 用法：
//!   gateway [--bind 127.0.0.1] [--port 0] [--write]
//! 默认绑回环地址，不监听 `0.0.0.0`；跨机器访问交给用户自己的网
//! （Tailscale/SSH 隧道），网关自己不做中继、不做公网暴露。`--write` 开启后
//! 这条链接能 `input`（原始键盘）+ approve/deny/reply（见 smeltd「远程操控」），
//! 链接本身就是授权，不再额外要求当面确认。

use clap::Parser;
use smelt_remote_gateway as remote_gateway;
use std::net::IpAddr;

/// smelt 远程操作网关（独立进程版）。
///
/// 默认绑回环，不监听 `0.0.0.0`。跨机器访问用 Tailscale / SSH 隧道。
#[derive(Debug, Parser)]
#[command(name = "gateway", version, about)]
struct Cli {
    /// 绑定地址。默认只回环。
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,

    /// 端口。0 表示让内核挑一个空闲端口。
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// 开启可写：input + approve/deny/reply。链接本身就是授权。
    #[arg(long)]
    write: bool,
}

#[tokio::main]
async fn main() {
    // 独立网关也会读取主题、agent profiles 和历史标题，必须与内嵌网关使用同一存储。
    smelt_core::sqlite_state::enable_sqlite_state();
    let cli = Cli::parse();

    // 128 位随机 token，一次性打印在 stdout；不落盘、不设过期。
    let token = uuid::Uuid::new_v4().simple().to_string();
    let app = remote_gateway::build_router(token.clone(), cli.write);

    let listener = match tokio::net::TcpListener::bind((cli.bind, cli.port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("绑定 {}:{} 失败：{e}", cli.bind, cli.port);
            std::process::exit(1);
        }
    };
    let addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("读取绑定地址失败：{e}");
            std::process::exit(1);
        }
    };

    println!(
        "smelt 远程操作网关（{}）",
        if cli.write {
            "可写：input + approve/deny/reply"
        } else {
            "只读观战"
        }
    );
    println!(
        "绑定：{addr}（默认只回环，不监听 0.0.0.0；跨机器访问用你自己的网：Tailscale / SSH 隧道）"
    );
    println!("分享链接（会话列表）：http://{addr}/?token={token}");

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("网关退出：{e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn defaults_to_loopback_readonly() {
        let cli = Cli::try_parse_from(["gateway"]).expect("无参数应使用默认值");
        assert_eq!(cli.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(cli.port, 0);
        assert!(!cli.write);
    }

    #[test]
    fn parses_bind_port_and_write() {
        let cli =
            Cli::try_parse_from(["gateway", "--bind", "10.0.0.1", "--port", "8080", "--write"])
                .expect("合法参数应解析成功");
        assert_eq!(cli.bind, "10.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(cli.port, 8080);
        assert!(cli.write);
    }

    #[test]
    fn rejects_invalid_bind() {
        let err = Cli::try_parse_from(["gateway", "--bind", "not-an-ip"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn rejects_unknown_args() {
        let err = Cli::try_parse_from(["gateway", "--nope"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }
}
