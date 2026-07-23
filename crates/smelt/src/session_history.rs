//! 历史会话浏览：列出某个项目下各家 agent CLI 本地保存的历史会话（Claude Code /
//! Codex / Grok / Copilot 各有自己的存储格式，四份独立实现，共用同一套
//! `SessionSummary`/`Turn`/`SessionDetail` 展示模型），点开能看完整对话内容（只读
//! 浏览，不支持 resume——续接走 ACP 协议本身，见 acp.rs）。跟 usage_stats.rs 读的
//! 是同一份 Claude 数据源，但目的不同——那边统计聚合数字，这里还原对话本身。
//!
//! 四家格式调研自实测（各 CLI 版本可能变，这些解析都是「尽力而为」，不是协议）：
//! - Claude: `~/.claude/projects/<项目目录编码>/<session_id>.jsonl`
//! - Codex: `~/.codex/sessions/<年>/<月>/<日>/rollout-*.jsonl`（按日期分区，不按项目）
//! - Grok: `~/.grok/sessions/<url编码cwd>/<session_id>/`（`summary.json` + `chat_history.jsonl`）
//! - Copilot: `~/.copilot/session-state/<session_id>/`（`workspace.yaml` + `events.jsonl`）

use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
}

/// 项目目录编码规则：Claude Code 把项目路径里的 `/` 和 `.` 都换成 `-`
/// （已经拿 codux 的实现 `project_path.replace('/', '-').replace('.', '-')` 印证过，
/// 跟本机实测的编码目录名完全对得上）。
fn project_dir(cwd: &str) -> String {
    cwd.replace('/', "-").replace('.', "-")
}

/// 某个会话的 transcript 文件路径（`<项目目录>/<会话 id>.jsonl`）。
///
/// ACP 的会话 id 就是 Claude Code 的 transcript 文件名（实测印证）；这个文件
/// 存在与否 = 这段对话有没有真正落盘 = 续接有没有可能成功。acp.rs 靠它避开
/// 注定失败的 `session/resume`（省下约 2 秒白等）。
pub(crate) fn transcript_path(cwd: &str, session_id: &str) -> PathBuf {
    projects_root().join(project_dir(cwd)).join(format!("{session_id}.jsonl"))
}

fn projects_root() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(".claude").join("projects")
}

/// 某个项目的记忆目录（`<项目目录>/memory`）。编码规则只有这一份，claude_memory.rs
/// 从这里取，别再复制一遍 project_dir——规则一旦变，两处会悄悄不一致。
pub(crate) fn memory_dir(cwd: &str) -> PathBuf {
    projects_root().join(project_dir(cwd)).join("memory")
}

/// 一份历史会话的概览（列表用）。
#[derive(Clone)]
pub struct SessionSummary {
    pub path: PathBuf,
    /// 首条用户消息文本（截断），取不到就回退用 session id（文件名去掉扩展名）。
    pub title: String,
    pub started_at: Option<DateTime<Utc>>,
    pub last_active_at: Option<DateTime<Utc>>,
    /// user + assistant 消息总数（不含被跳过的 tool_result / 内部记录）。
    pub message_count: usize,
    /// 本份会话消耗的 token 总量（input+output+两种 cache 相加，算法跟 usage_stats
    /// 一致），供总览卡片展示「当前会话」口径的用量——跟用量页的整项目累计口径不同。
    pub total_tokens: u64,
    /// 最近一次工具调用名（按文件行序，最后一个 tool_use 块），供总览卡片展示。
    pub last_tool: Option<String>,
}

/// 一轮对话：用户发言 / Claude 回复（含它这轮调用了哪些工具）。
pub struct Turn {
    pub is_user: bool,
    pub timestamp: Option<DateTime<Utc>>,
    pub text: String,
    /// 这轮里 assistant 调用的工具名（user 轮恒为空）。
    pub tools: Vec<String>,
}

pub struct SessionDetail {
    pub turns: Vec<Turn>,
}

/// 列出某个项目目录下的所有历史会话，按最近活跃时间降序。
/// 只读扫描，可能要几十毫秒（视会话数量），调用方应放后台线程跑。
pub fn list_sessions(cwd: &str) -> Vec<SessionSummary> {
    let dir = projects_root().join(project_dir(cwd));
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out: Vec<SessionSummary> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .filter_map(|path| summarize_session(&path))
        .collect();
    out.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at));
    out
}

fn summarize_session(path: &Path) -> Option<SessionSummary> {
    let text = std::fs::read_to_string(path).ok()?;
    let session_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();

    let mut title: Option<String> = None;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut last_active_at: Option<DateTime<Utc>> = None;
    let mut message_count = 0usize;
    let mut total_tokens = 0u64;
    let mut last_tool: Option<String> = None;
    let mut seen_uuids: HashSet<String> = HashSet::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
        let Some(kind) = row.get("type").and_then(|v| v.as_str()) else { continue };
        if kind != "user" && kind != "assistant" {
            continue;
        }
        let ts = row
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        if let Some(ts) = ts {
            started_at = Some(started_at.map_or(ts, |s: DateTime<Utc>| s.min(ts)));
            last_active_at = Some(last_active_at.map_or(ts, |l: DateTime<Utc>| l.max(ts)));
        }

        if kind == "user" {
            // content 是纯字符串才算真实用户发言；数组形态是 tool_result 回填，不计数、不当标题。
            if let Some(text) = row.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                message_count += 1;
                if title.is_none() && !text.trim().is_empty() {
                    title = Some(truncate(text.trim(), 80));
                }
            }
        } else {
            // assistant：content 数组里只要有 text 块就算一条消息；同 uuid 只算一次
            // （日志重写/追加异常会重复），token 累加算法跟 usage_stats 保持一致。
            let dup = row
                .get("uuid")
                .and_then(|v| v.as_str())
                .is_some_and(|u| !seen_uuids.insert(u.to_string()));
            let blocks = row.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array());
            let has_text = blocks
                .is_some_and(|blocks| blocks.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("text")));
            if has_text {
                message_count += 1;
            }
            if let Some(blocks) = blocks {
                for b in blocks {
                    if b.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        if let Some(name) = b.get("name").and_then(|n| n.as_str()) {
                            last_tool = Some(name.to_string());
                        }
                    }
                }
            }
            if !dup {
                if let Some(usage) = row.get("message").and_then(|m| m.get("usage")) {
                    let field = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                    total_tokens += field("input_tokens")
                        + field("output_tokens")
                        + field("cache_creation_input_tokens")
                        + field("cache_read_input_tokens");
                }
            }
        }
    }

    Some(SessionSummary {
        title: title.unwrap_or(session_id),
        path: path.to_path_buf(),
        started_at,
        last_active_at,
        message_count,
        total_tokens,
        last_tool,
    })
}

