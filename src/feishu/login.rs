//! login：飞书扫码登录，获取 session 写入 ~/.lark_cli/session，让 smelt 自包含、
//! 不再依赖 lark_tools 去刷新登录态。走 login.feishu.cn 的 JSON 接口（非 protobuf）。
//! 移植自 lark_tools auth.py 的 cmd_login。

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use std::time::Duration;

const INIT_URL: &str = "https://login.feishu.cn/accounts/qrlogin/init";
const POLL_URL: &str = "https://login.feishu.cn/accounts/qrlogin/polling";

/// 扫码登录的固定 header（抓包魔法值，飞书版本升级可能需更新）。
fn qr_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("x-api-version", HeaderValue::from_static("1.0.8"));
    h.insert("x-app-id", HeaderValue::from_static("2"));
    h.insert(
        "x-device-info",
        HeaderValue::from_static("device_id=0;device_name=smelt;device_os=Mac"),
    );
    h.insert("x-locale", HeaderValue::from_static("zh-CN"));
    h.insert("x-terminal-type", HeaderValue::from_static("2"));
    h
}

pub async fn run() -> Result<()> {
    let client = reqwest::Client::new();

    // 1. init：拿二维码 token 与 x-flow-key。
    let init_resp = client
        .post(INIT_URL)
        .headers(qr_headers())
        .json(&serde_json::json!({ "biz_type": null, "redirect_uri": "https://www.feishu.cn" }))
        .send()
        .await
        .context("请求二维码 init 失败")?;
    let flow_key = init_resp
        .headers()
        .get("x-flow-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let init_body: serde_json::Value = init_resp.json().await.context("解析 init 响应失败")?;
    if init_body["code"].as_i64() != Some(0) {
        anyhow::bail!("二维码 init 返回错误: {}", init_body);
    }
    let token = init_body["data"]["step_info"]["token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("init 响应缺少 token"))?;

    // 2. 渲染二维码（内容是 token，扫码后在手机上确认）。
    let qr_content = format!("{{\"qrlogin\":{{\"token\":\"{token}\"}}}}");
    print_qr(&qr_content)?;
    println!("请用飞书 App 扫描上方二维码并在手机上确认登录…");

    // 3. 轮询（最多 180 秒）。
    for _ in 0..180 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut h = qr_headers();
        if !flow_key.is_empty() {
            if let Ok(v) = HeaderValue::from_str(&flow_key) {
                h.insert("x-flow-key", v);
            }
        }
        let resp = client
            .post(POLL_URL)
            .headers(h)
            .json(&serde_json::json!({ "biz_type": null }))
            .send()
            .await
            .context("轮询登录状态失败")?;

        let cookie_session = extract_session(resp.headers());
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        let data = &body["data"];
        let status = data["step_info"]["status"].as_i64();
        let next_step = data["next_step"].as_str();

        match (next_step, status) {
            (Some("enter_app"), _) | (_, Some(4)) => {
                // 已确认——提取 session。
                let session = cookie_session
                    .ok_or_else(|| anyhow::anyhow!("登录已确认但响应中未找到 session cookie"))?;
                save_session(&session)?;
                println!("✅ 登录成功，session 已保存到 ~/.lark_cli/session");
                return Ok(());
            }
            (_, Some(2)) => println!("已扫描，等待手机确认…"),
            (_, Some(3)) => anyhow::bail!("已在手机上取消登录"),
            (_, Some(5)) => anyhow::bail!("二维码已过期，请重新运行 smelt feishu login"),
            _ => {}
        }
    }
    anyhow::bail!("登录超时（180 秒未确认）")
}

/// 终端渲染二维码（反色以适配深色终端）。
fn print_qr(content: &str) -> Result<()> {
    use qrcode::render::unicode;
    let code = qrcode::QrCode::new(content.as_bytes()).context("生成二维码失败")?;
    let img = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .build();
    println!("{img}");
    Ok(())
}

/// 从 Set-Cookie 头里提取 session 值。
fn extract_session(headers: &HeaderMap) -> Option<String> {
    for v in headers.get_all("set-cookie") {
        let Ok(s) = v.to_str() else { continue };
        // 形如：session=<value>; Path=/; ...
        for part in s.split(';') {
            let part = part.trim();
            if let Some(val) = part.strip_prefix("session=") {
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// 写入 ~/.lark_cli/session（0600），与 lark_tools 共享同一份登录态。
fn save_session(value: &str) -> Result<()> {
    let dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("无法定位 home 目录"))?
        .join(".lark_cli");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("session");
    std::fs::write(&path, value).with_context(|| format!("写入 {:?} 失败", path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_session_from_set_cookie() {
        let mut h = HeaderMap::new();
        h.append("set-cookie", HeaderValue::from_static("foo=bar; Path=/"));
        h.append(
            "set-cookie",
            HeaderValue::from_static("session=abc123; Path=/; HttpOnly"),
        );
        assert_eq!(extract_session(&h).as_deref(), Some("abc123"));
    }

    #[test]
    fn extract_session_none_when_absent() {
        let mut h = HeaderMap::new();
        h.append("set-cookie", HeaderValue::from_static("foo=bar"));
        assert_eq!(extract_session(&h), None);
    }
}
