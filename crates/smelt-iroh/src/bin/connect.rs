//! `smelt-iroh-connect`：经 iroh 连一个远程运行时，验证配对和远程调用。
//!
//! ```text
//! smelt-iroh-connect pair '<smelt+iroh://…>' [--name 名字]
//! smelt-iroh-connect describe <endpoint-id> [--relay URL]
//! smelt-iroh-connect devices <endpoint-id> [--relay URL]
//! ```
//!
//! 用的是桌面也会用的 `RuntimeConnector` 和 `RuntimeClient`，身份是
//! `~/.smelt/client-iroh-secret`。

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use iroh::EndpointId;
use smelt_iroh::RelaySettings;
use smelt_iroh::runtime::IrohClient;
use smelt_model::control_api::{DeviceList, DevicePair};
use smelt_model::remote_config::PairParams;
use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::EmptyParams;

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Parser)]
#[command(name = "smelt-iroh-connect", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 用一次性配对串把本机登记成运行时的设备。
    Pair {
        uri: String,
        #[arg(long, default_value = "smelt-iroh-connect")]
        name: String,
    },
    /// 握手：运行时身份、版本、平台。
    Describe {
        peer: EndpointId,
        #[arg(long)]
        relay: Option<String>,
    },
    /// 列出运行时上的已配对设备。
    Devices {
        peer: EndpointId,
        #[arg(long)]
        relay: Option<String>,
    },
}

fn client(peer: EndpointId, relay: Option<&str>) -> Result<RuntimeClient> {
    let relay = relay.map(RelaySettings::parse).transpose()?;
    let secret = smelt_iroh::load_or_create_secret(&smelt_iroh::default_client_secret_path()?)?;
    let iroh = IrohClient::new(secret)?;
    eprintln!("本机设备身份：{}", iroh.id());
    Ok(RuntimeClient::new(iroh.connector(peer, relay.as_ref())))
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Pair { uri, name } => {
            let pairing = smelt_pairing::parse_pairing_uri(&uri).map_err(|e| anyhow!(e))?;
            let peer: EndpointId = pairing
                .endpoint_id
                .parse()
                .map_err(|_| anyhow!("配对串里的 endpoint id 不合法"))?;
            let client = client(peer, pairing.relay_url.as_deref())?;
            client
                .call::<DevicePair>(
                    &PairParams {
                        code: pairing.code,
                        name,
                    },
                    TIMEOUT,
                )
                .context("配对失败")?;
            println!("已配对 {peer}");
        }
        Command::Describe { peer, relay } => {
            let described = client(peer, relay.as_deref())?.describe()?;
            println!("{}", serde_json::to_string_pretty(&described)?);
        }
        Command::Devices { peer, relay } => {
            let devices = client(peer, relay.as_deref())?
                .call::<DeviceList>(&EmptyParams::default(), TIMEOUT)?;
            println!("{}", serde_json::to_string_pretty(&devices)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn requires_a_subcommand() {
        let err = Cli::try_parse_from(["smelt-iroh-connect"]).unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn rejects_an_invalid_peer() {
        let err = Cli::try_parse_from(["smelt-iroh-connect", "describe", "not-a-key"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }
}