/// 读某一份会话 transcript，还原成 Turn 列表供浏览。
/// 跳过子代理（isSidechain）消息 —— 混进主线对话会话读起来很乱，先不做嵌套展示；
/// 也跳过纯 tool_result 的 user 消息（那是工具输出回填，不是真实用户发言，assistant
/// 轮次里的工具名已经能说明调用了什么）。
pub fn load_session_detail(path: &Path) -> Option<SessionDetail> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut turns = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
        if row.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        let Some(kind) = row.get("type").and_then(|v| v.as_str()) else { continue };
        if kind != "user" && kind != "assistant" {
            continue;
        }
        let timestamp = row
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        let content = row.get("message").and_then(|m| m.get("content"));

        if kind == "user" {
            let Some(text) = content.and_then(|c| c.as_str()) else { continue };
            if text.trim().is_empty() {
                continue;
            }
            turns.push(Turn { is_user: true, timestamp, text: text.to_string(), tools: Vec::new() });
        } else {
            let blocks = content.and_then(|c| c.as_array());
            let Some(blocks) = blocks else { continue };
            let text = blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            let tools: Vec<String> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                .filter_map(|b| b.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect();
            if text.trim().is_empty() && tools.is_empty() {
                continue;
            }
            turns.push(Turn { is_user: false, timestamp, text, tools });
        }
    }

    Some(SessionDetail { turns })
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

// ===================== Codex =====================
//
// `~/.codex/sessions/<年>/<月>/<日>/rollout-*.jsonl`：不像 Claude 按项目分目录，
// 只能按日期分区遍历、逐份看第一行 session_meta 里的 cwd 是否匹配——文件多的话
// 比 Claude 那版慢，调用方本来就放后台线程跑，可以接受。

fn codex_sessions_root() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(".codex").join("sessions")
}

pub fn list_codex_sessions(cwd: &str) -> Vec<SessionSummary> {
    let root = codex_sessions_root();
    let mut out = Vec::new();
    for year in read_dir_ok(&root) {
        for month in read_dir_ok(&year) {
            for day in read_dir_ok(&month) {
                for path in read_dir_ok(&day) {
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    if let Some(s) = summarize_codex_session(&path, cwd) {
                        out.push(s);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at));
    out
}

fn read_dir_ok(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|it| it.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// Codex 的 `response_item.payload.type=="message"` 里，`role=="user"` 的第一条
/// 常常不是人打的字，是 CLI 自己注入的 `<environment_context>…</environment_context>`
/// ——拿这个当标题会很怪，跟真实问题一样都用尖括号开头这个弱信号过滤掉。
/// 实测这个弱信号会漏（比如 IDE 插件注入的 `# Context from my IDE setup:` 是
/// `#` 开头，不是 `<`）——协议没有专门的「这条是合成的」标记，只能靠外观猜，
/// 猜不准的会在真人对话里多出几条奇怪的「用户消息」，暂时接受。
fn is_synthetic_codex_text(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('<') || t.starts_with("# Context from")
}

fn summarize_codex_session(path: &Path, want_cwd: &str) -> Option<SessionSummary> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let first = lines.next()?.trim();
    let meta: Value = serde_json::from_str(first).ok()?;
    if meta.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
        return None;
    }
    let payload = meta.get("payload")?;
    if payload.get("cwd").and_then(|v| v.as_str()) != Some(want_cwd) {
        return None; // 先过滤 cwd，不匹配就不用往下解析整份文件
    }
    let session_id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

    let mut title: Option<String> = None;
    let started_at = meta.get("timestamp").and_then(|v| v.as_str()).and_then(parse_rfc3339);
    let mut last_active_at = started_at;
    let mut message_count = 0usize;
    let mut last_tool: Option<String> = None;

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(ts) = row.get("timestamp").and_then(|v| v.as_str()).and_then(parse_rfc3339) {
            last_active_at = Some(last_active_at.map_or(ts, |l: DateTime<Utc>| l.max(ts)));
        }
        if row.get("type").and_then(|v| v.as_str()) != Some("response_item") {
            continue;
        }
        let Some(item) = row.get("payload") else { continue };
        match item.get("type").and_then(|v| v.as_str()) {
            Some("message") => {
                let Some(msg_text) = codex_message_text(item) else { continue };
                // role 不只有 user/assistant——实测还见过 system/developer 这类指令性
                // 角色（比如 `<permissions instructions>` 说明块）。只认 user/assistant，
                // 别的一律跳过：归到 assistant 会显示成「AI 说了这段系统指令」，误导人。
                let is_user = match item.get("role").and_then(|v| v.as_str()) {
                    Some("user") => true,
                    Some("assistant") => false,
                    _ => continue,
                };
                // 合成的 <environment_context> 用户消息不计入消息数——跟
                // load_codex_session_detail 里跳过它是同一条口径，不然列表页显示的
                // 数字会比点开详情页实际看到的轮次还多，对不上。
                if is_user && is_synthetic_codex_text(&msg_text) {
                    continue;
                }
                message_count += 1;
                if is_user && title.is_none() {
                    title = Some(truncate(msg_text.trim(), 80));
                }
            }
            Some("function_call") => {
                if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                    last_tool = Some(name.to_string());
                }
            }
            _ => {}
        }
    }

    Some(SessionSummary {
        path: path.to_path_buf(),
        title: title.unwrap_or(session_id),
        started_at,
        last_active_at,
        message_count,
        // Codex 的 event_msg.token_count 是「速率限制用量占比」，不是这一份会话的
        // token 总数，跟 Claude 那份口径对不上，宁可不接也不接一个会误导人的数字。
        total_tokens: 0,
        last_tool,
    })
}

