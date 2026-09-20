//! 历史会话浏览：列出某个项目下各家 agent CLI 本地保存的历史会话。每种格式由
//! 自己的 Provider 解析，共用 `SessionSummary`/`Turn`/`SessionDetail` 展示模型；
//! 续接同时支持 ACP 消息流和 CLI/TUI 两条路径。
//!
//! 各家格式调研自实测（CLI 版本可能变化，这些解析都是「尽力而为」，不是协议）：
//! - Claude: `~/.claude/projects/<项目目录编码>/<session_id>.jsonl`
//! - Codex: `~/.codex/sessions/<年>/<月>/<日>/rollout-*.jsonl`（按日期分区，不按项目）
//! - Grok: `~/.grok/sessions/<url编码cwd>/<session_id>/`（`summary.json` + `chat_history.jsonl`）
//! - Copilot: `~/.copilot/session-state/<session_id>/`（`workspace.yaml` + `events.jsonl`）
//! - Pi: `~/.pi/agent/sessions/**/<session>.jsonl`（可由 `PI_CODING_AGENT_DIR` /
//!   `PI_CODING_AGENT_SESSION_DIR` 或 `settings.json` 的 `sessionDir` 覆盖）
//! - dsh: 原生 profile 的 session persistence（JSONL 后端可提供本地预览）

use crate::agent_kind::{ConversationAgentKind, HistorySourceKind, TerminalAgentKind};
use crate::fs::{FileSystem, LocalFs};
use crate::session_handoff::HandoffTurn;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// 项目目录编码 / transcript 路径：唯一权威来源现在是 crate::claude_paths。
// ACP 连接层挪进 smelt-core 后，续接可行性预检也要用同一份规则，不能这边一份那边一份。
pub use crate::claude_paths::{project_dir, projects_root};

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// 一份历史会话的概览（列表用）。
#[derive(Clone)]
pub struct SessionSummary {
    pub path: PathBuf,
    /// 实际展示标题：用户设置过名称时优先，否则使用 agent_title。
    pub title: String,
    /// Agent transcript / summary 提供的原始标题，用作搜索和自定义标题下的辅助信息。
    pub agent_title: String,
    pub custom_title: Option<String>,
    /// agent 那头认得的 session id——续接时要发给协议的就是这个，不是 `path`。
    /// 各家 agent 取法不一样：Claude 是文件名去扩展名，Codex 是 `session_meta.id`，
    /// Grok/Copilot 是会话目录名，`path` 本身的形状（文件 vs 目录）各家也不一样，
    /// 不能拿 `path` 现算，得在各自的 summarize 里就近取一份存下来。
    pub resume_id: String,
    pub started_at: Option<DateTime<Utc>>,
    pub last_active_at: Option<DateTime<Utc>>,
    /// user + assistant 消息总数（不含被跳过的 tool_result / 内部记录）。
    pub message_count: usize,
    /// 本份会话消耗的 token 总量（input+output+两种 cache 相加），供总览卡片展示
    /// 「当前会话」口径的用量。
    pub total_tokens: u64,
}

/// 一轮对话：用户发言 / Assistant 回复（含它这轮调用了哪些工具）。
pub struct Turn {
    pub is_user: bool,
    pub timestamp: Option<DateTime<Utc>>,
    pub text: String,
    /// 这轮里 assistant 调用的工具名（user 轮恒为空）。
    pub tools: Vec<String>,
    /// 这轮工具入参里出现的文件路径，去重后按出现顺序排列。会话迁移要靠它告诉
    /// 下一个 agent「上一段动过哪些文件」；历史页本身不显示。
    ///
    /// 各家 agent 给得起的程度不一样（都是实测）：Claude 的 `tool_use.input.file_path`、
    /// Grok 的 `target_file`/`path`/`target_directory`、Copilot 的 `path` 都是结构化
    /// 参数，直接取；**Codex 取不到**——它的工具是 `exec_command`，参数只有一整条
    /// shell 命令（`cmd`），从命令行里猜路径不如不猜，留空。
    pub tool_paths: Vec<String>,
}

impl Turn {
    /// 纯文本轮次（没有工具信息）的构造快捷方式，省得各家读取器各写一遍
    /// `tools: Vec::new(), tool_paths: Vec::new()`。
    fn message(is_user: bool, timestamp: Option<DateTime<Utc>>, text: String) -> Self {
        Self {
            is_user,
            timestamp,
            text,
            tools: Vec::new(),
            tool_paths: Vec::new(),
        }
    }
}

pub struct SessionDetail {
    pub turns: Vec<Turn>,
    /// 原会话用的模型（读得到才有）。迁移时写进交接头部，作为「上一段是谁干的」
    /// 的交代——不会拿它去设置目标 agent 的模型，那是各家私有取值。
    pub model: Option<String>,
}

type ListSessions = fn(&str, Option<&str>) -> Vec<SessionSummary>;
type LoadSessionDetail = fn(&Path) -> Option<SessionDetail>;
type AcpIndexPath = fn(Option<&str>, &str) -> PathBuf;

/// Provider 可选的 ACP `session/list` 索引能力。能力对象同时持有合成路径策略，
/// 调用方不需要先检查布尔值、再假设路径生成器必然存在。
pub(crate) struct AcpSessionIndex {
    path: AcpIndexPath,
}

impl AcpSessionIndex {
    pub(crate) fn path(&self, profile_id: Option<&str>, resume_id: &str) -> PathBuf {
        (self.path)(profile_id, resume_id)
    }
}

/// 一种 Agent 的历史数据 Provider。列表、详情和可选的 ACP live index 在同一个
/// 注册点声明，调用方只负责传上下文，不再按 Agent 身份分发到具体解析器。
pub(crate) struct SessionHistoryProvider {
    pub agent: HistorySourceKind,
    list_sessions: ListSessions,
    load_detail: LoadSessionDetail,
    acp_session_index: Option<AcpSessionIndex>,
}

impl SessionHistoryProvider {
    pub(crate) fn list_sessions(
        &self,
        cwd: &str,
        override_dir: Option<&str>,
    ) -> Vec<SessionSummary> {
        (self.list_sessions)(cwd, override_dir)
    }

    fn load_detail(&self, path: &Path) -> Option<SessionDetail> {
        (self.load_detail)(path)
    }

    pub(crate) fn acp_session_index(&self) -> Option<&AcpSessionIndex> {
        self.acp_session_index.as_ref()
    }
}

fn dsh_acp_index_path(profile_id: Option<&str>, resume_id: &str) -> PathBuf {
    crate::agent_kind::dsh_sessions_root()
        .unwrap_or_else(std::env::temp_dir)
        .join(".acp-index")
        .join(profile_id.unwrap_or("default"))
        .join(resume_id.replace(['/', '\\'], "_"))
}

/// 历史 Provider 注册表。顺序必须与 `HistorySourceKind::ALL` 一致；完整性测试守住
/// 一种来源恰好对应一个 Provider，新增来源时不会漏掉列表或详情链路。
///
/// 注意这张表的键是 [`HistorySourceKind`] 而不是 `ConversationAgentKind`：能不能读
/// 历史取决于本机有没有落盘 transcript，跟有没有 ACP 无关。
pub(crate) static SESSION_HISTORY_PROVIDERS: &[SessionHistoryProvider] = &[
    SessionHistoryProvider {
        agent: HistorySourceKind::Conversation(ConversationAgentKind::Claude),
        list_sessions,
        load_detail: load_session_detail,
        acp_session_index: None,
    },
    SessionHistoryProvider {
        agent: HistorySourceKind::Conversation(ConversationAgentKind::Copilot),
        list_sessions: list_copilot_sessions,
        load_detail: load_copilot_session_detail,
        acp_session_index: None,
    },
    SessionHistoryProvider {
        agent: HistorySourceKind::Conversation(ConversationAgentKind::Codex),
        list_sessions: list_codex_sessions,
        load_detail: load_codex_session_detail,
        acp_session_index: None,
    },
    SessionHistoryProvider {
        agent: HistorySourceKind::Conversation(ConversationAgentKind::Grok),
        list_sessions: list_grok_sessions,
        load_detail: load_grok_session_detail,
        acp_session_index: None,
    },
    SessionHistoryProvider {
        agent: HistorySourceKind::Conversation(ConversationAgentKind::Cursor),
        list_sessions: list_cursor_sessions,
        load_detail: load_cursor_session_detail,
        acp_session_index: None,
    },
    SessionHistoryProvider {
        agent: HistorySourceKind::Conversation(ConversationAgentKind::OpenCode),
        list_sessions: list_opencode_sessions,
        load_detail: load_opencode_session_detail,
        acp_session_index: None,
    },
    SessionHistoryProvider {
        agent: HistorySourceKind::Conversation(ConversationAgentKind::Kiro),
        list_sessions: list_kiro_sessions,
        load_detail: load_kiro_session_detail,
        acp_session_index: None,
    },
    SessionHistoryProvider {
        agent: HistorySourceKind::Conversation(ConversationAgentKind::Pi),
        list_sessions: list_pi_sessions,
        load_detail: load_pi_session_detail,
        acp_session_index: None,
    },
    SessionHistoryProvider {
        agent: HistorySourceKind::Conversation(ConversationAgentKind::Dsh),
        list_sessions: list_dsh_sessions,
        load_detail: load_dsh_session_detail,
        acp_session_index: Some(AcpSessionIndex {
            path: dsh_acp_index_path,
        }),
    },
    // 只有 TUI、没有 ACP，但会话实打实落在 `~/.gemini/antigravity-cli/`。
    SessionHistoryProvider {
        agent: HistorySourceKind::TerminalOnly(TerminalAgentKind::Antigravity),
        list_sessions: list_antigravity_sessions,
        load_detail: load_antigravity_session_detail,
        acp_session_index: None,
    },
];

pub(crate) fn history_provider(agent: HistorySourceKind) -> &'static SessionHistoryProvider {
    SESSION_HISTORY_PROVIDERS
        .iter()
        .find(|provider| provider.agent == agent)
        .unwrap_or_else(|| panic!("{} 未注册历史 Provider", agent.id()))
}

pub fn load_agent_session_detail(agent: HistorySourceKind, path: &Path) -> Option<SessionDetail> {
    history_provider(agent).load_detail(path)
}

