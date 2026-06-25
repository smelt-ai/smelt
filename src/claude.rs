//! claude：读取 Claude Code 的本地会话历史（~/.claude/projects/*/*.jsonl），
//! 提取「我真正发出的指令/提问」，作为分身数据源——反映你和 AI 协作的风格与关注点。
//! 只取用户文本，跳过工具结果与系统注入；纯本地，零授权。

use crate::digest;
use anyhow::Result;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::SystemTime;

/// 只读最近这么多个会话文件（按修改时间）。
const MAX_SESSIONS: usize = 20;
/// 汇总进报告的提问条数上限。
const MAX_PROMPTS: usize = 80;
/// 单条提问压成一行后的最大长度。
const MAX_PROMPT_LEN: usize = 300;

/// 一条用户提问：时间戳、所属项目、单行文本。
struct Prompt {
    ts: String,
    project: String,
    text: String,
}

/// `smelt sessions` 子命令：预览分身从 Claude Code 会话里提取到的提问。
pub fn run() -> Result<()> {
    match recent_prompts_report()? {
        Some(report) => print!("{report}"),
        None => println!("未从 Claude Code 会话中提取到提问（~/.claude/projects 为空？）。"),
    }
    Ok(())
}

/// 对外入口：生成会话提问报告；无内容时返回 None。
pub fn recent_prompts_report() -> Result<Option<String>> {
    let files = session_files();
    let mut prompts: Vec<Prompt> = Vec::new();
    for f in files {
        collect_prompts(&f, &mut prompts);
    }
    Ok(render(prompts, MAX_PROMPTS))
}

/// 收集最近修改的会话文件。
fn session_files() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let root = home.join(".claude/projects");
    let mut files: Vec<(PathBuf, SystemTime)> = Vec::new();

    let Ok(projects) = std::fs::read_dir(&root) else { return Vec::new() };
    for proj in projects.flatten() {
        if !proj.path().is_dir() {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(proj.path()) else { continue };
        for f in rd.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(mtime) = f.metadata().and_then(|m| m.modified()) {
                files.push((path, mtime));
            }
        }
    }
    files.sort_by(|a, b| b.1.cmp(&a.1));
    files.truncate(MAX_SESSIONS);
    files.into_iter().map(|(p, _)| p).collect()
}

/// 逐行解析一个会话文件，提取真实用户提问。
fn collect_prompts(path: &PathBuf, out: &mut Vec<Prompt>) {
    let Ok(file) = std::fs::File::open(path) else { return };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if v["type"] != "user" {
            continue;
        }
        // content 为字符串才是用户打的字（工具结果是数组）。
        let Some(raw) = v["message"]["content"].as_str() else { continue };
        if !is_real_prompt(raw) {
            continue;
        }
        // 复用 digest 的敏感词表：含疑似密钥的提问整条跳过。
        if digest::sanitize(raw).1 > 0 {
            continue;
        }
        let text = one_line(raw, MAX_PROMPT_LEN);
        if text.is_empty() {
            continue;
        }
        let project = v["cwd"]
            .as_str()
            .and_then(|c| c.rsplit('/').next())
            .unwrap_or("?")
            .to_string();
        let ts = v["timestamp"].as_str().unwrap_or("").to_string();
        out.push(Prompt { ts, project, text });
    }
}

/// 判断是否为真实用户提问（排除空、系统注入、中断标记）。
fn is_real_prompt(raw: &str) -> bool {
    let s = raw.trim_start();
    !(s.is_empty()
        || s.starts_with('<') // <command-...>, <system-reminder>, <task-notification> 等
        || s.starts_with("Caveat:")
        || s.starts_with("[Request"))
}

/// 把多行文本压成一行并截断。
fn one_line(raw: &str, max: usize) -> String {
    let joined = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > max {
        joined.chars().take(max).collect::<String>() + "…"
    } else {
        joined
    }
}

/// 渲染报告：按时间倒序，列出最近的用户提问。
fn render(mut prompts: Vec<Prompt>, limit: usize) -> Option<String> {
    if prompts.is_empty() {
        return None;
    }
    prompts.sort_by(|a, b| b.ts.cmp(&a.ts));
    prompts.truncate(limit);

    let mut out = String::from(
        "下面是我最近在 Claude Code 里发出的指令/提问（反映我和 AI 协作的风格与关注点）。\n",
    );
    for p in &prompts {
        out.push_str(&format!("- [{}] {}\n", p.project, p.text));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_real_prompt_filters_injections() {
        assert!(is_real_prompt("帮我加个功能"));
        assert!(!is_real_prompt("<task-notification>"));
        assert!(!is_real_prompt("<command-name>/foo</command-name>"));
        assert!(!is_real_prompt("Caveat: The messages below..."));
        assert!(!is_real_prompt("[Request interrupted by user]"));
        assert!(!is_real_prompt("   "));
    }

    #[test]
    fn one_line_collapses_and_truncates() {
        assert_eq!(one_line("a\n  b\t c", 100), "a b c");
        let long = "x".repeat(50);
        let r = one_line(&long, 10);
        assert_eq!(r.chars().count(), 11); // 10 + 省略号
        assert!(r.ends_with('…'));
    }

    #[test]
    fn render_sorts_recent_first() {
        let prompts = vec![
            Prompt { ts: "2026-01-01T00:00:00Z".into(), project: "a".into(), text: "旧".into() },
            Prompt { ts: "2026-06-01T00:00:00Z".into(), project: "b".into(), text: "新".into() },
        ];
        let r = render(prompts, 10).unwrap();
        let pnew = r.find("新").unwrap();
        let pold = r.find("旧").unwrap();
        assert!(pnew < pold, "最近的提问应排在前面");
        assert!(r.contains("[b]"));
    }

    #[test]
    fn empty_yields_none() {
        assert!(render(vec![], 10).is_none());
    }
}
