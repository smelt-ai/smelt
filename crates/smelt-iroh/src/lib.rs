//! iroh 隧道：手机按**持久 EndpointId** 拨号 Mac，同网 mDNS 直连、跨网打洞、
//! 打洞失败自动回退中继（网络变好还会自己升级回直连）。
//!
//! 刻意**不发明新协议**：一条 iroh 双向流对应一条到本机 `remote_gateway` 的 TCP
//! 连接，逐字节转发。上层 HTTP / WebSocket / token 鉴权原样复用，`/acp/ws` 那套
//! 手机端代码一行都不用改。这也是为什么这里没有任何业务帧的概念——业务在网关里。
//!
//! 对比现有 `smelt-bridge`（WebRTC）：那条路要自建信令 + coturn，且只有浏览器
//! SPA 用得上；这条路 Rust 原生，手机 app 能直接复用（见 `crates/smelt-mobile`）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use iroh::{Endpoint, SecretKey};

/// 隧道的 ALPN。换协议语义时必须同步改两端，否则 iroh 会直接拒绝握手——
/// 这正是我们要的：宁可连不上，也不要两端对协议理解不一致还硬跑。
pub const ALPN: &[u8] = b"smelt/tunnel/1";

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

/// 用给定密钥起一个 iroh Endpoint。
///
/// `N0` preset 自带：中继回退 + pkarr/DNS 地址发布 + 本地发现，
/// 也就是「同网直连、跨网打洞、打不通走中继」这套行为的来源。
pub async fn bind_endpoint(secret: SecretKey, alpns: Vec<Vec<u8>>) -> Result<Endpoint> {
    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret)
        .alpns(alpns)
        .bind()
        .await
        .context("iroh Endpoint 绑定失败")?;
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

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