fn codex_message_text(payload: &Value) -> Option<String> {
    let blocks = payload.get("content")?.as_array()?;
    let text = blocks
        .iter()
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

pub fn load_codex_session_detail(path: &Path) -> Option<SessionDetail> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut turns: Vec<Turn> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
        if row.get("type").and_then(|v| v.as_str()) != Some("response_item") {
            continue;
        }
        let timestamp = row.get("timestamp").and_then(|v| v.as_str()).and_then(parse_rfc3339);
        let Some(item) = row.get("payload") else { continue };
        match item.get("type").and_then(|v| v.as_str()) {
            Some("message") => {
                let Some(msg_text) = codex_message_text(item) else { continue };
                let is_user = match item.get("role").and_then(|v| v.as_str()) {
                    Some("user") => true,
                    Some("assistant") => false,
                    _ => continue, // system/developer 等指令角色，不是真实对话轮次
                };
                if is_user && is_synthetic_codex_text(&msg_text) {
                    continue; // CLI 自己注入的 <environment_context>，不是真人发言
                }
                turns.push(Turn { is_user, timestamp, text: msg_text, tools: Vec::new() });
            }
            Some("function_call") => {
                let Some(name) = item.get("name").and_then(|v| v.as_str()) else { continue };
                // 工具调用挂到「上一条 assistant 轮次」上——Codex 的日志比 Claude 更碎，
                // 一次 assistant 发言常拆成「先一条 message 说要干嘛，再几条 function_call」，
                // 没有上一条 assistant 轮次就单独开一条只带工具名、没有正文的轮次。
                match turns.last_mut() {
                    Some(t) if !t.is_user => t.tools.push(name.to_string()),
                    _ => turns.push(Turn {
                        is_user: false,
                        timestamp,
                        text: String::new(),
                        tools: vec![name.to_string()],
                    }),
                }
            }
            _ => {}
        }
    }

    Some(SessionDetail { turns })
}

// ===================== Grok =====================
//
// `~/.grok/sessions/<url编码cwd>/<session_id>/`：`summary.json` 已经现成给了标题/
// 时间/消息数（不用像 Claude/Codex 那样扫整份 transcript 才能拿到概览，列表这块
// 反而是四家里最快的），`chat_history.jsonl` 才是完整对话内容。

fn grok_sessions_root() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(".grok").join("sessions")
}

pub fn list_grok_sessions(cwd: &str) -> Vec<SessionSummary> {
    let root = grok_sessions_root();
    let mut out = Vec::new();
    for project_dir in read_dir_ok(&root) {
        if !project_dir.is_dir() {
            continue; // 跳过同级的 session_search.sqlite
        }
        for session_dir in read_dir_ok(&project_dir) {
            if let Some(s) = summarize_grok_session(&session_dir, cwd) {
                out.push(s);
            }
        }
    }
    out.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at));
    out
}

fn summarize_grok_session(session_dir: &Path, want_cwd: &str) -> Option<SessionSummary> {
    let summary_path = session_dir.join("summary.json");
    let text = std::fs::read_to_string(&summary_path).ok()?;
    let summary: Value = serde_json::from_str(&text).ok()?;
    if summary.get("info").and_then(|i| i.get("cwd")).and_then(|v| v.as_str()) != Some(want_cwd) {
        return None;
    }
    let session_id =
        session_dir.file_name().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
    let title = summary
        .get("session_summary")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| truncate(s.trim(), 80))
        .unwrap_or(session_id);
    Some(SessionSummary {
        path: session_dir.to_path_buf(),
        title,
        started_at: summary.get("created_at").and_then(|v| v.as_str()).and_then(parse_rfc3339),
        last_active_at: summary.get("updated_at").and_then(|v| v.as_str()).and_then(parse_rfc3339),
        message_count: summary
            .get("num_chat_messages")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        // summary.json 没有 token 统计字段（实测），跟 Codex 一样宁可留空。
        total_tokens: 0,
        last_tool: None, // 要扫 chat_history.jsonl 才能拿到，摘要行不为这一列多付这个成本
    })
}

/// Grok 把 IDE 环境信息 / 项目说明这类系统注入内容也存成 `type:"user"`。多数带
/// `synthetic_reason` 字段（如 `"compaction_meta"`/`"project_instructions"`）能直接
/// 识别；但实测第一轮的 `<user_info>…</user_info>` 环境块不带这个字段（大概是
/// CLI 认为它是「第一轮正常内容的一部分」而不是「事后注入」），得再兜底一层：
/// 剥掉 `<user_query>` 包装后文本仍然是尖括号开头，说明这不是真实问题、是别的
/// 原始上下文块，同样当合成消息跳过。
fn is_synthetic_grok_row(row: &Value, extracted_text: &str) -> bool {
    row.get("synthetic_reason").is_some() || extracted_text.trim_start().starts_with('<')
}

fn grok_text_blocks(content: &Value) -> String {
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// 真人问题外面常包一层 `<user_query>…</user_query>`（CLI 自己加的），原样显示会
/// 让消息气泡里露出 XML 标签，剥掉更贴近「这就是用户打的字」。
fn strip_user_query_wrapper(text: &str) -> &str {
    let t = text.trim();
    let Some(rest) = t.strip_prefix("<user_query>") else { return text };
    rest.strip_suffix("</user_query>").map(str::trim).unwrap_or(text)
}

pub fn load_grok_session_detail(session_dir: &Path) -> Option<SessionDetail> {
    let path = session_dir.join("chat_history.jsonl");
    let text = std::fs::read_to_string(&path).ok()?;
    let mut turns: Vec<Turn> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
        match row.get("type").and_then(|v| v.as_str()) {
            Some("user") => {
                let Some(content) = row.get("content") else { continue };
                let raw = grok_text_blocks(content);
                if raw.trim().is_empty() {
                    continue;
                }
                let text = strip_user_query_wrapper(&raw).to_string();
                if is_synthetic_grok_row(&row, &text) {
                    continue;
                }
                turns.push(Turn { is_user: true, timestamp: None, text, tools: Vec::new() });
            }
            Some("assistant") => {
                let text =
                    row.get("content").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let tools: Vec<String> = row
                    .get("tool_calls")
                    .and_then(|v| v.as_array())
                    .map(|calls| {
                        calls
                            .iter()
                            .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                if text.trim().is_empty() && tools.is_empty() {
                    continue;
                }
                // Grok 的 chat_history.jsonl 逐行不带时间戳（跟 Claude/Codex 不同），
                // 只有整份会话的 created_at/updated_at（见 summary.json），没有更细的
                // 逐轮时间可用，就都留 None，UI 本来就把 None 当「不显示时间」处理。
                turns.push(Turn { is_user: false, timestamp: None, text, tools });
            }
            _ => {} // reasoning / system / tool_result：跳过，同 Claude 对 tool_result 的处理
        }
    }

    Some(SessionDetail { turns })
}

// ===================== Copilot =====================
//
// `~/.copilot/session-state/<session_id>/`：`workspace.yaml`（10 来行的扁平
// `key: value`，没有嵌套/列表，手写小解析器就够，不为这一个文件引入 yaml 依赖）
// 给 cwd/标题/时间，`events.jsonl` 才是完整对话内容。

fn copilot_sessions_root() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(".copilot").join("session-state")
}

/// 只认得住扁平 `key: value` 这一种形状——Copilot 目前这个文件就是这样（实测），
/// 真出现嵌套/列表会直接读不到对应字段，调用方本来就都用 `Option`/回退处理。
fn parse_flat_yaml(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

pub fn list_copilot_sessions(cwd: &str) -> Vec<SessionSummary> {
    let root = copilot_sessions_root();
    let mut out = Vec::new();
    for session_dir in read_dir_ok(&root) {
        if let Some(s) = summarize_copilot_session(&session_dir, cwd) {
            out.push(s);
        }
    }
    out.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at));
    out
}

