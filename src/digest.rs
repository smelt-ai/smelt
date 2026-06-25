//! digest：读取 shell 历史，调用 DeepSeek API 提炼 instinct 并入库，最后刷新 global.md。
//! 发送前会过滤掉可能含敏感信息（密钥/token/密码等）的行。

use crate::claude;
use crate::db;
use crate::git;
use crate::model::{Instinct, Scope};
use crate::render;
use crate::scan;
use anyhow::{Context, Result};
use serde::Deserialize;

const MODEL: &str = "deepseek-chat";
const API_URL: &str = "https://api.deepseek.com/chat/completions";
const MAX_TOKENS: u32 = 2048;

#[derive(Debug, Deserialize)]
struct RawInstinct {
    content: String,
    confidence: f32,
    domain: Vec<String>,
}

fn read_history(n: usize) -> Result<String> {
    let path = if let Ok(f) = std::env::var("SMELT_HISTORY_FILE") {
        std::path::PathBuf::from(f)
    } else {
        dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("无法定位 home 目录"))?
            .join(".zsh_history")
    };
    let bytes = std::fs::read(&path).with_context(|| format!("读取 {:?} 失败", path))?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].join("\n"))
}

/// 发送前的隐私过滤：剔除疑似含敏感信息的行，返回 (过滤后文本, 被剔除行数)。
pub(crate) fn sanitize(history: &str) -> (String, usize) {
    const NEEDLES: &[&str] = &[
        "key", "token", "secret", "password", "passwd", "credential",
        "bearer", "authorization", "api_key", "apikey", "access_key",
        "private", "passphrase",
        "sk-", "ghp_", "gho_", "github_pat_", "xox", "akia", "asia",
        "aws_secret", "-----begin", "ssh-rsa", "ssh-ed25519",
    ];
    let mut kept = Vec::new();
    let mut removed = 0usize;
    for line in history.lines() {
        let low = line.to_lowercase();
        if NEEDLES.iter().any(|n| low.contains(n)) {
            removed += 1;
        } else {
            kept.push(line);
        }
    }
    (kept.join("\n"), removed)
}

pub(crate) fn api_key() -> Result<String> {
    if let Ok(k) = std::env::var("DEEPSEEK_API_KEY") {
        if !k.is_empty() {
            return Ok(k);
        }
    }
    let cfg = db::smelt_dir()?.join("config.toml");
    let text = std::fs::read_to_string(&cfg)
        .with_context(|| format!("读取 {:?} 失败，且未设置 DEEPSEEK_API_KEY", cfg))?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("DEEPSEEK_API_KEY") {
            if let Some(eq) = rest.split('=').nth(1) {
                return Ok(eq.trim().trim_matches('"').to_string());
            }
        }
    }
    anyhow::bail!("config.toml 中未找到 DEEPSEEK_API_KEY")
}

pub(crate) fn stable_id(content: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in content.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

pub(crate) async fn chat(key: &str, prompt: &str) -> Result<String> {
    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "stream": false,
        "messages": [{ "role": "user", "content": prompt }]
    });
    let client = reqwest::Client::new();
    let resp = client
        .post(API_URL)
        .bearer_auth(key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("请求 DeepSeek API 失败")?;
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.context("解析 API 响应失败")?;
    if !status.is_success() {
        anyhow::bail!("DeepSeek API 返回错误 {}: {}", status, json);
    }
    let text = json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("API 响应缺少 message.content 字段: {}", json))?;
    Ok(text.to_string())
}

