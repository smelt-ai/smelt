//! digest：读取 shell 历史，调用 Claude API 提炼 instinct 并入库。

use crate::db;
use crate::model::{Instinct, Scope};
use anyhow::{Context, Result};
use serde::Deserialize;

/// 使用的 Claude 模型（skill 推荐默认；如需省钱可换 claude-sonnet-4-6）。
const MODEL: &str = "claude-opus-4-8";
const API_URL: &str = "https://api.anthropic.com/v1/messages";
/// 留足空间避免 JSON 数组被截断（stop_reason: "max_tokens"）。
const MAX_TOKENS: u32 = 2048;

/// Claude 返回的单条 instinct（API JSON 结构）。
#[derive(Debug, Deserialize)]
struct RawInstinct {
    content: String,
    confidence: f32,
    domain: Vec<String>,
}

/// 读取 ~/.zsh_history 最近 `n` 行。
fn read_history(n: usize) -> Result<String> {
    let path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("无法定位 home 目录"))?
        .join(".zsh_history");
    // zsh_history 可能含非 UTF-8 字节，按 lossy 读取。
    let bytes = std::fs::read(&path).with_context(|| format!("读取 {:?} 失败", path))?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].join("\n"))
}

/// 从环境变量或 ~/.smelt/config.toml 读取 API key。
fn api_key() -> Result<String> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.is_empty() {
            return Ok(k);
        }
    }
    let cfg = db::smelt_dir()?.join("config.toml");
    let text = std::fs::read_to_string(&cfg)
        .with_context(|| format!("读取 {:?} 失败，且未设置 ANTHROPIC_API_KEY", cfg))?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ANTHROPIC_API_KEY") {
            if let Some(eq) = rest.split('=').nth(1) {
                return Ok(eq.trim().trim_matches('"').to_string());
            }
        }
    }
    anyhow::bail!("config.toml 中未找到 ANTHROPIC_API_KEY")
}

/// 执行一次蒸馏。
pub async fn run() -> Result<()> {
    let history = read_history(200)?;
    if history.trim().is_empty() {
        println!("shell 历史为空，跳过。");
        return Ok(());
    }
    let key = api_key()?;
    let raws = call_claude(&key, &history).await?;
    println!("提炼出 {} 条 instinct。", raws.len());

    let conn = db::open()?;
    let now = chrono::Utc::now().to_rfc3339();
    for r in &raws {
        let confidence = r.confidence.clamp(0.3, 0.9);
        let id = stable_id(&r.content);
        let it = Instinct {
            id,
            content: r.content.clone(),
            confidence,
            domain: r.domain.clone(),
            evidence_count: 1,
            last_seen: now.clone(),
            scope: Scope::Global,
        };
        db::upsert(&conn, &it)?;
        println!("  [{:.2}] {}", confidence, r.content);
    }
    Ok(())
}

/// 基于内容生成稳定 id（FNV-1a 哈希的十六进制）。
fn stable_id(content: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in content.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

/// 调用 Claude API，返回提炼出的 instinct 列表。
async fn call_claude(key: &str, history: &str) -> Result<Vec<RawInstinct>> {
    let prompt = format!(
        "下面是我最近的 shell 命令历史。请提炼出 3-5 条关于我编码 / 工作习惯的 instinct。\n\
         每条要具体、可操作。只返回 JSON 数组，每个元素形如 \
         {{\"content\": \"...\", \"confidence\": 0.3-0.9 的小数, \"domain\": [\"领域标签\"]}}。\n\
         不要输出 JSON 以外的任何内容。\n\n\
         === shell 历史 ===\n{history}"
    );

    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "messages": [{ "role": "user", "content": prompt }]
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(API_URL)
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("请求 Claude API 失败")?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().await.context("解析 API 响应失败")?;
    if !status.is_success() {
        anyhow::bail!("Claude API 返回错误 {}: {}", status, json);
    }

    // 取 content[0].text
    let text = json["content"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("API 响应缺少 text 字段: {}", json))?;

    parse_instincts(text)
}

/// 从模型文本中提取 JSON 数组并反序列化。
fn parse_instincts(text: &str) -> Result<Vec<RawInstinct>> {
    let start = text
        .find('[')
        .ok_or_else(|| anyhow::anyhow!("响应中无 JSON 数组: {text}"))?;
    let end = text
        .rfind(']')
        .ok_or_else(|| anyhow::anyhow!("响应中无 JSON 数组: {text}"))?;
    let slice = &text[start..=end];
    let raws: Vec<RawInstinct> =
        serde_json::from_str(slice).with_context(|| format!("解析 instinct JSON 失败: {slice}"))?;
    Ok(raws)
}