fn summarize_copilot_session(session_dir: &Path, want_cwd: &str) -> Option<SessionSummary> {
    let yaml_text = std::fs::read_to_string(session_dir.join("workspace.yaml")).ok()?;
    let fields = parse_flat_yaml(&yaml_text);
    if fields.get("cwd").map(String::as_str) != Some(want_cwd) {
        return None;
    }
    let session_id =
        session_dir.file_name().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
    let title = fields
        .get("summary")
        .or_else(|| fields.get("name"))
        .filter(|s| !s.trim().is_empty())
        .map(|s| truncate(s, 80))
        .unwrap_or(session_id);

    // workspace.yaml 没存消息数/最近工具名，要拿这两项就得扫一遍 events.jsonl——
    // 跟 Claude/Codex 列表页同样的代价，不算特殊。
    let (mut message_count, mut last_tool) = (0usize, None);
    if let Ok(text) = std::fs::read_to_string(session_dir.join("events.jsonl")) {
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<Value>(line.trim()) else { continue };
            match row.get("type").and_then(|v| v.as_str()) {
                Some("user.message") | Some("assistant.message") => message_count += 1,
                Some("tool.execution_start") => {
                    if let Some(name) =
                        row.get("data").and_then(|d| d.get("toolName")).and_then(|v| v.as_str())
                    {
                        last_tool = Some(name.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    Some(SessionSummary {
        path: session_dir.to_path_buf(),
        title,
        started_at: fields.get("created_at").and_then(|v| parse_rfc3339(v)),
        last_active_at: fields.get("updated_at").and_then(|v| parse_rfc3339(v)),
        message_count,
        total_tokens: 0, // events.jsonl 没有可靠的整会话 token 汇总字段（实测）
        last_tool,
    })
}

pub fn load_copilot_session_detail(session_dir: &Path) -> Option<SessionDetail> {
    let text = std::fs::read_to_string(session_dir.join("events.jsonl")).ok()?;
    let mut turns: Vec<Turn> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
        let Some(data) = row.get("data") else { continue };
        match row.get("type").and_then(|v| v.as_str()) {
            Some("user.message") => {
                // `content` 是用户原始打字；`transformedContent` 是 CLI 拼进 IDE 选区
                // 之类上下文之后的版本，混进去展示会很乱，只取干净的那份。
                let Some(text) = data.get("content").and_then(|v| v.as_str()) else { continue };
                if text.trim().is_empty() {
                    continue;
                }
                turns.push(Turn {
                    is_user: true,
                    timestamp: None,
                    text: text.to_string(),
                    tools: Vec::new(),
                });
            }
            Some("assistant.message") => {
                let text =
                    data.get("content").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let tools: Vec<String> = data
                    .get("toolRequests")
                    .and_then(|v| v.as_array())
                    .map(|reqs| {
                        reqs.iter()
                            .filter_map(|r| r.get("name").and_then(|n| n.as_str()))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                if text.trim().is_empty() && tools.is_empty() {
                    continue;
                }
                turns.push(Turn { is_user: false, timestamp: None, text, tools });
            }
            _ => {} // tool.execution_*/hook.*/session.*/system.*：跳过，工具名已从 toolRequests 拿到
        }
    }

    Some(SessionDetail { turns })
}

// ===================== GPUI 面板 =====================
//
// 以上是纯逻辑（无 GPUI 依赖，好单测）；以下是从 main.rs 拆过来的面板部分——
// `impl Workspace` 方法 + 渲染函数，字段仍然声明在 main.rs 的 `Workspace` struct 里。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState};
use gpui_component::*;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use crate::claude_memory::MemoryEntry;
use crate::usage_stats::format_count;
use crate::{placeholder_view, Workspace};

/// 历史会话表格「时间」列文案：有明显跨度（>1 分钟）就顺带标一下这个会话跑了多久，
/// 纯单条消息的会话就只显示时间点，不必画蛇添足展示"0 分钟"。
fn session_when(s: &SessionSummary) -> String {
    match (s.started_at, s.last_active_at) {
        (Some(start), Some(last)) if (last - start).num_minutes() >= 1 => format!(
            "{} · 跑了 {} 分钟",
            last.with_timezone(&chrono::Local).format("%m-%d %H:%M"),
            (last - start).num_minutes()
        ),
        (_, Some(last)) => last.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string(),
        _ => String::new(),
    }
}

/// 历史会话表格的数据委托：持有当前项目的会话列表 + 列定义，渲染/排序都在这实现。
pub struct SessionHistoryDelegate {
    pub sessions: Rc<Vec<SessionSummary>>,
    columns: Vec<Column>,
}

impl SessionHistoryDelegate {
    fn new(sessions: Rc<Vec<SessionSummary>>) -> Self {
        Self {
            sessions,
            columns: vec![
                Column::new("title", "标题").width(px(260.)),
                Column::new("when", "时间").width(px(180.)).sortable(),
                Column::new("messages", "消息数").width(px(90.)).sortable(),
                Column::new("tokens", "Tokens").width(px(90.)).sortable(),
            ],
        }
    }
}

impl TableDelegate for SessionHistoryDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.sessions.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let s = &self.sessions[row_ix];
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        match self.columns[col_ix].key.as_ref() {
            "title" => div().text_color(fg).child(s.title.clone()).into_any_element(),
            "when" => div().text_color(muted).child(session_when(s)).into_any_element(),
            "messages" => {
                div().text_color(muted).child(s.message_count.to_string()).into_any_element()
            }
            "tokens" => div().text_color(muted).child(format_count(s.total_tokens)).into_any_element(),
            _ => Empty.into_any_element(),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        let key = self.columns[col_ix].key.clone();
        let rows = Rc::make_mut(&mut self.sessions);
        match (key.as_ref(), sort) {
            ("when", ColumnSort::Ascending) => rows.sort_by_key(|s| s.last_active_at),
            ("when", ColumnSort::Descending) => rows.sort_by_key(|s| std::cmp::Reverse(s.last_active_at)),
            ("messages", ColumnSort::Ascending) => rows.sort_by_key(|s| s.message_count),
            ("messages", ColumnSort::Descending) => rows.sort_by_key(|s| std::cmp::Reverse(s.message_count)),
            ("tokens", ColumnSort::Ascending) => rows.sort_by_key(|s| s.total_tokens),
            ("tokens", ColumnSort::Descending) => rows.sort_by_key(|s| std::cmp::Reverse(s.total_tokens)),
            // Default：不重排，维持 list_sessions 原始顺序（按时间新→旧）。
            _ => {}
        }
    }
}

/// 历史会话列表的三种状态：还没扫描完 / 扫描完但没有历史会话 / 拿到数据（表格 Entity
/// 已经就绪，见 Workspace::ensure_session_table）。
pub enum HistoryListState {
    Loading,
    Empty,
    Ready(Entity<TableState<SessionHistoryDelegate>>),
}

/// 历史会话页的两个子页，共用「左列表 + 右详情」的骨架：
/// - `Sessions`：Claude Code 存的历史对话（`*.jsonl`）
/// - `Memories`：Claude Code 攒的长期记忆（`memory/*.md`，见 claude_memory.rs）
///
/// 两者是同一个目录下的邻居数据，都属于「Claude Code 专属层」。
#[derive(Clone, Copy, PartialEq)]
pub enum HistoryPane {
    Sessions,
    Memories,
}

/// 历史会话页：左侧列出当前项目下 Claude Code 保存的历史会话，右侧显示选中会话的
/// 对话内容（只读浏览，不支持 resume）。数据来自 session_history 模块，跟「用量」
/// 页读的是同一份 `~/.claude/projects/**/*.jsonl`，但这里还原对话本身而非统计聚合。
pub fn history_view(
    pane: HistoryPane,
    agent: AcpAgentKind,
    list: HistoryListState,
    detail: &Option<(std::path::PathBuf, Rc<SessionDetail>)>,
    memories: Option<Rc<Vec<MemoryEntry>>>,
    memory_selected: Option<usize>,
    cx: &mut Context<Workspace>,
) -> Div {
    let (muted, fg, c_border, accent, secondary) = {
        let t = cx.theme();
        (t.muted_foreground, t.foreground, t.border, t.primary, t.secondary)
    };

    // 「会话 / 记忆」切换：两块数据是同一个项目的两种视角，共用下面的左右布局，
    // 所以做成页内切换而不是各占一个顶层 tab。「记忆」目前只有 Claude Code 会写
    // （`~/.claude/.../memory/*.md`），不是四家通用的东西，agent tab 只在「会话」
    // 子页出现。
    let switcher = h_flex()
        .flex_none()
        .gap_1()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(c_border)
        .child(pane_button("会话", HistoryPane::Sessions, pane, accent, fg, muted, cx))
        .child(pane_button("记忆", HistoryPane::Memories, pane, accent, fg, muted, cx));

    if pane == HistoryPane::Memories {
        return v_flex()
            .flex_1()
            .min_h_0()
            .child(switcher)
            .child(memory_body(memories, memory_selected, muted, fg, c_border, accent, cx));
    }

    // 会话来源分 tab：四家 agent 各自的本地存储格式完全不同（见文件头注释），
    // 没法合并成一份列表，只能让用户自己选看哪家。
    let agent_switcher = h_flex()
        .flex_none()
        .gap_1()
        .px_3()
        .py_1p5()
        .border_b_1()
        .border_color(c_border)
        .children(AcpAgentKind::ALL.map(|a| agent_tab_button(a, agent, accent, fg, muted, cx)));

    let list_body: AnyElement = match list {
        HistoryListState::Loading => placeholder_view("加载中…", muted).into_any_element(),
        HistoryListState::Empty => {
            placeholder_view("这个项目还没有本地保存的历史会话", muted).into_any_element()
        }
        HistoryListState::Ready(table) => {
            div().flex_1().min_h_0().child(DataTable::new(&table).stripe(true)).into_any_element()
        }
    };

    let detail_body: AnyElement = match detail {
        None => placeholder_view("← 选择一个历史会话查看内容", muted).into_any_element(),
        Some((_, d)) if d.turns.is_empty() => {
            placeholder_view("这份会话没有可展示的对话内容", muted).into_any_element()
        }
        Some((_, d)) => div()
            .id("session-detail")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .children(d.turns.iter().enumerate().map(|(i, t)| {
                let role = if t.is_user { "用户" } else { "Claude" };
                let role_color = if t.is_user { accent } else { fg };
                let bubble_bg = if t.is_user { accent.opacity(0.12) } else { secondary };
                // 工具名按出现顺序去重计数，多次调用同一工具合并成一行摘要
                // （比如连续 3 次 Bash 就显示"Bash ×3"），不然长会话里全是重复胶囊。
                let tool_summary = (!t.tools.is_empty()).then(|| {
                    let mut order: Vec<&String> = Vec::new();
                    let mut counts: HashMap<&String, usize> = HashMap::new();
                    for tool in &t.tools {
                        counts.entry(tool).and_modify(|c| *c += 1).or_insert_with(|| {
                            order.push(tool);
                            1
                        });
                    }
                    order
                        .into_iter()
                        .map(|name| {
                            let c = counts[name];
                            if c > 1 { format!("{name} ×{c}") } else { name.clone() }
                        })
                        .collect::<Vec<_>>()
                        .join(" · ")
                });
                v_flex()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .bg(bubble_bg)
                    .when(t.is_user, |el| el.max_w(px(560.)))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_baseline()
                            .child(div().font_semibold().text_sm().text_color(role_color).child(role))
                            .children(t.timestamp.map(|ts| {
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(ts.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
                            })),
                    )
                    // 必须逐气泡给唯一 id：便捷函数 text::markdown() 拿调用处代码位置
                    // 当 id，循环里所有气泡会共享同一份 TextView 状态（文本互踩、高度
                    // 测量错乱，气泡整个叠在一起）。
                    .child(
                        div()
                            .text_sm()
                            .text_color(fg)
                            .child(crate::markdown_mermaid::markdown_view(("turn-md", i), t.text.clone())),
                    )
                    .children(tool_summary.map(|s| {
                        div().text_xs().text_color(muted).child(format!("🔧 {s}"))
                    }))
                    .into_any_element()
            }))
            .into_any_element(),
    };

    v_flex().flex_1().min_h_0().child(switcher).child(agent_switcher).child(
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .child(
                div()
                    .w(px(280.))
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .border_r_1()
                    .border_color(c_border)
                    .child(list_body),
            )
            .child(detail_body),
    )
}

/// 会话来源 tab 上的一个按钮，选中态用 accent 底色标出来（跟 `pane_button` 同款
/// 视觉，但换 agent 时还要顺带清掉右侧详情——不然会显示"上一个 agent 那份会话"
/// 的残留内容，跟点开新会话前那一瞬间的空白状态不一致）。
#[allow(clippy::too_many_arguments)]
fn agent_tab_button(
    target: AcpAgentKind,
    current: AcpAgentKind,
    accent: Hsla,
    fg: Hsla,
    muted: Hsla,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    let selected = target == current;
    div()
        .id(target.id())
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .text_color(if selected { fg } else { muted })
        .when(selected, |d| d.bg(accent.opacity(0.18)))
        .when(!selected, |d| d.hover(|s| s.text_color(fg)))
        .child(target.short_label())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                if this.history_agent != target {
                    this.history_agent = target;
                    this.session_detail = None;
                    cx.notify();
                }
            }),
        )
}

/// 切换条上的一个按钮。选中态用 accent 底色标出来。
#[allow(clippy::too_many_arguments)]
fn pane_button(
    label: &'static str,
    target: HistoryPane,
    current: HistoryPane,
    accent: Hsla,
    fg: Hsla,
    muted: Hsla,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    let selected = target == current;
    div()
        .id(label)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .text_color(if selected { fg } else { muted })
        .when(selected, |d| d.bg(accent.opacity(0.18)))
        .when(!selected, |d| d.hover(|s| s.text_color(fg)))
        .child(label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                if this.history_pane != target {
                    this.history_pane = target;
                    // 换子页时清掉右边的选中项，免得显示上一个子页残留的详情。
                    this.memory_selected = None;
                    cx.notify();
                }
            }),
        )
}