/// 从工具入参里挑出看着像文件/目录路径的值。只认各家实测存在的结构化参数名，
/// 不做启发式猜测——猜错了写进交接记录，会把下一个 agent 引到不存在的文件上。
fn tool_path_args(input: &Value) -> Vec<String> {
    const PATH_KEYS: [&str; 4] = ["file_path", "target_file", "path", "target_directory"];
    PATH_KEYS
        .iter()
        .filter_map(|key| input.get(key).and_then(|v| v.as_str()))
        .filter(|p| !p.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// 追加路径并去重（同一轮里 Read 完再 Edit 同一个文件很常见）。
fn push_unique_paths(target: &mut Vec<String>, paths: Vec<String>) {
    for path in paths {
        if !target.contains(&path) {
            target.push(path);
        }
    }
}

/// 列出某个项目目录下的所有历史会话，按最近活跃时间降序。
/// 只读扫描，可能要几十毫秒（视会话数量），调用方应放后台线程跑。
///
/// `override_dir`：多 workspace 场景下手动添加的 profile 显式指定的
/// `CLAUDE_CONFIG_DIR`（见 `crate::claude_paths::projects_root`），
/// `None` 就是默认 workspace，行为不变。
pub fn list_sessions(cwd: &str, override_dir: Option<&str>) -> Vec<SessionSummary> {
    list_sessions_with(&LocalFs, cwd, override_dir)
}

/// [`list_sessions`] 的 provider-aware 底层。路径解析仍遵循 Claude Code 的
/// 配置规则，目录扫描和 transcript 读取全部由 `fs` 决定。
pub fn list_sessions_with(
    fs: &dyn FileSystem,
    cwd: &str,
    override_dir: Option<&str>,
) -> Vec<SessionSummary> {
    let dir = projects_root(override_dir).join(project_dir(cwd));
    let Ok(entries) = fs.read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<SessionSummary> = entries
        .into_iter()
        .map(|e| e.path)
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .filter_map(|path| summarize_session_with(fs, &path))
        .collect();
    out.sort_by_key(|item| std::cmp::Reverse(item.last_active_at));
    out
}

/// 用户消息的真实文本。`message.content` 有两种形状：老一点的纯字符串，和
/// 实测目前 Claude Code CLI（含 ACP 模式）在用的块数组
/// `[{"type":"text","text":"..."}]`——工具结果回填给 user 角色时也是数组
/// （`[{"type":"tool_result",...}]`），形状一样但不是真人发言，得按块的
/// `type` 精确区分，不能像之前那样直接把"是不是数组"当判断依据（那样会把
/// 块数组格式的真实发言也一并当成 tool_result 漏掉，历史页显示的用户消息
/// 就会全部消失）。
const CLAUDE_LOCAL_COMMAND_MARKERS: &[(&str, &str)] = &[
    ("<command-name>", "</command-name>"),
    ("<command-message>", "</command-message>"),
    ("<command-args>", "</command-args>"),
    ("<local-command-stdout>", "</local-command-stdout>"),
    ("<local-command-stderr>", "</local-command-stderr>"),
];

/// Match claude-agent-acp's replay filtering: local slash-command bookkeeping
/// is not a user turn and is deliberately omitted by `session/load`.
fn strip_claude_local_command_metadata(text: &str) -> Option<String> {
    let mut text = text.to_string();
    for (open, close) in CLAUDE_LOCAL_COMMAND_MARKERS {
        while let Some(start) = text.find(open) {
            let Some(relative_end) = text[start + open.len()..].find(close) else {
                break;
            };
            let end = start + open.len() + relative_end + close.len();
            text.replace_range(start..end, "");
        }
    }
    (!text.trim().is_empty()).then_some(text)
}

fn claude_user_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return strip_claude_local_command_metadata(s);
    }
    let blocks = content.as_array()?;
    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .filter_map(strip_claude_local_command_metadata)
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn summarize_session_with(fs: &dyn FileSystem, path: &Path) -> Option<SessionSummary> {
    let text = fs.read_to_string(path).ok()?;
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let mut title: Option<String> = None;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut last_active_at: Option<DateTime<Utc>> = None;
    let mut message_count = 0usize;
    let mut total_tokens = 0u64;
    let mut seen_uuids: HashSet<String> = HashSet::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if row.get("isMeta").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(kind) = row.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
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
            if let Some(text) = row
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(claude_user_text)
            {
                message_count += 1;
                if title.is_none() {
                    title = Some(truncate(text.trim(), 80));
                }
            }
        } else {
            // assistant：content 数组里只要有 text 块就算一条消息；同 uuid 只算一次
            // （日志重写/追加异常会重复），token 只累加一次。
            let dup = row
                .get("uuid")
                .and_then(|v| v.as_str())
                .is_some_and(|u| !seen_uuids.insert(u.to_string()));
            let blocks = row
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());
            let has_text = blocks.is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            });
            if has_text {
                message_count += 1;
            }
            if !dup && let Some(usage) = row.get("message").and_then(|m| m.get("usage")) {
                let field = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                total_tokens += field("input_tokens")
                    + field("output_tokens")
                    + field("cache_creation_input_tokens")
                    + field("cache_read_input_tokens");
            }
        }
    }

    // Claude ACP omits local-command-only transcripts during session/load. Do
    // not advertise those files as resumable conversations in the first place.
    let title = title?;
    Some(SessionSummary {
        title: title.clone(),
        agent_title: title,
        custom_title: None,
        path: path.to_path_buf(),
        resume_id: session_id,
        started_at,
        last_active_at,
        message_count,
        total_tokens,
    })
}

/// 读某一份会话 transcript，还原成 Turn 列表供浏览。
/// 跳过子代理（isSidechain）消息 —— 混进主线对话会话读起来很乱，先不做嵌套展示；
/// 也跳过纯 tool_result 的 user 消息（那是工具输出回填，不是真实用户发言，assistant
/// 轮次里的工具名已经能说明调用了什么）。
pub fn load_session_detail(path: &Path) -> Option<SessionDetail> {
    load_session_detail_with(&LocalFs, path)
}

pub fn load_session_detail_with(fs: &dyn FileSystem, path: &Path) -> Option<SessionDetail> {
    let text = fs.read_to_string(path).ok()?;
    let mut turns = Vec::new();
    let mut model = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if row.get("isMeta").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if row.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        let Some(kind) = row.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
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
            let Some(text) = content.and_then(claude_user_text) else {
                continue;
            };
            turns.push(Turn::message(true, timestamp, text));
        } else {
            if model.is_none() {
                model = row
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
            let blocks = content.and_then(|c| c.as_array());
            let Some(blocks) = blocks else { continue };
            let text = blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            let tool_blocks = blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"));
            let mut tools = Vec::new();
            let mut tool_paths = Vec::new();
            for block in tool_blocks {
                if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                    tools.push(name.to_string());
                }
                if let Some(input) = block.get("input") {
                    push_unique_paths(&mut tool_paths, tool_path_args(input));
                }
            }
            if text.trim().is_empty() && tools.is_empty() {
                continue;
            }
            turns.push(Turn {
                is_user: false,
                timestamp,
                text,
                tools,
                tool_paths,
            });
        }
    }

    Some(SessionDetail { turns, model })
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

pub fn truncate_title(title: &str) -> String {
    truncate(title, 80)
}

// ===================== Codex =====================
//
// `~/.codex/sessions/<年>/<月>/<日>/rollout-*.jsonl`：不像 Claude 按项目分目录，
// 只能按日期分区遍历、逐份看第一行 session_meta 里的 cwd 是否匹配——文件多的话
// 比 Claude 那版慢，调用方本来就放后台线程跑，可以接受。

fn codex_sessions_root(override_dir: Option<&str>) -> PathBuf {
    codex_home(override_dir).join("sessions")
}

/// `CODEX_HOME` 设了就整段替换默认的 `~/.codex`（同 claude_paths.rs 的
/// `CLAUDE_CONFIG_DIR` 处理，走同一份 `crate::login_env` 探测）。
/// `override_dir` 优先于全局探测——多 workspace profile 的显式指定。
fn codex_home(override_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = override_dir {
        return PathBuf::from(dir);
    }
    if let Some(dir) = crate::login_env::codex_home() {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".codex")
}

pub fn list_codex_sessions(cwd: &str, override_dir: Option<&str>) -> Vec<SessionSummary> {
    list_codex_sessions_with(&LocalFs, cwd, override_dir)
}

pub fn list_codex_sessions_with(
    fs: &dyn FileSystem,
    cwd: &str,
    override_dir: Option<&str>,
) -> Vec<SessionSummary> {
    let root = codex_sessions_root(override_dir);
    let mut out = Vec::new();
    for year in read_dir_ok(fs, &root) {
        for month in read_dir_ok(fs, &year) {
            for day in read_dir_ok(fs, &month) {
                for path in read_dir_ok(fs, &day) {
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    if let Some(s) = summarize_codex_session_with(fs, &path, cwd) {
                        out.push(s);
                    }
                }
            }
        }
    }
    out.sort_by_key(|item| std::cmp::Reverse(item.last_active_at));
    out
}