pub async fn run() -> Result<()> {
    let raw = read_history(200)?;
    let (history, removed) = sanitize(&raw);
    if removed > 0 {
        println!("🔒 已过滤 {} 行可能含敏感信息的历史，不会发送。", removed);
    }
    if history.trim().is_empty() {
        println!("（过滤后）历史为空，跳过。");
        return Ok(());
    }
    let key = api_key()?;

    // 第二个数据源：最近改动的文件（仅元数据）。未配置扫描目录时为空，行为不变。
    let scan_block = match scan::recent_files_report() {
        Ok(Some(r)) => format!("\n\n=== 最近改动的文件 ===\n{r}"),
        Ok(None) => String::new(),
        Err(e) => {
            eprintln!("扫描最近文件失败（已忽略）: {e:#}");
            String::new()
        }
    };
    // 第三个数据源：Claude Code 会话里的提问（反映 AI 协作习惯）。
    let session_block = match claude::recent_prompts_report() {
        Ok(Some(r)) => format!("\n\n=== Claude Code 会话提问 ===\n{r}"),
        Ok(None) => String::new(),
        Err(e) => {
            eprintln!("读取会话历史失败（已忽略）: {e:#}");
            String::new()
        }
    };

    // 第四个数据源：git 提交行为（反映编码工作流与提交习惯）。
    let git_block = match git::recent_activity_report() {
        Ok(Some(r)) => format!("\n\n=== Git 提交行为 ===\n{r}"),
        Ok(None) => String::new(),
        Err(e) => {
            eprintln!("读取 git 行为失败（已忽略）: {e:#}");
            String::new()
        }
    };

    // 动态拼接来源说明，新增数据源时只需在这里加一行。
    let mut sources = vec!["shell 命令历史"];
    if !scan_block.is_empty() {
        sources.push("最近改动的文件");
    }
    if !session_block.is_empty() {
        sources.push("Claude Code 里的提问");
    }
    if !git_block.is_empty() {
        sources.push("git 提交行为");
    }

    let prompt = format!(
        "下面是我最近的{sources}。请综合提炼出 3-5 条关于我编码 / 工作习惯的 instinct。\n\
         每条要具体、可操作。只返回 JSON 数组，每个元素形如 \
         {{\"content\": \"...\", \"confidence\": 0.3-0.9 的小数, \"domain\": [\"领域标签\"]}}。\n\
         不要输出 JSON 以外的任何内容。\n\n\
         === shell 历史 ===\n{history}{scan_block}{session_block}{git_block}",
        sources = sources.join("、")
    );
    let text = chat(&key, &prompt).await?;
    let raws = parse_instincts(&text)?;
    println!("提炼出 {} 条 instinct。", raws.len());

    let conn = db::open()?;
    let now = chrono::Utc::now().to_rfc3339();
    for r in &raws {
        let confidence = r.confidence.clamp(0.3, 0.9);
        let it = Instinct {
            id: stable_id(&r.content),
            content: r.content.clone(),
            confidence,
            domain: r.domain.clone(),
            evidence_count: 1,
            last_seen: now.clone(),
            scope: Scope::Global,
            project: None,
        };
        db::upsert(&conn, &it)?;
        println!("  [{:.2}] {}", confidence, r.content);
    }
    let path = render::write_global()?;
    println!("已更新 {:?}", path);

    // 项目级提炼：对每个活跃 repo 单独提炼该项目特有的 instinct（Scope::Project）。
    for repo in git::per_repo().into_iter().take(5) {
        let commits = repo.commits.join("\n");
        let prompt = format!(
            "下面是我在「{}」这个 git 项目里最近的提交。请提炼 2-3 条**该项目特有**的工作 / 编码习惯 instinct，\
             不要泛泛而谈。只返回 JSON 数组，元素形如 \
             {{\"content\":\"...\",\"confidence\":0.3-0.9 的小数,\"domain\":[\"标签\"]}}。\n\n=== 提交 ===\n{commits}",
            repo.name
        );
        match chat(&key, &prompt).await.and_then(|t| parse_instincts(&t)) {
            Ok(raws) => {
                for r in &raws {
                    let it = Instinct {
                        id: stable_id(&format!("{}::{}", repo.name, r.content)),
                        content: r.content.clone(),
                        confidence: r.confidence.clamp(0.3, 0.9),
                        domain: r.domain.clone(),
                        evidence_count: 1,
                        last_seen: now.clone(),
                        scope: Scope::Project,
                        project: Some(repo.name.clone()),
                    };
                    db::upsert(&conn, &it)?;
                }
                println!("  [项目 {}] 提炼 {} 条", repo.name, raws.len());
            }
            Err(e) => eprintln!("项目 {} 提炼失败（已忽略）: {e:#}", repo.name),
        }
    }
    let pj_paths = render::write_projects()?;
    if !pj_paths.is_empty() {
        println!("已更新 {} 个项目级 instinct 文件", pj_paths.len());
    }
    Ok(())
}

fn parse_instincts(text: &str) -> Result<Vec<RawInstinct>> {
    let start = text.find('[').ok_or_else(|| anyhow::anyhow!("响应中无 JSON 数组: {text}"))?;
    let end = text.rfind(']').ok_or_else(|| anyhow::anyhow!("响应中无 JSON 数组: {text}"))?;
    let slice = &text[start..=end];
    let raws: Vec<RawInstinct> =
        serde_json::from_str(slice).with_context(|| format!("解析 instinct JSON 失败: {slice}"))?;
    Ok(raws)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_id_is_deterministic() {
        assert_eq!(stable_id("cargo build"), stable_id("cargo build"));
        assert_ne!(stable_id("cargo build"), stable_id("cargo test"));
        assert_eq!(stable_id("cargo build").len(), 16);
    }

    #[test]
    fn parse_plain_json_array() {
        let txt = r#"[{"content":"用 anyhow","confidence":0.8,"domain":["rust"]}]"#;
        let v = parse_instincts(txt).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].content, "用 anyhow");
    }

    #[test]
    fn parse_markdown_fenced_json() {
        let txt = "这是结果：\n```json\n[{\"content\":\"a\",\"confidence\":0.5,\"domain\":[]}]\n```\n完毕";
        let v = parse_instincts(txt).unwrap();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn parse_without_array_errors() {
        assert!(parse_instincts("没有任何数组").is_err());
    }

    #[test]
    fn sanitize_strips_sensitive_lines() {
        let raw = "cargo build\nexport DEEPSEEK_API_KEY=sk-abc123\nls -la\ncurl -H \"Authorization: Bearer x\"\ngit status";
        let (clean, removed) = sanitize(raw);
        assert_eq!(removed, 2);
        assert!(clean.contains("cargo build"));
        assert!(clean.contains("git status"));
        assert!(!clean.contains("sk-abc123"));
        assert!(!clean.to_lowercase().contains("authorization"));
    }
}