/// 记忆子页：左列表（标题 + 一句话描述）+ 右详情（markdown 全文）。
#[allow(clippy::too_many_arguments)]
fn memory_body(
    memories: Option<Rc<Vec<MemoryEntry>>>,
    selected: Option<usize>,
    muted: Hsla,
    fg: Hsla,
    c_border: Hsla,
    accent: Hsla,
    cx: &mut Context<Workspace>,
) -> Div {
    let list_body: AnyElement = match &memories {
        None => placeholder_view("加载中…", muted).into_any_element(),
        Some(list) if list.is_empty() => placeholder_view(
            "这个项目还没有记忆。Claude Code 会把值得长期记住的事写进 ~/.claude 下的 memory 目录。",
            muted,
        )
        .into_any_element(),
        Some(list) => {
            let mut col = v_flex().id("memory-list").flex_1().min_h_0().overflow_y_scroll().p_2().gap_1();
            for (ix, m) in list.iter().enumerate() {
                let is_sel = selected == Some(ix);
                col = col.child(
                    v_flex()
                        .id(("memory-row", ix))
                        .w_full()
                        .gap_0p5()
                        .px_2()
                        .py_2()
                        .rounded_md()
                        .cursor_pointer()
                        .when(is_sel, |d| d.bg(accent.opacity(0.18)))
                        .when(!is_sel, |d| d.hover(|s| s.bg(c_border.opacity(0.5))))
                        .child(div().text_sm().text_color(fg).child(m.name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(truncate(&m.description, 60)),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.memory_selected = Some(ix);
                                cx.notify();
                            }),
                        ),
                );
            }
            col.into_any_element()
        }
    };

    let detail_body: AnyElement = match memories.as_ref().and_then(|l| selected.and_then(|ix| l.get(ix))) {
        None => placeholder_view("← 选择一条记忆查看内容", muted).into_any_element(),
        Some(m) => v_flex()
            .id("memory-detail")
            .flex_1()
            .min_h_0()
            // min_w_0 不能省：flex item 的默认 min-width 是 auto，即「不收缩到比内容更窄」。
            // 少了它，这一栏会被记忆正文里最长的那行撑开，超出窗口的部分被直接裁掉，
            // 文本也永远不会换行（会话那边没踩到，是因为气泡上有 max_w 兜着）。
            .min_w_0()
            .overflow_y_scroll()
            .p_4()
            .gap_2()
            .child(div().text_lg().text_color(fg).child(m.name.clone()))
            .children((!m.description.is_empty()).then(|| {
                div().text_sm().text_color(muted).child(m.description.clone())
            }))
            // markdown 得给唯一 id，否则跟别处的 TextView 共享状态互踩（同 turn 气泡的坑）。
            // 外面这层 w_full + min_w_0 是给正文定死一个「可用宽度」，长行才会在这个宽度
            // 上折行；不设的话它按内容宽度铺开，撑破整栏被裁掉。
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .child(crate::markdown_mermaid::markdown_view("memory-md", m.body.clone())),
            )
            .into_any_element(),
    };

    div()
        .flex_1()
        .min_h_0()
        .flex()
        .child(
            div()
                .w(px(280.))
                .flex()
                .flex_col()
                .min_h_0()
                .border_r_1()
                .border_color(c_border)
                .child(list_body),
        )
        .child(detail_body)
}