fn read_dir_ok(fs: &dyn FileSystem, dir: &Path) -> Vec<PathBuf> {
    fs.read_dir(dir)
        .map(|entries| entries.into_iter().map(|e| e.path).collect())
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

fn summarize_codex_session_with(
    fs: &dyn FileSystem,
    path: &Path,
    want_cwd: &str,
) -> Option<SessionSummary> {
    let text = fs.read_to_string(path).ok()?;
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
    let session_id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let mut title: Option<String> = None;
    let started_at = meta
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(parse_rfc3339);
    let mut last_active_at = started_at;
    let mut message_count = 0usize;

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ts) = row
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(parse_rfc3339)
        {
            last_active_at = Some(last_active_at.map_or(ts, |l: DateTime<Utc>| l.max(ts)));
        }
        if row.get("type").and_then(|v| v.as_str()) != Some("response_item") {
            continue;
        }
        let Some(item) = row.get("payload") else {
            continue;
        };
        match item.get("type").and_then(|v| v.as_str()) {
            Some("message") => {
                let Some(msg_text) = codex_message_text(item) else {
                    continue;
                };
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
            Some("function_call") => {}
            _ => {}
        }
    }

    let title = title.unwrap_or_else(|| session_id.clone());
    Some(SessionSummary {
        path: path.to_path_buf(),
        title: title.clone(),
        agent_title: title,
        custom_title: None,
        resume_id: session_id,
        started_at,
        last_active_at,
        message_count,
        // Codex 的 event_msg.token_count 是「速率限制用量占比」，不是这一份会话的
        // token 总数，跟 Claude 那份口径对不上，宁可不接也不接一个会误导人的数字。
        total_tokens: 0,
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
    load_codex_session_detail_with(&LocalFs, path)
}

pub fn load_codex_session_detail_with(fs: &dyn FileSystem, path: &Path) -> Option<SessionDetail> {
    let text = fs.read_to_string(path).ok()?;
    let mut turns: Vec<Turn> = Vec::new();
    let mut model = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // 模型不在 `session_meta` 里，在每轮的 `turn_context`（实测 rollout：
        // session_meta 只有 cwd/git/cli_version 那些，model 每轮重复写一次）。
        if row.get("type").and_then(|v| v.as_str()) == Some("turn_context") && model.is_none() {
            model = row
                .get("payload")
                .and_then(|p| p.get("model"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        if row.get("type").and_then(|v| v.as_str()) != Some("response_item") {
            continue;
        }
        let timestamp = row
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(parse_rfc3339);
        let Some(item) = row.get("payload") else {
            continue;
        };
        match item.get("type").and_then(|v| v.as_str()) {
            Some("message") => {
                let Some(msg_text) = codex_message_text(item) else {
                    continue;
                };
                let is_user = match item.get("role").and_then(|v| v.as_str()) {
                    Some("user") => true,
                    Some("assistant") => false,
                    _ => continue, // system/developer 等指令角色，不是真实对话轮次
                };
                if is_user && is_synthetic_codex_text(&msg_text) {
                    continue; // CLI 自己注入的 <environment_context>，不是真人发言
                }
                turns.push(Turn::message(is_user, timestamp, msg_text));
            }
            Some("function_call") => {
                let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
                    continue;
                };
                // 工具调用挂到「上一条 assistant 轮次」上——Codex 的日志比 Claude 更碎，
                // 一次 assistant 发言常拆成「先一条 message 说要干嘛，再几条 function_call」，
                // 没有上一条 assistant 轮次就单独开一条只带工具名、没有正文的轮次。
                // 文件路径这里取不到：Codex 的工具是 `exec_command`，入参只有一条
                // shell 命令字符串（实测 arg keys 是 cmd/workdir/max_output_tokens），
                // 没有结构化的文件参数可读，`tool_paths` 一律留空。
                match turns.last_mut() {
                    Some(t) if !t.is_user => t.tools.push(name.to_string()),
                    _ => turns.push(Turn {
                        is_user: false,
                        timestamp,
                        text: String::new(),
                        tools: vec![name.to_string()],
                        tool_paths: Vec::new(),
                    }),
                }
            }
            _ => {}
        }
    }

    Some(SessionDetail { turns, model })
}

// ===================== Grok =====================
//
// `~/.grok/sessions/<url编码cwd>/<session_id>/`：`summary.json` 已经现成给了标题/
// 时间/消息数（不用像 Claude/Codex 那样扫整份 transcript 才能拿到概览，列表这块
// 效率很高），`chat_history.jsonl` 才是完整对话内容。

fn grok_sessions_root(override_dir: Option<&str>) -> PathBuf {
    grok_home(override_dir).join("sessions")
}

/// `GROK_HOME` 设了就整段替换默认的 `~/.grok`。`override_dir` 同上，优先级最高。
fn grok_home(override_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = override_dir {
        return PathBuf::from(dir);
    }
    if let Some(dir) = crate::login_env::grok_home() {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".grok")
}

pub fn list_grok_sessions(cwd: &str, override_dir: Option<&str>) -> Vec<SessionSummary> {
    list_grok_sessions_with(&LocalFs, cwd, override_dir)
}

pub fn list_grok_sessions_with(
    fs: &dyn FileSystem,
    cwd: &str,
    override_dir: Option<&str>,
) -> Vec<SessionSummary> {
    let root = grok_sessions_root(override_dir);
    let mut out = Vec::new();
    for project_dir in read_dir_ok(fs, &root) {
        if !fs.is_dir(&project_dir) {
            continue; // 跳过同级的 session_search.sqlite
        }
        for session_dir in read_dir_ok(fs, &project_dir) {
            if let Some(s) = summarize_grok_session_with(fs, &session_dir, cwd) {
                out.push(s);
            }
        }
    }
    out.sort_by_key(|item| std::cmp::Reverse(item.last_active_at));
    out
}

fn summarize_grok_session_with(
    fs: &dyn FileSystem,
    session_dir: &Path,
    want_cwd: &str,
) -> Option<SessionSummary> {
    let summary_path = session_dir.join("summary.json");
    let text = fs.read_to_string(&summary_path).ok()?;
    let summary: Value = serde_json::from_str(&text).ok()?;
    if summary
        .get("info")
        .and_then(|i| i.get("cwd"))
        .and_then(|v| v.as_str())
        != Some(want_cwd)
    {
        return None;
    }
    let session_id = session_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let title = summary
        .get("session_summary")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| truncate(s.trim(), 80))
        .unwrap_or_else(|| session_id.clone());
    Some(SessionSummary {
        path: session_dir.to_path_buf(),
        title: title.clone(),
        agent_title: title,
        custom_title: None,
        resume_id: session_id,
        started_at: summary
            .get("created_at")
            .and_then(|v| v.as_str())
            .and_then(parse_rfc3339),
        last_active_at: summary
            .get("updated_at")
            .and_then(|v| v.as_str())
            .and_then(parse_rfc3339),
        message_count: summary
            .get("num_chat_messages")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        // summary.json 没有 token 统计字段（实测），跟 Codex 一样宁可留空。
        total_tokens: 0,
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
    let Some(rest) = t.strip_prefix("<user_query>") else {
        return text;
    };
    rest.strip_suffix("</user_query>")
        .map(str::trim)
        .unwrap_or(text)
}

pub fn load_grok_session_detail(session_dir: &Path) -> Option<SessionDetail> {
    load_grok_session_detail_with(&LocalFs, session_dir)
}

pub fn load_grok_session_detail_with(
    fs: &dyn FileSystem,
    session_dir: &Path,
) -> Option<SessionDetail> {
    let path = session_dir.join("chat_history.jsonl");
    let text = fs.read_to_string(&path).ok()?;
    let mut turns: Vec<Turn> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match row.get("type").and_then(|v| v.as_str()) {
            Some("user") => {
                let Some(content) = row.get("content") else {
                    continue;
                };
                let raw = grok_text_blocks(content);
                if raw.trim().is_empty() {
                    continue;
                }
                let text = strip_user_query_wrapper(&raw).to_string();
                if is_synthetic_grok_row(&row, &text) {
                    continue;
                }
                turns.push(Turn::message(true, None, text));
            }
            Some("assistant") => {
                let text = row
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let mut tools: Vec<String> = Vec::new();
                let mut tool_paths: Vec<String> = Vec::new();
                for call in row
                    .get("tool_calls")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(name) = call.get("name").and_then(|n| n.as_str()) {
                        tools.push(name.to_string());
                    }
                    // Grok 的 arguments 是 JSON **字符串**（实测
                    // `{"target_directory":"."}`），不是对象，得再解一层。
                    if let Some(args) = call
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    {
                        push_unique_paths(&mut tool_paths, tool_path_args(&args));
                    }
                }
                if text.trim().is_empty() && tools.is_empty() {
                    continue;
                }
                // Grok 的 chat_history.jsonl 逐行不带时间戳（跟 Claude/Codex 不同），
                // 只有整份会话的 created_at/updated_at（见 summary.json），没有更细的
                // 逐轮时间可用，就都留 None，UI 本来就把 None 当「不显示时间」处理。
                turns.push(Turn {
                    is_user: false,
                    timestamp: None,
                    text,
                    tools,
                    tool_paths,
                });
            }
            _ => {} // reasoning / system / tool_result：跳过，同 Claude 对 tool_result 的处理
        }
    }

    // 模型不在 chat_history.jsonl 里，在同目录的 summary.json（`current_model_id`）。
    let model = fs
        .read_to_string(&session_dir.join("summary.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|summary| {
            summary
                .get("current_model_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });

    Some(SessionDetail { turns, model })
}

// ===================== Copilot =====================
//
// `~/.copilot/session-state/<session_id>/`：`workspace.yaml`（10 来行的扁平
// `key: value`，没有嵌套/列表，手写小解析器就够，不为这一个文件引入 yaml 依赖）
// 给 cwd/标题/时间，`events.jsonl` 才是完整对话内容。

fn copilot_sessions_root(override_dir: Option<&str>) -> PathBuf {
    copilot_home(override_dir).join("session-state")
}

/// `COPILOT_HOME` 优先（官方推荐用法，整段替换默认 `~/.copilot`），没设再看
/// `XDG_CONFIG_HOME`（这种情况下基准目录是 `$XDG_CONFIG_HOME/copilot`），
/// 都没设才落到默认位置。`override_dir`（多 workspace profile）优先级最高，
/// 直接就是完整的 Copilot 数据目录（不用再拼 `/copilot` 子目录）。
fn copilot_home(override_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = override_dir {
        return PathBuf::from(dir);
    }
    if let Some(dir) = crate::login_env::copilot_home() {
        return PathBuf::from(dir);
    }
    if let Some(xdg) = crate::login_env::xdg_config_home() {
        return PathBuf::from(xdg).join("copilot");
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".copilot")
}

/// 只认得住扁平 `key: value` 这一种形状——Copilot 目前这个文件就是这样（实测），
/// 真出现嵌套/列表会直接读不到对应字段，调用方本来就都用 `Option`/回退处理。
fn parse_flat_yaml(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

pub fn list_copilot_sessions(cwd: &str, override_dir: Option<&str>) -> Vec<SessionSummary> {
    list_copilot_sessions_with(&LocalFs, cwd, override_dir)
}

pub fn list_copilot_sessions_with(
    fs: &dyn FileSystem,
    cwd: &str,
    override_dir: Option<&str>,
) -> Vec<SessionSummary> {
    let root = copilot_sessions_root(override_dir);
    let mut out = Vec::new();
    for session_dir in read_dir_ok(fs, &root) {
        if let Some(s) = summarize_copilot_session_with(fs, &session_dir, cwd) {
            out.push(s);
        }
    }
    out.sort_by_key(|item| std::cmp::Reverse(item.last_active_at));
    out
}

fn summarize_copilot_session_with(
    fs: &dyn FileSystem,
    session_dir: &Path,
    want_cwd: &str,
) -> Option<SessionSummary> {
    let yaml_text = fs
        .read_to_string(&session_dir.join("workspace.yaml"))
        .ok()?;
    let fields = parse_flat_yaml(&yaml_text);
    if fields.get("cwd").map(String::as_str) != Some(want_cwd) {
        return None;
    }
    let session_id = session_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let title = fields
        .get("summary")
        .or_else(|| fields.get("name"))
        .filter(|s| !s.trim().is_empty())
        .map(|s| truncate(s, 80))
        .unwrap_or_else(|| session_id.clone());

    // workspace.yaml 没存消息数，要拿到它就得扫一遍 events.jsonl。
    let mut message_count = 0usize;
    if let Ok(text) = fs.read_to_string(&session_dir.join("events.jsonl")) {
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            match row.get("type").and_then(|v| v.as_str()) {
                Some("user.message") | Some("assistant.message") => message_count += 1,
                _ => {}
            }
        }
    }

    Some(SessionSummary {
        path: session_dir.to_path_buf(),
        title: title.clone(),
        agent_title: title,
        custom_title: None,
        resume_id: session_id,
        started_at: fields.get("created_at").and_then(|v| parse_rfc3339(v)),
        last_active_at: fields.get("updated_at").and_then(|v| parse_rfc3339(v)),
        message_count,
        total_tokens: 0, // events.jsonl 没有可靠的整会话 token 汇总字段（实测）
    })
}

pub fn load_copilot_session_detail(session_dir: &Path) -> Option<SessionDetail> {
    load_copilot_session_detail_with(&LocalFs, session_dir)
}

pub fn load_copilot_session_detail_with(
    fs: &dyn FileSystem,
    session_dir: &Path,
) -> Option<SessionDetail> {
    let text = fs.read_to_string(&session_dir.join("events.jsonl")).ok()?;
    let mut turns: Vec<Turn> = Vec::new();
    let mut model = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(data) = row.get("data") else {
            continue;
        };
        match row.get("type").and_then(|v| v.as_str()) {
            Some("user.message") => {
                // `content` 是用户原始打字；`transformedContent` 是 CLI 拼进 IDE 选区
                // 之类上下文之后的版本，混进去展示会很乱，只取干净的那份。
                let Some(text) = data.get("content").and_then(|v| v.as_str()) else {
                    continue;
                };
                if text.trim().is_empty() {
                    continue;
                }
                turns.push(Turn::message(true, None, text.to_string()));
            }
            Some("assistant.message") => {
                // 同一份会话里模型可能换过（实测有 claude-sonnet-5 / claude-opus-5 /
                // gpt-5.6-terra 混着的），交接头部要交代的是「最后在用哪个」，所以
                // 每条都覆盖，取最后一次。
                if let Some(m) = data.get("model").and_then(|v| v.as_str()) {
                    model = Some(m.to_string());
                }
                let text = data
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let mut tools: Vec<String> = Vec::new();
                let mut tool_paths: Vec<String> = Vec::new();
                for req in data
                    .get("toolRequests")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(name) = req.get("name").and_then(|n| n.as_str()) {
                        tools.push(name.to_string());
                    }
                    // Copilot 的 arguments 已经是对象（不像 Grok 是 JSON 字符串）。
                    if let Some(args) = req.get("arguments") {
                        push_unique_paths(&mut tool_paths, tool_path_args(args));
                    }
                }
                if text.trim().is_empty() && tools.is_empty() {
                    continue;
                }
                turns.push(Turn {
                    is_user: false,
                    timestamp: None,
                    text,
                    tools,
                    tool_paths,
                });
            }
            _ => {} // tool.execution_*/hook.*/session.*/system.*：跳过，工具名已从 toolRequests 拿到
        }
    }

    Some(SessionDetail { turns, model })
}

/// Cursor CLI 本地历史尚未接入；先返回空列表，ACP 续接仍走协议自身的
/// `session/load`。
pub fn list_cursor_sessions(_cwd: &str, _override_dir: Option<&str>) -> Vec<SessionSummary> {
    Vec::new()
}

/// Cursor CLI 本地历史尚未接入；避免猜错文件格式。
pub fn load_cursor_session_detail(_session_dir: &Path) -> Option<SessionDetail> {
    None
}

/// OpenCode 本地历史尚未接入；先返回空列表，ACP 续接仍走协议自身的
/// `session/load`。
pub fn list_opencode_sessions(_cwd: &str, _override_dir: Option<&str>) -> Vec<SessionSummary> {
    Vec::new()
}

/// OpenCode 本地历史尚未接入；避免猜错文件格式。
pub fn load_opencode_session_detail(_session_dir: &Path) -> Option<SessionDetail> {
    None
}

/// Kiro CLI 本地历史尚未接入；先返回空列表，ACP 续接仍走协议自身的
/// `session/load`。官方文档写会话在 `~/.kiro/sessions/cli/`，格式确认后再接。
pub fn list_kiro_sessions(_cwd: &str, _override_dir: Option<&str>) -> Vec<SessionSummary> {
    Vec::new()
}

/// Kiro CLI 本地历史尚未接入；避免猜错文件格式。
pub fn load_kiro_session_detail(_session_dir: &Path) -> Option<SessionDetail> {
    None
}

// ===================== Pi =====================
//
// Pi 的会话是带树形 parentId 的 JSONL 文件，默认位于
// `~/.pi/agent/sessions/--<cwd>--/*.jsonl`。这里按文件头里的 cwd 过滤，而不是
// 依赖目录名编码；这样也能读 `sessionDir` 自定义位置和未来改变编码规则的存档。

fn pi_agent_dir(override_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = override_dir.filter(|dir| !dir.trim().is_empty()) {
        return PathBuf::from(crate::workspace_override::expand_tilde(dir));
    }
    std::env::var("PI_CODING_AGENT_DIR")
        .ok()
        .filter(|dir| !dir.trim().is_empty())
        .map(|dir| PathBuf::from(crate::workspace_override::expand_tilde(&dir)))
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".pi")
                .join("agent")
        })
}

