//! gateway：飞书 im/gateway 的 HTTP + protobuf 请求层，移植自 lark_tools 的 gateway.py。
//! 用本地 session cookie 以个人身份发请求。⚠️ 绝不打印 / 记录 session 值。

use super::proto::{decode_packet, encode_message, encode_packet, generic_decode, Message, Packet, Pb};
use anyhow::{Context, Result};
use std::time::Duration;

const GATEWAY_HOST: &str = "internal-api-lark-api.feishu.cn";
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36";

/// 飞书租户的 Origin（如 https://&lt;tenant&gt;.feishu.cn）。**不写死在代码里**——
/// 从环境变量 SMELT_FEISHU_TENANT 或 ~/.smelt/config.toml 的 feishu_tenant 读，
/// 避免把公司身份硬编码进仓库。
pub(crate) fn tenant_origin() -> Result<String> {
    if let Ok(t) = std::env::var("SMELT_FEISHU_TENANT") {
        let t = t.trim();
        if !t.is_empty() {
            return Ok(format!("https://{t}.feishu.cn"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        if let Ok(text) = std::fs::read_to_string(home.join(".smelt/config.toml")) {
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix("feishu_tenant") {
                    if let Some(eq) = rest.split('=').nth(1) {
                        let t = eq.trim().trim_matches('"');
                        if !t.is_empty() {
                            return Ok(format!("https://{t}.feishu.cn"));
                        }
                    }
                }
            }
        }
    }
    anyhow::bail!(
        "未配置飞书租户：请设置 SMELT_FEISHU_TENANT 环境变量，\
         或在 ~/.smelt/config.toml 写 feishu_tenant = \"<你的租户>\""
    )
}

/// 读取本地 session（~/.lark_cli/session）。返回值是凭据，调用方务必避免打印。
pub fn load_session() -> Result<String> {
    let path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("无法定位 home 目录"))?
        .join(".lark_cli/session");
    let raw = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "读取 {:?} 失败——请先用 lark_tools 的 `lark_cli.py login` 扫码登录飞书",
            path
        )
    })?;
    let v = raw.trim().to_string();
    if v.is_empty() {
        anyhow::bail!("session 为空，请重新登录飞书");
    }
    Ok(v)
}

/// 向 im/gateway 发一个 protobuf 请求，返回响应原始字节。
pub async fn send(session: &str, cmd: u64, payload: &[u8]) -> Result<Vec<u8>> {
    let cid = uuid::Uuid::new_v4().to_string();
    let request_id = uuid::Uuid::new_v4().to_string();
    let body = encode_packet(1, cmd, payload, &cid);
    let origin = tenant_origin()?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("https://{GATEWAY_HOST}/im/gateway/"))
        .header("Content-Type", "application/x-protobuf")
        .header("X-Command", cmd.to_string())
        .header("X-Request-Id", request_id)
        .header("X-Source", "web")
        .header("x-command-version", "7.61.0")
        .header("x-web-version", "7.61.0")
        .header("x-lgw-terminal-type", "2")
        .header("x-lgw-os-type", "3")
        .header("locale", "zh_CN")
        .header("Cookie", format!("session={session}"))
        .header("User-Agent", UA)
        .header("Origin", &origin)
        .header("Referer", format!("{origin}/"))
        .body(body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("请求 im/gateway 失败")?;

    let status = resp.status();
    let bytes = resp.bytes().await.context("读取 gateway 响应失败")?;
    if !status.is_success() {
        anyhow::bail!("gateway 返回 HTTP {}", status);
    }
    Ok(bytes.to_vec())
}

/// 解码响应：返回外层 Packet 与解码后的 payload。
pub fn decode_response(buffer: &[u8]) -> (Packet, Message) {
    let packet = decode_packet(buffer);
    let payload = generic_decode(&packet.payload);
    (packet, payload)
}

/// 探活（cmd 84，拉部门列表）：成功返回 status==0 即登录态有效。
pub async fn check_auth(session: &str) -> bool {
    let payload = encode_message(&[(1, Pb::Str("0".into()))]);
    match send(session, 84, &payload).await {
        Ok(buf) if !buf.is_empty() => decode_packet(&buf).status == 0,
        _ => false,
    }
}