use crate::settings::AcpAgentKind;

/// 历史会话缓存 key：四家 agent 各存各的，同一个 cwd 换个 tab 是完全不同的数据，
/// 光用 cwd 当 key 会把 Claude 的列表和 Codex 的列表互相顶掉。
pub(crate) fn session_list_key(agent: AcpAgentKind, cwd: &str) -> String {
    format!("{}:{cwd}", agent.id())
}

fn list_sessions_for(agent: AcpAgentKind, cwd: &str) -> Vec<SessionSummary> {
    match agent {
        AcpAgentKind::Claude => list_sessions(cwd),
        AcpAgentKind::Codex => list_codex_sessions(cwd),
        AcpAgentKind::Grok => list_grok_sessions(cwd),
        AcpAgentKind::Copilot => list_copilot_sessions(cwd),
    }
}

fn load_session_detail_for(agent: AcpAgentKind, path: &Path) -> Option<SessionDetail> {
    match agent {
        AcpAgentKind::Claude => load_session_detail(path),
        AcpAgentKind::Codex => load_codex_session_detail(path),
        AcpAgentKind::Grok => load_grok_session_detail(path),
        AcpAgentKind::Copilot => load_copilot_session_detail(path),
    }
}

impl Workspace {
    /// 历史会话页：确保当前 agent + 项目的会话列表缓存新鲜（>10s 或缺失就后台
    /// 重新扫描）。总览卡片那边固定传 `AcpAgentKind::Claude`，跟历史页的 tab
    /// 切换共用同一份缓存/同一套读写路径。
    pub fn ensure_session_list(&mut self, agent: AcpAgentKind, cwd: String, cx: &mut Context<Self>) {
        let key = session_list_key(agent, &cwd);
        let fresh = self
            .session_list
            .get(&key)
            .is_some_and(|(t, _)| t.elapsed() < std::time::Duration::from_secs(10));
        if fresh || self.session_list_inflight.contains(&key) {
            return;
        }
        self.session_list_inflight.insert(key.clone());
        cx.spawn(async move |this, cx| {
            let c = cwd.clone();
            let sessions = cx
                .background_executor()
                .spawn(async move { list_sessions_for(agent, &c) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.session_list_inflight.remove(&key);
                this.session_list.insert(key, (Instant::now(), Rc::new(sessions)));
                cx.notify();
            });
        })
        .detach();
    }