fn pi_resolve_configured_path(base: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(crate::workspace_override::expand_tilde(raw.trim()));
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn pi_sessions_dir(fs: &dyn FileSystem, override_dir: Option<&str>) -> PathBuf {
    let agent_dir = pi_agent_dir(override_dir);
    // The environment override is intentionally ignored only for injected test/profile
    // roots. A profile-specific root is already the strongest caller-provided setting.
    if override_dir.is_none()
        && let Ok(session_dir) = std::env::var("PI_CODING_AGENT_SESSION_DIR")
        && !session_dir.trim().is_empty()
    {
        return pi_resolve_configured_path(&agent_dir, &session_dir);
    }
    let settings_path = agent_dir.join("settings.json");
    let configured = fs
        .read_to_string(&settings_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|settings| {
            settings
                .get("sessionDir")
                .and_then(Value::as_str)
                .filter(|dir| !dir.trim().is_empty())
                .map(|dir| pi_resolve_configured_path(&agent_dir, dir))
        });
    configured.unwrap_or_else(|| agent_dir.join("sessions"))
}

fn walk_pi_session_files(fs: &dyn FileSystem, dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    // A malformed symlink tree must not turn a history refresh into unbounded recursion.
    if depth > 64 {
        return;
    }
    for entry in fs.read_dir(dir).unwrap_or_default() {
        let is_symlink = fs
            .symlink_metadata(&entry.path)
            .is_some_and(|meta| meta.is_symlink);
        if is_symlink {
            continue;
        }
        if entry.is_dir {
            walk_pi_session_files(fs, &entry.path, depth + 1, files);
        } else if entry.path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            files.push(entry.path);
        }
    }
}

fn pi_timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> {
    match value {
        Some(Value::String(value)) => parse_rfc3339(value),
        Some(Value::Number(value)) => value
            .as_i64()
            .and_then(DateTime::from_timestamp_millis)
            .or_else(|| {
                value
                    .as_u64()
                    .and_then(|ms| i64::try_from(ms).ok())
                    .and_then(DateTime::from_timestamp_millis)
            }),
        _ => None,
    }
}

fn pi_entry_timestamp(entry: &Value) -> Option<DateTime<Utc>> {
    pi_timestamp(entry.get("timestamp")).or_else(|| {
        entry
            .get("message")
            .and_then(|message| pi_timestamp(message.get("timestamp")))
    })
}

fn pi_session_header(text: &str) -> Option<(String, String, Option<DateTime<Utc>>)> {
    let header = serde_json::from_str::<Value>(text.lines().next()?.trim()).ok()?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let id = header.get("id").and_then(Value::as_str)?.trim();
    let cwd = header.get("cwd").and_then(Value::as_str)?.trim();
    if id.is_empty() || cwd.is_empty() {
        return None;
    }
    Some((
        id.to_string(),
        cwd.to_string(),
        pi_timestamp(header.get("timestamp")),
    ))
}

fn pi_content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.to_string(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn pi_tool_call_parts(block: &Value) -> Option<(String, Value)> {
    let name = block
        .get("name")
        .or_else(|| block.get("toolName"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())?
        .to_string();
    let args = block
        .get("arguments")
        .or_else(|| block.get("input"))
        .cloned()
        .unwrap_or(Value::Null);
    let args = if let Value::String(raw) = args {
        serde_json::from_str(&raw).unwrap_or(Value::Null)
    } else {
        args
    };
    Some((name, args))
}

fn pi_assistant_parts(message: &Value) -> (String, Vec<String>, Vec<String>) {
    let mut text = String::new();
    let mut tools = Vec::new();
    let mut paths = Vec::new();
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        return (pi_content_text(message.get("content")), tools, paths);
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(value) = block.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(value);
                }
            }
            Some("toolCall") | Some("tool_call") => {
                if let Some((name, args)) = pi_tool_call_parts(block) {
                    tools.push(name);
                    push_unique_paths(&mut paths, tool_path_args(&args));
                }
            }
            _ => {}
        }
    }
    (text, tools, paths)
}