    /// 历史会话页：点开一份会话，按当前 tab 选中的 agent 用对应的解析器后台跑成
    /// Turn 列表。用自增 gen 丢弃过期结果（解析期间又点了别的会话，或切了 tab）。
    pub fn open_session_detail(
        &mut self,
        agent: AcpAgentKind,
        path: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.session_detail_gen = self.session_detail_gen.wrapping_add(1);
        let r#gen = self.session_detail_gen;
        self.session_detail = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let detail = cx
                .background_executor()
                .spawn(async move { load_session_detail_for(agent, &p) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.session_detail_gen != r#gen {
                    return;
                }
                if let Some(detail) = detail {
                    this.session_detail = Some((path, Rc::new(detail)));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 历史会话表格懒建 / 刷新：同项目（key 不变）只换 delegate 里的数据（保留排序/
    /// 滚动/选中状态）；换项目（key 变）整个重建 Entity（重置这些状态，体感上是
    /// "进了一个新页面"）。
    pub fn ensure_session_table(
        &mut self,
        key: &str,
        sessions: Rc<Vec<SessionSummary>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TableState<SessionHistoryDelegate>> {
        if self.session_table_key.as_deref() == Some(key) {
            if let Some(table) = &self.session_table {
                table.update(cx, |t, cx| {
                    t.delegate_mut().sessions = sessions;
                    t.refresh(cx);
                });
                return table.clone();
            }
        }
        let table = cx.new(|cx| TableState::new(SessionHistoryDelegate::new(sessions), window, cx));
        self.session_table_sub =
            Some(cx.subscribe_in(&table, window, |this, table, ev: &TableEvent, _window, cx| {
                if let TableEvent::SelectRow(ix) = ev {
                    if let Some(s) = table.read(cx).delegate().sessions.get(*ix) {
                        this.open_session_detail(this.history_agent, s.path.clone(), cx);
                    }
                }
            }));
        self.session_table_key = Some(key.to_string());
        self.session_table = Some(table.clone());
        table
    }
}

#[cfg(test)]
mod tests {
    // 不用 `use super::*;`：本文件后半段引入了 gpui/gpui_component 的 glob 导入，
    // 带进这个测试模块会让 trait 解析图爆炸式增长，`cargo test` 编译期直接撞
    // rustc 的递归限制崩溃（甚至 SIGBUS）——只导入测试真正用到的几个名字就够了。
    use super::{
        list_codex_sessions, list_copilot_sessions, list_grok_sessions, list_sessions,
        load_codex_session_detail, load_copilot_session_detail, load_grok_session_detail,
        load_session_detail, project_dir,
    };
    use std::path::Path;
    use std::sync::Mutex;

    fn write(dir: &Path, name: &str, lines: &[&str]) {
        std::fs::write(dir.join(name), lines.join("\n")).unwrap();
    }

    /// 好几个测试都要临时改 `HOME` 指向沙盒目录再复原——`cargo test` 默认多线程
    /// 并发跑同一个二进制里的测试，两个测试同时改这个进程级环境变量会互相踩
    /// （一个测试的 HOME 被另一个测试的复原覆盖掉）。拿这把锁把"改 HOME → 跑
    /// 逻辑 → 复原 HOME"这段区间串行化，锁本身不检查什么，只借它的互斥语义。
    static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// 在锁保护下临时把 HOME 指向 `home`，跑完 `f` 再复原。
    fn with_home<R>(home: &Path, f: impl FnOnce() -> R) -> R {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", home);
        }
        let result = f();
        match prev {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        result
    }

    #[test]
    fn project_dir_replaces_slashes_and_dots() {
        assert_eq!(project_dir("/Users/c.chen/dev/smelt"), "-Users-c-chen-dev-smelt");
    }

    #[test]
    fn list_sessions_summarizes_title_and_counts_and_sorts_by_recency() {
        let tmp = std::env::temp_dir().join("smelt-session-history-test-list");
        let _ = std::fs::remove_dir_all(&tmp);
        let proj_root = tmp.join(".claude").join("projects").join(project_dir("/x/y"));
        std::fs::create_dir_all(&proj_root).unwrap();

        write(
            &proj_root,
            "older.jsonl",
            &[
                r#"{"type":"user","timestamp":"2026-07-01T00:00:00Z","message":{"content":"hello there"}}"#,
                r#"{"type":"assistant","timestamp":"2026-07-01T00:00:05Z","message":{"content":[{"type":"text","text":"hi"}]}}"#,
            ],
        );
        write(
            &proj_root,
            "newer.jsonl",
            &[r#"{"type":"user","timestamp":"2026-07-05T00:00:00Z","message":{"content":"second session"}}"#],
        );

        let sessions = with_home(&tmp, || list_sessions("/x/y"));
        std::fs::remove_dir_all(&tmp).unwrap();

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].path.file_stem().unwrap(), "newer");
        assert_eq!(sessions[0].title, "second session");
        assert_eq!(sessions[1].path.file_stem().unwrap(), "older");
        assert_eq!(sessions[1].message_count, 2);
    }

    #[test]
    fn load_session_detail_skips_tool_result_and_sidechain() {
        let tmp = std::env::temp_dir().join("smelt-session-history-test-detail.jsonl");
        write(
            &tmp.parent().unwrap().to_path_buf(),
            tmp.file_name().unwrap().to_str().unwrap(),
            &[
                r#"{"type":"user","timestamp":"2026-07-01T00:00:00Z","message":{"content":"do the thing"}}"#,
                r#"{"type":"user","timestamp":"2026-07-01T00:00:01Z","message":{"content":[{"type":"tool_result","content":"raw output"}]}}"#,
                r#"{"type":"assistant","timestamp":"2026-07-01T00:00:02Z","message":{"content":[{"type":"text","text":"done"},{"type":"tool_use","name":"Bash"}]}}"#,
                r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-07-01T00:00:03Z","message":{"content":[{"type":"text","text":"subagent chatter"}]}}"#,
            ],
        );

        let detail = load_session_detail(&tmp).unwrap();
        std::fs::remove_file(&tmp).unwrap();

        assert_eq!(detail.turns.len(), 2);
        assert!(detail.turns[0].is_user);
        assert_eq!(detail.turns[0].text, "do the thing");
        assert!(!detail.turns[1].is_user);
        assert_eq!(detail.turns[1].text, "done");
        assert_eq!(detail.turns[1].tools, vec!["Bash".to_string()]);
    }

    #[test]
    fn codex_reader_filters_by_cwd_skips_synthetic_context_and_groups_tool_calls() {
        let tmp = std::env::temp_dir().join("smelt-session-history-test-codex");
        let _ = std::fs::remove_dir_all(&tmp);
        let day_dir = tmp.join(".codex").join("sessions").join("2026").join("07").join("01");
        std::fs::create_dir_all(&day_dir).unwrap();
        write(
            &day_dir,
            "rollout-test.jsonl",
            &[
                r#"{"timestamp":"2026-07-01T00:00:00Z","type":"session_meta","payload":{"id":"cx-1","cwd":"/proj"}}"#,
                r#"{"timestamp":"2026-07-01T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd stuff</environment_context>"}]}}"#,
                r#"{"timestamp":"2026-07-01T00:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"实际问题"}]}}"#,
                r#"{"timestamp":"2026-07-01T00:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"我来看看"}]}}"#,
                r#"{"timestamp":"2026-07-01T00:00:04Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1"}}"#,
                r#"{"timestamp":"2026-07-01T00:00:05Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"ok"}}"#,
            ],
        );
        // 不同 cwd 的会话不该出现在结果里。
        write(
            &day_dir,
            "rollout-other.jsonl",
            &[r#"{"timestamp":"2026-07-01T00:00:00Z","type":"session_meta","payload":{"id":"cx-2","cwd":"/other"}}"#],
        );

        let sessions = with_home(&tmp, || list_codex_sessions("/proj"));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "实际问题"); // 合成的 environment_context 不该被当标题
        assert_eq!(sessions[0].message_count, 2);
        assert_eq!(sessions[0].last_tool.as_deref(), Some("exec_command"));

        let detail = load_codex_session_detail(&sessions[0].path).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(detail.turns.len(), 2); // 合成消息被跳过，剩真实问题 + assistant 轮
        assert!(detail.turns[0].is_user);
        assert_eq!(detail.turns[0].text, "实际问题");
        assert!(!detail.turns[1].is_user);
        assert_eq!(detail.turns[1].text, "我来看看");
        assert_eq!(detail.turns[1].tools, vec!["exec_command".to_string()]); // 工具调用挂在上一条 assistant 轮上
    }

    #[test]
    fn grok_reader_reads_summary_json_and_skips_synthetic_rows() {
        let tmp = std::env::temp_dir().join("smelt-session-history-test-grok");
        let _ = std::fs::remove_dir_all(&tmp);
        let session_dir = tmp.join(".grok").join("sessions").join("proj").join("s1");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("summary.json"),
            r#"{"info":{"cwd":"/proj"},"session_summary":"聊聊策略","created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-01T00:05:00Z","num_chat_messages":2}"#,
        )
        .unwrap();
        write(
            &session_dir,
            "chat_history.jsonl",
            &[
                r#"{"type":"user","synthetic_reason":"project_instructions","content":[{"type":"text","text":"注入的项目说明"}]}"#,
                // 实测：第一轮的 <user_info> 环境块不带 synthetic_reason 字段，得靠
                // 「剥完包装仍是尖括号开头」这条兜底规则识别，不是只认这个字段。
                r#"{"type":"user","content":[{"type":"text","text":"<user_info>\nOS: macos\n</user_info>"}]}"#,
                r#"{"type":"user","content":[{"type":"text","text":"<user_query>真实问题</user_query>"}]}"#,
                r#"{"type":"assistant","content":"回答","tool_calls":[{"id":"c1","name":"grep"}]}"#,
                r#"{"type":"tool_result","tool_call_id":"c1","content":"..."}"#,
            ],
        );

        let sessions = with_home(&tmp, || list_grok_sessions("/proj"));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "聊聊策略");
        assert_eq!(sessions[0].message_count, 2);

        let detail = load_grok_session_detail(&sessions[0].path).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(detail.turns.len(), 2); // 两条合成消息（带/不带 synthetic_reason）都被跳过
        assert!(detail.turns[0].is_user);
        assert_eq!(detail.turns[0].text, "真实问题"); // <user_query> 包装被剥掉
        assert!(!detail.turns[1].is_user);
        assert_eq!(detail.turns[1].tools, vec!["grep".to_string()]);
    }

    #[test]
    fn copilot_reader_reads_workspace_yaml_and_events_jsonl() {
        let tmp = std::env::temp_dir().join("smelt-session-history-test-copilot");
        let _ = std::fs::remove_dir_all(&tmp);
        let session_dir = tmp.join(".copilot").join("session-state").join("s1");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("workspace.yaml"),
            "id: s1\ncwd: /proj\nsummary: 调试问题\ncreated_at: 2026-07-01T00:00:00.000Z\nupdated_at: 2026-07-01T00:05:00.000Z\n",
        )
        .unwrap();
        write(
            &session_dir,
            "events.jsonl",
            &[
                r#"{"type":"user.message","data":{"content":"真实问题","transformedContent":"<ide_selection>真实问题</ide_selection>"}}"#,
                r#"{"type":"assistant.message","data":{"content":"回答","toolRequests":[{"toolCallId":"t1","name":"bash"}]}}"#,
                r#"{"type":"tool.execution_start","data":{"toolCallId":"t1","toolName":"bash"}}"#,
            ],
        );

        let sessions = with_home(&tmp, || list_copilot_sessions("/proj"));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "调试问题");
        assert_eq!(sessions[0].message_count, 2);
        assert_eq!(sessions[0].last_tool.as_deref(), Some("bash"));

        let detail = load_copilot_session_detail(&sessions[0].path).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(detail.turns.len(), 2);
        assert!(detail.turns[0].is_user);
        // transformedContent（带 IDE 上下文）不该混进来，只取干净的 content。
        assert_eq!(detail.turns[0].text, "真实问题");
        assert_eq!(detail.turns[1].tools, vec!["bash".to_string()]);
    }
}