fn pi_usage_total(usage: &Value) -> u64 {
    let fields = ["input", "output", "cacheRead", "cacheWrite"];
    let mut total = 0u64;
    let mut found = false;
    for field in fields {
        if let Some(value) = usage.get(field).and_then(Value::as_u64) {
            found = true;
            total = total.saturating_add(value);
        }
    }
    if found {
        total
    } else {
        usage
            .get("totalTokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }
}

fn pi_model_from_change(entry: &Value) -> Option<String> {
    let model = entry.get("modelId").and_then(Value::as_str)?.trim();
    if model.is_empty() {
        return None;
    }
    let provider = entry
        .get("provider")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty());
    Some(match provider {
        Some(provider) => format!("{provider}/{model}"),
        None => model.to_string(),
    })
}

pub fn list_pi_sessions(cwd: &str, override_dir: Option<&str>) -> Vec<SessionSummary> {
    list_pi_sessions_with(&LocalFs, cwd, override_dir)
}

pub fn list_pi_sessions_with(
    fs: &dyn FileSystem,
    cwd: &str,
    override_dir: Option<&str>,
) -> Vec<SessionSummary> {
    let mut files = Vec::new();
    walk_pi_session_files(fs, &pi_sessions_dir(fs, override_dir), 0, &mut files);
    files.sort();
    files
        .into_iter()
        .filter_map(|path| summarize_pi_session_with(fs, &path, cwd))
        .collect()
}

fn summarize_pi_session_with(
    fs: &dyn FileSystem,
    path: &Path,
    want_cwd: &str,
) -> Option<SessionSummary> {
    let text = fs.read_to_string(path).ok()?;
    let (resume_id, session_cwd, header_timestamp) = pi_session_header(&text)?;
    if session_cwd != want_cwd {
        return None;
    }

    let mut title = None;
    let mut first_user_title = None;
    let mut started_at = header_timestamp;
    let mut latest_message_at: Option<DateTime<Utc>> = None;
    let mut latest_any_at = header_timestamp;
    let mut message_count = 0usize;
    let mut total_tokens = 0u64;

    for line in text.lines().skip(1) {
        let Ok(entry) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let timestamp = pi_entry_timestamp(&entry);
        if let Some(timestamp) = timestamp {
            started_at = Some(started_at.map_or(timestamp, |current| current.min(timestamp)));
            latest_any_at = Some(latest_any_at.map_or(timestamp, |current| current.max(timestamp)));
        }
        match entry.get("type").and_then(Value::as_str) {
            Some("session_info") => {
                if let Some(name) = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                {
                    title = Some(name.to_string());
                }
            }
            Some("message") => {
                if let Some(timestamp) = timestamp {
                    latest_message_at =
                        Some(latest_message_at.map_or(timestamp, |current| current.max(timestamp)));
                }
                let Some(message) = entry.get("message") else {
                    continue;
                };
                match message.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        let text = pi_content_text(message.get("content"));
                        if !text.trim().is_empty() {
                            message_count += 1;
                            if first_user_title.is_none() {
                                first_user_title = Some(text);
                            }
                        }
                    }
                    Some("assistant") => {
                        let (text, tools, _) = pi_assistant_parts(message);
                        if !text.trim().is_empty() || !tools.is_empty() {
                            message_count += 1;
                        }
                        if let Some(usage) = message.get("usage") {
                            total_tokens = total_tokens.saturating_add(pi_usage_total(usage));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let title = title
        .or(first_user_title)
        .map(|title| truncate(title.trim(), 80))
        .unwrap_or_else(|| resume_id.clone());
    Some(SessionSummary {
        path: path.to_path_buf(),
        title: title.clone(),
        agent_title: title,
        custom_title: None,
        resume_id,
        started_at,
        last_active_at: latest_message_at.or(latest_any_at),
        message_count,
        total_tokens,
    })
}

pub fn load_pi_session_detail(path: &Path) -> Option<SessionDetail> {
    load_pi_session_detail_with(&LocalFs, path)
}

pub fn load_pi_session_detail_with(fs: &dyn FileSystem, path: &Path) -> Option<SessionDetail> {
    let text = fs.read_to_string(path).ok()?;
    parse_pi_session_detail(&text)
}

fn parse_pi_session_detail(text: &str) -> Option<SessionDetail> {
    pi_session_header(text)?;
    let mut turns = Vec::new();
    let mut model = None;

    for line in text.lines().skip(1) {
        let Ok(entry) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match entry.get("type").and_then(Value::as_str) {
            Some("model_change") => {
                if let Some(value) = pi_model_from_change(&entry) {
                    model = Some(value);
                }
            }
            Some("message") => {
                let Some(message) = entry.get("message") else {
                    continue;
                };
                let timestamp = pi_entry_timestamp(&entry);
                match message.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        let text = pi_content_text(message.get("content"));
                        if !text.trim().is_empty() {
                            turns.push(Turn::message(true, timestamp, text));
                        }
                    }
                    Some("assistant") => {
                        if let Some(value) = message.get("model").and_then(Value::as_str)
                            && !value.trim().is_empty()
                        {
                            model = Some(value.to_string());
                        }
                        let (text, tools, tool_paths) = pi_assistant_parts(message);
                        if text.trim().is_empty() && tools.is_empty() {
                            continue;
                        }
                        turns.push(Turn {
                            is_user: false,
                            timestamp,
                            text,
                            tools,
                            tool_paths,
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Some(SessionDetail { turns, model })
}

// ===================== 交接 =====================

/// 历史 transcript → 交接轮次（`crate::session_handoff` 的中立 IR）。这是当前唯一
/// 的文本交接来源，生成的轮次统一交给同一个摘要渲染器。
///
/// 各家 transcript 本来就没有工具的类别/状态（`detail` 恒为 `None`），图片也只会
/// 落盘引用或干脆不落（`images` 恒为 0）。
/// 一轮里的工具名列表 → 一行摘要，连续同名的折成 `名字 ×n`。
///
/// 实测一份 398 轮的 Codex 会话里，一轮连着调五次 `wait` 很常见，原样列出来是
/// 「wait、wait、wait、wait、wait」——占预算却不多给一点信息。
fn summarize_tool_names(names: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut count = 1usize;
    for (ix, name) in names.iter().enumerate() {
        if names.get(ix + 1) == Some(name) {
            count += 1;
            continue;
        }
        parts.push(if count > 1 {
            format!("{name} ×{count}")
        } else {
            name.clone()
        });
        count = 1;
    }
    parts.join("、")
}

pub fn handoff_turns_from_history(detail: &SessionDetail) -> Vec<HandoffTurn> {
    let mut turns = Vec::new();
    for turn in &detail.turns {
        if turn.is_user {
            if !turn.text.trim().is_empty() {
                turns.push(HandoffTurn::User {
                    text: turn.text.clone(),
                    images: 0,
                });
            }
            continue;
        }
        if !turn.text.trim().is_empty() {
            turns.push(HandoffTurn::Assistant(turn.text.clone()));
        }
        // 一轮里的工具合成一条：历史侧没有逐个工具的状态，拆开写只是把同样的
        // 信息摊得更长，白占 prompt 预算。
        if !turn.tools.is_empty() {
            turns.push(HandoffTurn::Tool {
                title: summarize_tool_names(&turn.tools),
                detail: None,
                paths: turn.tool_paths.clone(),
            });
        }
    }
    turns
}

// ---- DeepSeek Harness (dsh) ------------------------------------------------

/// dsh 的 JSONL 持久化把项目目录名编码成 `--<slug>--`：路径分隔符（`/` `\` `:`）
/// 连续压成一个 `-`，`[A-Za-z0-9._-]` 之外的字符转义成 `~XXXX`（大写十六进制、
/// 补足四位），开头的 `-` 全部去掉，空串退化成 `root`，再截断到 251 字节。
///
/// 这里是重实现而不是扫目录反查：会话根下同一个 cwd 只有一个项目目录，直接算出
/// 名字比列一层目录再逐个解码便宜，也不会被同级的无关目录干扰。规则跟着上游
/// `packages/session/session-persistence-jsonl/src/format.ts` 的 `projectKey`，
/// 是**存档格式契约**，改了等于认不出既有历史。
fn dsh_project_key(cwd: &str) -> String {
    let mut readable = String::new();
    let mut separator_run = false;
    for ch in cwd.chars() {
        if ch == '/' || ch == '\\' || ch == ':' {
            if !separator_run {
                readable.push('-');
            }
            separator_run = true;
        } else if ch != '~' && (ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-') {
            readable.push(ch);
            separator_run = false;
        } else {
            // 上游用 `charCodeAt`，也就是 UTF-16 码元；ASCII 之外必须按码元逐个
            // 转义，否则中文路径算出来的目录名对不上。
            let mut buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut buf) {
                readable.push_str(&format!("~{:04X}", unit));
            }
            separator_run = false;
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    let slug: String = slug.chars().take(251).collect();
    format!("--{slug}--")
}

/// 会话目录名用同一套 `~XXXX` 转义；解回 session id 才能拿去发 `session/load`。
/// 解不出（畸形转义）就返回 None，跳过这一条而不是猜。
fn dsh_decode_segment(segment: &str) -> Option<String> {
    if !segment.contains('~') {
        return Some(segment.to_string());
    }
    let mut units: Vec<u16> = Vec::new();
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'~' {
            let hex = segment.get(i + 1..i + 5)?;
            units.push(u16::from_str_radix(hex, 16).ok()?);
            i += 5;
        } else {
            // 未转义的一定是 ASCII 安全字符，按单字节推进不会切断多字节序列。
            units.push(bytes[i] as u16);
            i += 1;
        }
    }
    String::from_utf16(&units).ok()
}

/// dsh 的默认会话根由原生 DSH_HOME 决定；`override_dir` 给显式 persistence
/// 根使用，原样当作根。
fn dsh_sessions_root(cwd: &str, override_dir: Option<&str>) -> PathBuf {
    match override_dir {
        Some(dir) => PathBuf::from(dir),
        None => crate::agent_kind::dsh_sessions_root()
            .unwrap_or_else(|| Path::new(cwd).join(".sessions")),
    }
}

/// 新版 dsh 事件把消息内容平铺在 `data` 下（顶层只有 type/seq/time），老档
/// 直接平铺在顶层，两处都读。
fn dsh_event_data(event: &Value) -> &Value {
    event.get("data").unwrap_or(event)
}

/// 事件时间戳（毫秒）：新版用 `time`，老档用 `timestamp`。
fn dsh_event_timestamp(event: &Value) -> Option<i64> {
    event
        .get("time")
        .or_else(|| event.get("timestamp"))
        .and_then(Value::as_i64)
}

/// 真实用户消息才算一轮；agent 注入的 system-reminder / 运行时上下文等
/// `user/message` 也带 `source.kind`，不该刷进历史。老档没有 `source` 视为真。
fn dsh_is_real_user_message(event: &Value) -> bool {
    match dsh_event_data(event)
        .get("source")
        .and_then(|s| s.get("kind"))
        .and_then(Value::as_str)
    {
        Some("user") | None => true,
        Some(_) => false,
    }
}

/// 列出某个工作目录下的 dsh 会话。
///
pub fn list_dsh_sessions(cwd: &str, override_dir: Option<&str>) -> Vec<SessionSummary> {
    let project = dsh_sessions_root(cwd, override_dir).join(dsh_project_key(cwd));
    let mut out = std::fs::read_dir(project)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let session_dir = entry.path();
            let (log_path, text) = read_local_dsh_log(&session_dir)?;
            summarize_dsh_session(&session_dir, log_path, &text, cwd)
        })
        .collect::<Vec<_>>();
    out.sort_by_key(|session| Reverse(session.last_active_at));
    out
}

pub fn list_dsh_sessions_with(
    fs: &dyn FileSystem,
    cwd: &str,
    override_dir: Option<&str>,
) -> Vec<SessionSummary> {
    let project = dsh_sessions_root(cwd, override_dir).join(dsh_project_key(cwd));
    let mut out = Vec::new();
    for session_dir in read_dir_ok(fs, &project) {
        if let Some(summary) = summarize_dsh_session_with(fs, &session_dir, cwd) {
            out.push(summary);
        }
    }
    out.sort_by_key(|s| Reverse(s.last_active_at));
    out
}

fn summarize_dsh_session_with(
    fs: &dyn FileSystem,
    session_dir: &Path,
    want_cwd: &str,
) -> Option<SessionSummary> {
    let log_path = session_dir.join("session.jsonl");
    let text = fs.read_to_string(&log_path).ok()?;
    summarize_dsh_session(session_dir, log_path, &text, want_cwd)
}

fn read_local_dsh_log(session_dir: &Path) -> Option<(PathBuf, String)> {
    let plain = session_dir.join("session.jsonl");
    if let Ok(text) = std::fs::read_to_string(&plain) {
        return Some((plain, text));
    }
    let compressed = session_dir.join("session.jsonl.zstd");
    let bytes = std::fs::read(&compressed).ok()?;
    let decoded = zstd::stream::decode_all(bytes.as_slice()).ok()?;
    String::from_utf8(decoded)
        .ok()
        .map(|text| (compressed, text))
}

fn summarize_dsh_session(
    session_dir: &Path,
    log_path: PathBuf,
    text: &str,
    want_cwd: &str,
) -> Option<SessionSummary> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Value = serde_json::from_str(lines.next()?).ok()?;
    if header.get("type").and_then(|v| v.as_str()) != Some("session") {
        return None;
    }
    // 头里带 cwd 就以它为准；老档没有这个字段时退回目录归属，反正项目目录就是
    // 按 cwd 算出来的。
    if let Some(logged) = header.get("cwd").and_then(|v| v.as_str())
        && logged != want_cwd
    {
        return None;
    }
    let resume_id = header
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            session_dir
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(dsh_decode_segment)
        })?;
    let started_at = header
        .get("createdAt")
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis);

    let mut title = String::new();
    let mut message_count = 0usize;
    let mut total_tokens = 0u64;
    let mut last_active_at = started_at;
    for line in lines {
        let Ok(event): Result<Value, _> = serde_json::from_str(line) else {
            continue; // 半行（进程被杀在写盘中间）不该让整份会话消失
        };
        if let Some(ms) = dsh_event_timestamp(&event)
            && let Some(at) = DateTime::from_timestamp_millis(ms)
        {
            last_active_at = Some(at);
        }
        match event.get("type").and_then(|v| v.as_str()) {
            Some("user/message") if dsh_is_real_user_message(&event) => {
                message_count += 1;
                if title.is_empty() {
                    title = dsh_message_text(&event);
                }
            }
            Some("assistant/message") => {
                message_count += 1;
                total_tokens += dsh_usage_total(&event);
            }
            Some("session/title") => {
                if let Some(t) = dsh_event_data(&event).get("title").and_then(Value::as_str)
                    && !t.trim().is_empty()
                {
                    title = t.to_string();
                }
            }
            _ => {}
        }
    }
    // 空会话（开了没说话）退回 session id，和 Grok 那版一个口径——列表里
    // 一片同名的「未命名会话」区分不开，id 至少还能对上。
    let agent_title = if title.trim().is_empty() {
        resume_id.clone()
    } else {
        truncate(title.trim(), 80)
    };
    Some(SessionSummary {
        path: log_path,
        title: agent_title.clone(),
        agent_title,
        custom_title: None,
        resume_id,
        started_at,
        last_active_at,
        message_count,
        total_tokens,
    })
}

/// 读一份 dsh 会话的正文，用于历史预览与会话交接。
///
/// `path` 是 `list_dsh_sessions` 给出的 `session.jsonl` 本身（不是目录），跟
/// Claude 那版一样，其他几家给的是目录——形状不统一是既有约定，`SessionSummary`
/// 的 `path` 注释里已经写明。
pub fn load_dsh_session_detail(path: &Path) -> Option<SessionDetail> {
    let text = if path.extension().and_then(|extension| extension.to_str()) == Some("zstd") {
        let bytes = std::fs::read(path).ok()?;
        String::from_utf8(zstd::stream::decode_all(bytes.as_slice()).ok()?).ok()?
    } else {
        std::fs::read_to_string(path).ok()?
    };
    parse_dsh_session_detail(&text)
}

pub fn load_dsh_session_detail_with(fs: &dyn FileSystem, path: &Path) -> Option<SessionDetail> {
    let text = fs.read_to_string(path).ok()?;
    parse_dsh_session_detail(&text)
}

fn parse_dsh_session_detail(text: &str) -> Option<SessionDetail> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut model: Option<String> = None;
    // 一轮里的工具调用要挂到"上一条 assistant 文本"上，和其他几家的口径一致；
    // dsh 的 `tool/call` 是独立事件，不是消息里的块。
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let timestamp = dsh_event_timestamp(&row).and_then(DateTime::from_timestamp_millis);
        match row.get("type").and_then(|v| v.as_str()) {
            Some("user/message") => {
                // 合成注入的 user/message 不展示，只留真实用户轮。
                if !dsh_is_real_user_message(&row) {
                    continue;
                }
                let text = dsh_message_text(&row);
                if text.trim().is_empty() {
                    continue;
                }
                turns.push(Turn::message(true, timestamp, text));
            }
            Some("assistant/message") => {
                if model.is_none() {
                    model = dsh_message_model(&row);
                }
                turns.push(Turn::message(false, timestamp, dsh_message_text(&row)));
            }
            Some("tool/call") => {
                let Some((name, args)) = dsh_tool_call_parts(&row) else {
                    continue;
                };
                // 工具挂到最近一条 assistant 轮；没有的话补一条空的，免得
                // "这段动过哪些文件"因为文本为空就整段丢失。
                if !turns.last().is_some_and(|turn| !turn.is_user) {
                    turns.push(Turn::message(false, timestamp, String::new()));
                }
                let Some(turn) = turns.last_mut() else {
                    continue;
                };
                turn.tools.push(name);
                push_unique_paths(&mut turn.tool_paths, tool_path_args(&args));
            }
            _ => {}
        }
    }
    turns.retain(|turn| !turn.text.trim().is_empty() || !turn.tools.is_empty());
    Some(SessionDetail { turns, model })
}

/// 取一条 dsh 消息里的纯文本。新版正文在 `data.message.content`（assistant）或
/// `data.content`（user），老档平铺在 `message.content`；`content` 是 ContentBlock
/// 数组，图片/工具块没有 `text`，跳过即可。
fn dsh_message_text(event: &Value) -> String {
    let data = dsh_event_data(event);
    let Some(blocks) = data
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .or_else(|| data.get("content").and_then(Value::as_array))
    else {
        return String::new();
    };
    let mut out = String::new();
    for block in blocks {
        if block.get("type").and_then(|v| v.as_str()) == Some("text")
            && let Some(text) = block.get("text").and_then(|v| v.as_str())
        {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

/// 新版模型信息在 `data.message.source.model`，老档在 `message.model`/`model`。
fn dsh_message_model(event: &Value) -> Option<String> {
    let data = dsh_event_data(event);
    data.get("message")
        .and_then(|m| m.get("source"))
        .and_then(|s| s.get("model"))
        .or_else(|| data.get("message").and_then(|m| m.get("model")))
        .or_else(|| data.get("model"))
        .or_else(|| event.get("message").and_then(|m| m.get("model")))
        .or_else(|| event.get("model"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 一条 `tool/call` 的名字与参数。新版参数是 JSON 字符串（`data.arguments`），
/// 老档直接是对象（`args`）。
fn dsh_tool_call_parts(event: &Value) -> Option<(String, Value)> {
    let data = dsh_event_data(event);
    let name = data.get("name").and_then(Value::as_str)?.to_string();
    let args = if let Some(json) = data.get("arguments").and_then(Value::as_str) {
        serde_json::from_str(json).unwrap_or(Value::Null)
    } else {
        data.get("args").cloned().unwrap_or(Value::Null)
    };
    Some((name, args))
}

/// dsh 的四个计数是**互斥**的（缓存命中单独记，不折进 `inputTokens`），
/// 所以「本会话总量」是四项相加，不会重复计。
fn dsh_usage_total(event: &Value) -> u64 {
    let data = dsh_event_data(event);
    let Some(usage) = data
        .get("usage")
        .or_else(|| event.get("usage"))
        .or_else(|| data.get("message").and_then(|m| m.get("usage")))
    else {
        return 0;
    };
    [
        "inputTokens",
        "outputTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
    ]
    .iter()
    .filter_map(|key| usage.get(*key).and_then(Value::as_u64))
    .sum()
}

// ===================== Antigravity =====================

/// Antigravity 的时间戳形如 `2026-09-17 01:34:53.561993+00:00`——用空格分隔日期与
/// 时间，不是 RFC3339 的 `T`，直接喂 `parse_rfc3339` 会全部解析失败、导致列表
/// 排序和「跑了多久」全空。这里只补这一处形状差异，其余交给标准解析。
fn parse_antigravity_time(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    parse_rfc3339(raw).or_else(|| parse_rfc3339(&raw.replacen(' ', "T", 1)))
}

/// 列出 Antigravity 在本机存下的、属于该项目的会话。
///
/// 列表只读总索引库，不去翻每个会话的正文库：正文库单个可以到几十 MB，为了画
/// 一行列表去解析它们会让历史页卡住。代价是 `message_count` 用索引里的步数口径。
pub fn list_antigravity_sessions(cwd: &str, override_dir: Option<&str>) -> Vec<SessionSummary> {
    let mut out: Vec<SessionSummary> =
        crate::antigravity_history::list_conversations(override_dir, cwd)
            .into_iter()
            .map(|row| {
                let last_active_at = row
                    .last_modified
                    .as_deref()
                    .and_then(parse_antigravity_time);
                // 索引里没有「会话开始时间」，只有最后一次用户输入时间。拿它当
                // started_at 是诚实的近似：单轮会话两者相等，多轮会话则会低估
                // 时长，好过凭空捏一个开始时间。
                let started_at = row
                    .last_user_input
                    .as_deref()
                    .and_then(parse_antigravity_time)
                    .filter(|start| last_active_at.is_none_or(|last| *start <= last));
                // title 常常是空的，agy 自己的列表这时显示 preview。
                let raw_title = if row.title.trim().is_empty() {
                    row.preview.trim()
                } else {
                    row.title.trim()
                };
                let agent_title = if raw_title.is_empty() {
                    row.conversation_id.clone()
                } else {
                    truncate(raw_title, 80)
                };
                SessionSummary {
                    path: crate::antigravity_history::conversation_db(
                        override_dir,
                        &row.conversation_id,
                    ),
                    title: agent_title.clone(),
                    agent_title,
                    custom_title: None,
                    resume_id: row.conversation_id,
                    started_at,
                    last_active_at,
                    message_count: row.step_count.max(0) as usize,
                    // agy 的索引不记 token 用量，不编一个数字出来。
                    total_tokens: 0,
                }
            })
            .collect();
    out.sort_by_key(|item| Reverse(item.last_active_at));
    out
}

/// 读一份 Antigravity 会话的正文。`path` 是 `conversations/<id>.db` 本身。
pub fn load_antigravity_session_detail(path: &Path) -> Option<SessionDetail> {
    let turns = crate::antigravity_history::read_turns(path)?;
    Some(SessionDetail {
        turns: turns
            .into_iter()
            .map(|turn| Turn {
                is_user: turn.is_user,
                // 每步的时间戳藏在 protobuf 里且各步形状不一，索引层面拿不到；
                // 与其猜一个，不如不给——展示层对 None 有兜底。
                timestamp: None,
                text: turn.text,
                tools: turn.tools,
                tool_paths: turn.tool_paths,
            })
            .collect(),
        // 模型名是每步生成配置的一部分，不在会话级元数据里，读不到就不写。
        model: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemFs;

    fn write(fs: &dyn FileSystem, path: &Path, contents: &str) {
        fs.create_dir_all(path.parent().unwrap()).unwrap();
        fs.write(path, contents).unwrap();
    }

    #[test]
    fn history_providers_cover_every_source_once() {
        let registered = SESSION_HISTORY_PROVIDERS
            .iter()
            .map(|provider| provider.agent)
            .collect::<Vec<_>>();
        assert_eq!(registered, HistorySourceKind::ALL);
    }

    /// 历史来源是 ACP 种类的超集：每个 ACP agent 都得能读历史，但反过来不成立。
    /// 这条测试防的是「新增 ACP agent 却忘了注册历史 Provider」。
    #[test]
    fn every_acp_agent_has_a_history_source() {
        for kind in ConversationAgentKind::ALL {
            let source = HistorySourceKind::from(kind);
            assert_eq!(source.acp(), Some(kind), "{}", kind.id());
            // 没注册会在这里 panic。
            history_provider(source);
        }
    }

    /// Antigravity 只有 TUI，它必须能读历史、但不能被当成 ACP 对象。
    /// 这条测试钉住本次解耦的核心诉求：两个能力互相独立。
    #[test]
    fn terminal_only_source_reads_history_but_has_no_acp() {
        let source = HistorySourceKind::TerminalOnly(TerminalAgentKind::Antigravity);
        assert!(
            HistorySourceKind::ALL.contains(&source),
            "应已登记为历史来源"
        );
        assert!(source.is_bare_kind(), "应能作为裸种类出现在历史 tab");
        assert_eq!(source.acp(), None, "没有 ACP，不能当迁移目标");
        assert_eq!(source.terminal(), Some(TerminalAgentKind::Antigravity));
        assert_eq!(source.id(), "antigravity");
        assert!(history_provider(source).acp_session_index().is_none());
    }

    #[test]
    fn acp_live_index_is_a_provider_capability() {
        for agent in ConversationAgentKind::ALL {
            let index = history_provider(agent.into()).acp_session_index();
            assert_eq!(
                index.is_some(),
                agent == ConversationAgentKind::Dsh,
                "{}",
                agent.id()
            );
            if let Some(index) = index {
                let path = index.path(Some("team"), "session/a\\b");
                assert_eq!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("session_a_b")
                );
            }
        }
    }

    /// 项目目录名是存档格式契约（上游 `projectKey`）：分隔符压成一个 `-`、
    /// 开头的 `-` 去掉、非安全字符按 UTF-16 码元转义。算错了等于认不出历史。
    #[test]
    fn dsh_project_key_follows_the_upstream_encoding() {
        assert_eq!(
            dsh_project_key("/workspace/project"),
            "--workspace-project--"
        );
        // 连续分隔符只产生一个 `-`；Windows 盘符里的 `:` 同样算分隔符。
        assert_eq!(dsh_project_key("C:\\a\\\\b"), "--C-a-b--");
        // 全是分隔符时 slug 被 trim 空，退化成 root。
        assert_eq!(dsh_project_key("/"), "--root--");
        // 非 ASCII 按 UTF-16 码元逐个转义（"中" = U+4E2D）。
        assert_eq!(dsh_project_key("/中"), "--~4E2D--");
    }

    #[test]
    fn dsh_segment_decoding_round_trips_and_refuses_garbage() {
        assert_eq!(dsh_decode_segment("plain-id"), Some("plain-id".to_string()));
        assert_eq!(dsh_decode_segment("a~002Fb"), Some("a/b".to_string()));
        // 截断的转义序列宁可整条跳过，也不要猜出一个错的 session id。
        assert_eq!(dsh_decode_segment("a~00"), None);
    }

    #[test]
    fn dsh_sessions_are_listed_and_read_from_the_workspace_log() {
        let fs = MemFs::new();
        let cwd = "/workspace/project";
        let root = "/sessions-root";
        let log = PathBuf::from(root)
            .join(dsh_project_key(cwd))
            .join("sess-1")
            .join("session.jsonl");
        write(
            &fs,
            &log,
            concat!(
                r#"{"type":"session","version":1,"id":"sess-1","createdAt":1700000000000,"cwd":"/workspace/project","delegationDepth":0}"#,
                "\n",
                r#"{"type":"user/message","timestamp":1700000001000,"message":{"content":[{"type":"text","text":"给我加个测试"}]}}"#,
                "\n",
                r#"{"type":"tool/call","timestamp":1700000002000,"name":"str_replace_editor","args":{"path":"/workspace/project/src/lib.rs"}}"#,
                "\n",
                r#"{"type":"assistant/message","timestamp":1700000003000,"message":{"content":[{"type":"text","text":"加好了"},{"type":"image","data":"x"}],"model":"deepseek-v4-pro"},"usage":{"inputTokens":10,"outputTokens":5,"cacheReadTokens":3,"cacheWriteTokens":2}}"#,
                "\n",
                "{ 这行是半截 JSON",
            ),
        );

        let sessions = list_dsh_sessions_with(&fs, cwd, Some(root));
        assert_eq!(sessions.len(), 1);
        let summary = &sessions[0];
        assert_eq!(summary.resume_id, "sess-1");
        assert_eq!(summary.agent_title, "给我加个测试");
        assert_eq!(summary.message_count, 2);
        // 四个计数互斥，总量是相加：10 + 5 + 3 + 2。
        assert_eq!(summary.total_tokens, 20);
        assert_eq!(
            summary.started_at,
            DateTime::from_timestamp_millis(1700000000000)
        );
        // 半截 JSON 不该让最后一次活动时间倒退回头部时间。
        assert_eq!(
            summary.last_active_at,
            DateTime::from_timestamp_millis(1700000003000)
        );

        let detail = load_dsh_session_detail_with(&fs, &log).unwrap();
        assert_eq!(detail.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(detail.turns.len(), 3);
        assert!(detail.turns[0].is_user);
        // `tool/call` 先于 assistant 文本到达，挂在自己那一轮上。
        assert_eq!(detail.turns[1].tools, ["str_replace_editor"]);
        assert_eq!(
            detail.turns[1].tool_paths,
            ["/workspace/project/src/lib.rs"]
        );
        assert_eq!(detail.turns[2].text, "加好了");
    }

    /// 原生 deepseek-harness 实际落盘的新格式：
    /// 字段在 `data` 下、时间用 `time`、正文在 `data.message.content`，工具参数是
    /// JSON 字符串。合成注入的 user/message 不该刷成历史轮次。
    #[test]
    fn dsh_sessions_parse_the_installed_runtime_format() {
        let fs = MemFs::new();
        let cwd = "/workspace/project";
        let root = "/sessions-root";
        let log = PathBuf::from(root)
            .join(dsh_project_key(cwd))
            .join("sess-9")
            .join("session.jsonl");
        write(
            &fs,
            &log,
            concat!(
                r#"{"type":"session","version":0,"id":"sess-9","createdAt":1700000000000,"cwd":"/workspace/project","delegationDepth":0}"#,
                "\n",
                r#"{"type":"session/title","seq":8,"time":1700000000900,"data":{"title":"你好","messageSeqs":[4]}}"#,
                "\n",
                r#"{"type":"user/message","seq":4,"time":1700000001000,"data":{"id":"u-1","role":"user","content":[{"type":"text","text":"你好"}],"source":{"kind":"user"}}}"#,
                "\n",
                r#"{"type":"user/message","seq":5,"time":1700000001000,"data":{"content":[{"type":"text","text":"<system-reminder>..."}],"source":{"kind":"agent-instructions"}}}"#,
                "\n",
                r#"{"type":"assistant/message","seq":9,"time":1700000003000,"data":{"turn":1,"step":1,"message":{"role":"assistant","content":[{"type":"reasoning","text":"think"},{"type":"text","text":"加好了"}],"source":{"kind":"model","model":"deepseek-v4-flash"}},"usage":{"inputTokens":10,"outputTokens":5}}}"#,
                "\n",
                r#"{"type":"tool/call","seq":10,"time":1700000004000,"data":{"turn":1,"step":1,"callId":"c1","name":"str_replace_editor","arguments":"{\"file_path\":\"/workspace/project/src/lib.rs\"}"}}"#,
                "\n",
                r#"{"type":"turn/end","seq":11,"time":1700000005000,"data":{"turn":1,"reason":{"kind":"completed"}}}"#,
            ),
        );

        let sessions = list_dsh_sessions_with(&fs, cwd, Some(root));
        assert_eq!(sessions.len(), 1);
        let summary = &sessions[0];
        assert_eq!(summary.resume_id, "sess-9");
        // session/title 优先于首条用户文本。
        assert_eq!(summary.agent_title, "你好");
        assert_eq!(summary.message_count, 2);
        assert_eq!(summary.total_tokens, 15);
        assert_eq!(
            summary.last_active_at,
            DateTime::from_timestamp_millis(1700000005000)
        );

        let detail = load_dsh_session_detail_with(&fs, &log).unwrap();
        assert_eq!(detail.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(detail.turns.len(), 2);
        assert_eq!(detail.turns[0].text, "你好");
        // 合成 user/message 被过滤，assistant 只保留 text 块，工具挂到它上面。
        assert_eq!(detail.turns[1].text, "加好了");
        assert_eq!(detail.turns[1].tools, ["str_replace_editor"]);
        assert_eq!(
            detail.turns[1].tool_paths,
            ["/workspace/project/src/lib.rs"]
        );
    }

    /// 别的工作目录的会话不该出现在这个列表里。
    #[test]
    fn dsh_listing_rejects_a_log_belonging_to_another_cwd() {
        let fs = MemFs::new();
        let cwd = "/workspace/project";
        let root = "/sessions-root";
        let log = PathBuf::from(root)
            .join(dsh_project_key(cwd))
            .join("sess-2")
            .join("session.jsonl");
        write(
            &fs,
            &log,
            r#"{"type":"session","version":1,"id":"sess-2","createdAt":1,"cwd":"/somewhere/else","delegationDepth":0}"#,
        );
        assert!(list_dsh_sessions_with(&fs, cwd, Some(root)).is_empty());
    }

    #[test]
    fn dsh_listing_and_detail_support_the_default_zstd_log() {
        let cwd = "/workspace/project";
        let root = std::env::temp_dir().join(format!("smelt-dsh-zstd-{}", uuid::Uuid::new_v4()));
        let log = root
            .join(dsh_project_key(cwd))
            .join("sess-3")
            .join("session.jsonl.zstd");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        let source = [
            r#"{"type":"session","version":1,"id":"sess-3","createdAt":1700000000000,"cwd":"/workspace/project","delegationDepth":0}"#,
            r#"{"type":"user/message","seq":0,"time":1700000001000,"data":{"source":{"kind":"user"},"message":{"content":[{"type":"text","text":"hello"}]}}}"#,
            r#"{"type":"assistant/message","seq":1,"time":1700000002000,"data":{"message":{"content":[{"type":"text","text":"world"}]}}}"#,
        ]
        .join("\n");
        std::fs::write(
            &log,
            zstd::stream::encode_all(source.as_bytes(), 1).unwrap(),
        )
        .unwrap();

        let sessions = list_dsh_sessions(cwd, root.to_str());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].path, log);
        let detail = load_dsh_session_detail(&sessions[0].path).unwrap();
        assert_eq!(detail.turns.len(), 2);
        assert_eq!(detail.turns[0].text, "hello");
        assert_eq!(detail.turns[1].text, "world");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn all_session_parsers_use_injected_mem_provider() {
        let fs = MemFs::new();
        let cwd = "/workspace/project";

        let claude_path = PathBuf::from("/providers/claude/projects")
            .join(project_dir(cwd))
            .join("claude-id.jsonl");
        write(
            &fs,
            &claude_path,
            concat!(
                "{\"type\":\"user\",\"timestamp\":\"2026-08-17T01:00:00Z\",",
                "\"message\":{\"content\":\"Claude question\"}}\n",
                "{\"type\":\"assistant\",\"timestamp\":\"2026-08-17T01:01:00Z\",",
                "\"message\":{\"model\":\"claude-test\",\"content\":[",
                "{\"type\":\"text\",\"text\":\"Claude answer\"}]}}\n"
            ),
        );

        let codex_path = PathBuf::from("/providers/codex/sessions/2026/08/17/rollout.jsonl");
        write(
            &fs,
            &codex_path,
            &format!(
                concat!(
                    "{{\"type\":\"session_meta\",\"timestamp\":\"2026-08-17T02:00:00Z\",",
                    "\"payload\":{{\"cwd\":\"{cwd}\",\"id\":\"codex-id\"}}}}\n",
                    "{{\"type\":\"turn_context\",\"payload\":{{\"model\":\"codex-test\"}}}}\n",
                    "{{\"type\":\"response_item\",\"timestamp\":\"2026-08-17T02:01:00Z\",",
                    "\"payload\":{{\"type\":\"message\",\"role\":\"user\",",
                    "\"content\":[{{\"text\":\"Codex question\"}}]}}}}\n",
                    "{{\"type\":\"response_item\",\"timestamp\":\"2026-08-17T02:02:00Z\",",
                    "\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",",
                    "\"content\":[{{\"text\":\"Codex answer\"}}]}}}}\n"
                ),
                cwd = cwd
            ),
        );

        let grok_dir = PathBuf::from("/providers/grok/sessions/project-key/grok-id");
        write(
            &fs,
            &grok_dir.join("summary.json"),
            &format!(
                concat!(
                    "{{\"info\":{{\"cwd\":\"{cwd}\"}},\"session_summary\":\"Grok question\",",
                    "\"created_at\":\"2026-08-17T03:00:00Z\",",
                    "\"updated_at\":\"2026-08-17T03:01:00Z\",",
                    "\"num_chat_messages\":2,\"current_model_id\":\"grok-test\"}}"
                ),
                cwd = cwd
            ),
        );
        write(
            &fs,
            &grok_dir.join("chat_history.jsonl"),
            concat!(
                "{\"type\":\"user\",\"content\":[{\"text\":\"<user_query>Grok question",
                "</user_query>\"}]}\n",
                "{\"type\":\"assistant\",\"content\":\"Grok answer\"}\n"
            ),
        );

        let copilot_dir = PathBuf::from("/providers/copilot/session-state/copilot-id");
        write(
            &fs,
            &copilot_dir.join("workspace.yaml"),
            &format!(
                concat!(
                    "cwd: {cwd}\nsummary: Copilot question\n",
                    "created_at: 2026-08-17T04:00:00Z\n",
                    "updated_at: 2026-08-17T04:01:00Z\n"
                ),
                cwd = cwd
            ),
        );
        write(
            &fs,
            &copilot_dir.join("events.jsonl"),
            concat!(
                "{\"type\":\"user.message\",\"data\":{\"content\":\"Copilot question\"}}\n",
                "{\"type\":\"assistant.message\",\"data\":{\"content\":\"Copilot answer\",",
                "\"model\":\"copilot-test\"}}\n"
            ),
        );

        let claude = list_sessions_with(&fs, cwd, Some("/providers/claude"));
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].resume_id, "claude-id");
        let detail = load_session_detail_with(&fs, &claude[0].path).unwrap();
        assert_eq!(detail.turns.len(), 2);
        assert_eq!(detail.model.as_deref(), Some("claude-test"));

        let codex = list_codex_sessions_with(&fs, cwd, Some("/providers/codex"));
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].resume_id, "codex-id");
        let detail = load_codex_session_detail_with(&fs, &codex[0].path).unwrap();
        assert_eq!(detail.turns.len(), 2);
        assert_eq!(detail.model.as_deref(), Some("codex-test"));

        let grok = list_grok_sessions_with(&fs, cwd, Some("/providers/grok"));
        assert_eq!(grok.len(), 1);
        assert_eq!(grok[0].resume_id, "grok-id");
        let detail = load_grok_session_detail_with(&fs, &grok[0].path).unwrap();
        assert_eq!(detail.turns.len(), 2);
        assert_eq!(detail.model.as_deref(), Some("grok-test"));

        let copilot = list_copilot_sessions_with(&fs, cwd, Some("/providers/copilot"));
        assert_eq!(copilot.len(), 1);
        assert_eq!(copilot[0].resume_id, "copilot-id");
        let detail = load_copilot_session_detail_with(&fs, &copilot[0].path).unwrap();
        assert_eq!(detail.turns.len(), 2);
        assert_eq!(detail.model.as_deref(), Some("copilot-test"));
    }

    // 下面这批解析器用例走真实文件系统（不是 MemFs），所以要一个真目录做沙箱。
    // 与上面那个 `write` 分开：那个往注入的 `FileSystem` 写单串内容，这个往真
    // 路径按行写。
    fn write_lines(dir: &Path, name: &str, lines: &[&str]) {
        std::fs::write(dir.join(name), lines.join("\n")).unwrap();
    }

    /// 每个用例一个独立沙箱目录。落在 workspace 的 `target/` 下而不是 `/tmp`，
    /// 沿用 smelt 侧原来的做法：产物跟着 `cargo clean` 一起清掉。
    ///
    /// 目录名与 smelt 那份刻意区分：两个 crate 的测试二进制可能并行跑，共用同一
    /// 个 `remove_dir_all` + `create_dir_all` 的路径会互相踩。
    fn test_sandbox(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-artifacts/session-history-core")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn list_sessions_summarizes_title_and_counts_and_sorts_by_recency() {
        let tmp = test_sandbox("list");
        let _ = std::fs::remove_dir_all(&tmp);
        let config_dir = tmp.join(".claude");
        let proj_root = config_dir.join("projects").join(project_dir("/x/y"));
        std::fs::create_dir_all(&proj_root).unwrap();

        write_lines(
            &proj_root,
            "older.jsonl",
            &[
                r#"{"type":"user","timestamp":"2026-07-01T00:00:00Z","message":{"content":"hello there"}}"#,
                r#"{"type":"assistant","timestamp":"2026-07-01T00:00:05Z","message":{"content":[{"type":"text","text":"hi"}]}}"#,
            ],
        );
        write_lines(
            &proj_root,
            "newer.jsonl",
            &[
                r#"{"type":"user","timestamp":"2026-07-05T00:00:00Z","message":{"content":"second session"}}"#,
            ],
        );

        let sessions = list_sessions("/x/y", config_dir.to_str());
        std::fs::remove_dir_all(&tmp).unwrap();

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].path.file_stem().unwrap(), "newer");
        assert_eq!(sessions[0].title, "second session");
        assert_eq!(sessions[1].path.file_stem().unwrap(), "older");
        assert_eq!(sessions[1].message_count, 2);
    }

    #[test]
    fn list_sessions_skips_claude_local_command_only_transcripts() {
        let tmp = test_sandbox("claude-local-command");
        let _ = std::fs::remove_dir_all(&tmp);
        let config_dir = tmp.join(".claude");
        let proj_root = config_dir.join("projects").join(project_dir("/x/y"));
        std::fs::create_dir_all(&proj_root).unwrap();

        write_lines(
            &proj_root,
            "login-only.jsonl",
            &[
                r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>internal</local-command-caveat>"}}"#,
                r#"{"type":"user","message":{"content":"<command-name>/login</command-name><command-message>login</command-message><command-args></command-args>"}}"#,
                r#"{"type":"user","message":{"content":"<local-command-stdout>Login interrupted</local-command-stdout>"}}"#,
            ],
        );
        write_lines(
            &proj_root,
            "real.jsonl",
            &[
                r#"{"type":"user","message":{"content":"<command-name>/model</command-name>keep this question"}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"answer"}]}}"#,
            ],
        );

        let sessions = list_sessions("/x/y", config_dir.to_str());
        let detail = load_session_detail(&proj_root.join("real.jsonl")).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].resume_id, "real");
        assert_eq!(sessions[0].title, "keep this question");
        assert_eq!(detail.turns[0].text, "keep this question");
    }

    #[test]
    fn load_session_detail_skips_tool_result_and_sidechain() {
        let tmp = std::env::temp_dir().join("smelt-session-history-test-detail.jsonl");
        write_lines(
            tmp.parent().unwrap(),
            tmp.file_name().unwrap().to_str().unwrap(),
            &[
                r#"{"type":"user","timestamp":"2026-07-01T00:00:00Z","message":{"content":"do the thing"}}"#,
                r#"{"type":"user","timestamp":"2026-07-01T00:00:01Z","message":{"content":[{"type":"tool_result","content":"raw output"}]}}"#,
                r#"{"type":"assistant","timestamp":"2026-07-01T00:00:02Z","message":{"model":"claude-opus-5","content":[{"type":"text","text":"done"},{"type":"tool_use","name":"Bash"},{"type":"tool_use","name":"Edit","input":{"file_path":"src/main.rs","old_string":"a"}},{"type":"tool_use","name":"Read","input":{"file_path":"src/main.rs"}}]}}"#,
                r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-07-01T00:00:03Z","message":{"content":[{"type":"text","text":"subagent chatter"}]}}"#,
                // 实测目前 Claude Code CLI（含 ACP 模式）把用户发言也存成块数组，
                // 不是纯字符串——之前的代码只认字符串，会把这种真实发言当成
                // tool_result 漏掉，历史页里用户消息整个消失就是这么来的。
                r#"{"type":"user","timestamp":"2026-07-01T00:00:04Z","message":{"content":[{"type":"text","text":"block-array 格式的真实发言"}]}}"#,
            ],
        );

        let detail = load_session_detail(&tmp).unwrap();
        std::fs::remove_file(&tmp).unwrap();

        assert_eq!(detail.turns.len(), 3);
        assert!(detail.turns[0].is_user);
        assert_eq!(detail.turns[0].text, "do the thing");
        assert!(!detail.turns[1].is_user);
        assert_eq!(detail.turns[1].text, "done");
        assert_eq!(
            detail.turns[1].tools,
            vec!["Bash".to_string(), "Edit".to_string(), "Read".to_string()]
        );
        // 同一轮里 Edit 完再 Read 同一个文件，交接记录里只该出现一次
        assert_eq!(detail.turns[1].tool_paths, vec!["src/main.rs".to_string()]);
        assert!(detail.turns[2].is_user);
        assert_eq!(detail.turns[2].text, "block-array 格式的真实发言");
        assert_eq!(detail.model.as_deref(), Some("claude-opus-5"));
    }

    #[test]
    fn codex_reader_filters_by_cwd_skips_synthetic_context_and_groups_tool_calls() {
        let tmp = test_sandbox("codex");
        let _ = std::fs::remove_dir_all(&tmp);
        let config_dir = tmp.join(".codex");
        let day_dir = config_dir
            .join("sessions")
            .join("2026")
            .join("07")
            .join("01");
        std::fs::create_dir_all(&day_dir).unwrap();
        write_lines(
            &day_dir,
            "rollout-test.jsonl",
            &[
                r#"{"timestamp":"2026-07-01T00:00:00Z","type":"session_meta","payload":{"id":"cx-1","cwd":"/proj"}}"#,
                r#"{"timestamp":"2026-07-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.6-sol","cwd":"/proj"}}"#,
                r#"{"timestamp":"2026-07-01T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd stuff</environment_context>"}]}}"#,
                r#"{"timestamp":"2026-07-01T00:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"实际问题"}]}}"#,
                r#"{"timestamp":"2026-07-01T00:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"我来看看"}]}}"#,
                r#"{"timestamp":"2026-07-01T00:00:04Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1"}}"#,
                r#"{"timestamp":"2026-07-01T00:00:05Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"ok"}}"#,
            ],
        );
        // 不同 cwd 的会话不该出现在结果里。
        write_lines(
            &day_dir,
            "rollout-other.jsonl",
            &[
                r#"{"timestamp":"2026-07-01T00:00:00Z","type":"session_meta","payload":{"id":"cx-2","cwd":"/other"}}"#,
            ],
        );

        let sessions = list_codex_sessions("/proj", config_dir.to_str());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "实际问题"); // 合成的 environment_context 不该被当标题
        assert_eq!(sessions[0].message_count, 2);

        let detail = load_codex_session_detail(&sessions[0].path).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(detail.turns.len(), 2); // 合成消息被跳过，剩真实问题 + assistant 轮
        assert!(detail.turns[0].is_user);
        assert_eq!(detail.turns[0].text, "实际问题");
        assert!(!detail.turns[1].is_user);
        assert_eq!(detail.turns[1].text, "我来看看");
        assert_eq!(detail.turns[1].tools, vec!["exec_command".to_string()]); // 工具调用挂在上一条 assistant 轮上
        // Codex 的工具入参只有一条 shell 命令，没有结构化文件参数可读
        assert!(detail.turns[1].tool_paths.is_empty());
        // 模型在 turn_context 里，不在 session_meta
        assert_eq!(detail.model.as_deref(), Some("gpt-5.6-sol"));
    }

    #[test]
    fn grok_reader_reads_summary_json_and_skips_synthetic_rows() {
        let tmp = test_sandbox("grok");
        let _ = std::fs::remove_dir_all(&tmp);
        let config_dir = tmp.join(".grok");
        let session_dir = config_dir.join("sessions").join("proj").join("s1");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("summary.json"),
            r#"{"info":{"cwd":"/proj"},"session_summary":"聊聊策略","created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-01T00:05:00Z","num_chat_messages":2,"current_model_id":"grok-4.5"}"#,
        )
        .unwrap();
        write_lines(
            &session_dir,
            "chat_history.jsonl",
            &[
                r#"{"type":"user","synthetic_reason":"project_instructions","content":[{"type":"text","text":"注入的项目说明"}]}"#,
                // 实测：第一轮的 <user_info> 环境块不带 synthetic_reason 字段，得靠
                // 「剥完包装仍是尖括号开头」这条兜底规则识别，不是只认这个字段。
                r#"{"type":"user","content":[{"type":"text","text":"<user_info>\nOS: macos\n</user_info>"}]}"#,
                r#"{"type":"user","content":[{"type":"text","text":"<user_query>真实问题</user_query>"}]}"#,
                r#"{"type":"assistant","content":"回答","tool_calls":[{"id":"c1","name":"grep"},{"id":"c2","name":"read_file","arguments":"{\"target_file\":\"src/lib.rs\",\"limit\":50}"}]}"#,
                r#"{"type":"tool_result","tool_call_id":"c1","content":"..."}"#,
            ],
        );

        let sessions = list_grok_sessions("/proj", config_dir.to_str());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "聊聊策略");
        assert_eq!(sessions[0].message_count, 2);

        let detail = load_grok_session_detail(&sessions[0].path).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(detail.turns.len(), 2); // 两条合成消息（带/不带 synthetic_reason）都被跳过
        assert!(detail.turns[0].is_user);
        assert_eq!(detail.turns[0].text, "真实问题"); // <user_query> 包装被剥掉
        assert!(!detail.turns[1].is_user);
        assert_eq!(
            detail.turns[1].tools,
            vec!["grep".to_string(), "read_file".to_string()]
        );
        // Grok 的 arguments 是 JSON 字符串，得再解一层才能拿到 target_file
        assert_eq!(detail.turns[1].tool_paths, vec!["src/lib.rs".to_string()]);
        assert_eq!(detail.model.as_deref(), Some("grok-4.5"));
    }

    #[test]
    fn pi_reader_follows_session_dir_and_parses_messages_and_tool_calls() {
        let fs = MemFs::new();
        let cwd = "/workspace/project";
        let agent_dir = PathBuf::from("/providers/pi/agent");
        write(
            &fs,
            &agent_dir.join("settings.json"),
            r#"{"sessionDir":"custom-sessions"}"#,
        );
        let session = agent_dir
            .join("custom-sessions")
            .join("nested")
            .join("pi-session.jsonl");
        write(
            &fs,
            &session,
            concat!(
                r#"{"type":"session","version":3,"id":"pi-1","timestamp":"2026-08-17T05:00:00Z","cwd":"/workspace/project"}"#,
                "\n",
                r#"{"type":"model_change","id":"m1","parentId":null,"timestamp":"2026-08-17T05:00:01Z","provider":"sivla","modelId":"gpt-5.6-sol"}"#,
                "\n",
                r#"{"type":"message","id":"u1","parentId":"m1","timestamp":"2026-08-17T05:00:02Z","message":{"role":"user","content":"检查这个文件"}}"#,
                "\n",
                r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-08-17T05:00:03Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"先读文件"},{"type":"text","text":"我来检查"},{"type":"toolCall","id":"call-1","name":"read","arguments":{"path":"src/lib.rs"}}],"model":"sivla/gpt-5.6-sol","usage":{"input":10,"output":5,"cacheRead":3,"cacheWrite":2}}}"#,
                "\n",
                r#"{"type":"session_info","id":"i1","parentId":"a1","timestamp":"2026-08-17T05:00:04Z","name":"文件检查"}"#,
                "\n",
                "{ 这行故意是不完整 JSON",
            ),
        );
        let other = agent_dir.join("custom-sessions").join("other.jsonl");
        write(
            &fs,
            &other,
            r#"{"type":"session","version":3,"id":"pi-other","timestamp":"2026-08-17T06:00:00Z","cwd":"/workspace/other"}"#,
        );

        let sessions = list_pi_sessions_with(&fs, cwd, Some(agent_dir.to_str().unwrap()));
        assert_eq!(sessions.len(), 1);
        let summary = &sessions[0];
        assert_eq!(summary.resume_id, "pi-1");
        assert_eq!(summary.agent_title, "文件检查");
        assert_eq!(summary.message_count, 2);
        assert_eq!(summary.total_tokens, 20);
        assert_eq!(
            summary.last_active_at,
            parse_rfc3339("2026-08-17T05:00:03Z")
        );

        let detail = load_pi_session_detail_with(&fs, &session).unwrap();
        assert_eq!(detail.model.as_deref(), Some("sivla/gpt-5.6-sol"));
        assert_eq!(detail.turns.len(), 2);
        assert!(detail.turns[0].is_user);
        assert_eq!(detail.turns[0].text, "检查这个文件");
        assert_eq!(detail.turns[1].text, "我来检查");
        assert_eq!(detail.turns[1].tools, ["read"]);
        assert_eq!(detail.turns[1].tool_paths, ["src/lib.rs"]);
    }

    #[test]
    fn copilot_reader_reads_workspace_yaml_and_events_jsonl() {
        let tmp = test_sandbox("copilot");
        let _ = std::fs::remove_dir_all(&tmp);
        let config_dir = tmp.join(".copilot");
        let session_dir = config_dir.join("session-state").join("s1");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("workspace.yaml"),
            "id: s1\ncwd: /proj\nsummary: 调试问题\ncreated_at: 2026-07-01T00:00:00.000Z\nupdated_at: 2026-07-01T00:05:00.000Z\n",
        )
        .unwrap();
        write_lines(
            &session_dir,
            "events.jsonl",
            &[
                r#"{"type":"user.message","data":{"content":"真实问题","transformedContent":"<ide_selection>真实问题</ide_selection>"}}"#,
                r#"{"type":"assistant.message","data":{"model":"claude-sonnet-5","content":"回答","toolRequests":[{"toolCallId":"t1","name":"bash"}]}}"#,
                r#"{"type":"assistant.message","data":{"model":"claude-opus-5","content":"再看看","toolRequests":[{"toolCallId":"t2","name":"view","arguments":{"path":"src/app.ts"}}]}}"#,
                r#"{"type":"tool.execution_start","data":{"toolCallId":"t1","toolName":"bash"}}"#,
            ],
        );

        let sessions = list_copilot_sessions("/proj", config_dir.to_str());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "调试问题");
        assert_eq!(sessions[0].message_count, 3);

        let detail = load_copilot_session_detail(&sessions[0].path).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(detail.turns.len(), 3);
        assert!(detail.turns[0].is_user);
        // transformedContent（带 IDE 上下文）不该混进来，只取干净的 content。
        assert_eq!(detail.turns[0].text, "真实问题");
        assert_eq!(detail.turns[1].tools, vec!["bash".to_string()]);
        // Copilot 的 arguments 已经是对象，不用像 Grok 那样再解一层字符串
        assert_eq!(detail.turns[2].tool_paths, vec!["src/app.ts".to_string()]);
        // 同一份会话里换过模型：交接头部要交代的是最后在用的那个
        assert_eq!(detail.model.as_deref(), Some("claude-opus-5"));
    }
}
