//! Antigravity（`agy` TUI）本地历史的读取。
//!
//! Antigravity 没有 ACP stdio 服务，但**确实**把会话完整落盘在本机，所以它照样
//! 能出现在历史页——「能不能读历史」和「有没有 ACP」本来就是两件事。
//!
//! 落盘结构（实测 agy，2026-09）：
//! - `~/.gemini/antigravity-cli/conversation_summaries.db`：SQLite 总索引，一行一个
//!   会话，带标题、预览、步数、`workspace_uris`（`file:///...`）和时间戳。列表页
//!   只读这一张表就够了，不必去翻每个会话的正文库。
//! - `~/.gemini/antigravity-cli/conversations/<conversation_id>.db`：单个会话的正文，
//!   表 `steps(idx, step_type, step_payload)`，`step_payload` 是 protobuf。
//!
//! `step_type` 与 payload 字段的对应关系是对真实库做字段遍历实测出来的：
//! - `14` = 用户发言，正文在字段 `19.2`
//! - `15` = assistant 回合，最终回复在 `20.1`（`20.8` 是同一份文本的副本），
//!   `20.3` 是推理过程，`20.7.2` 是工具名，`20.7.3` 是工具入参 JSON
//! - `90` = 系统注入的上下文，`132` = 工具执行结果——都不是对话轮次，跳过
//!
//! 这些编号是 agy 的内部 protobuf schema，不是公开协议，升级后可能变。所以下面
//! 一律「尽力而为」：读不出来就当没有这一轮，绝不 panic、绝不猜。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Antigravity CLI 的数据根目录。
///
/// `AGY_HOME` 在实测中并不存在，这里只支持 `GEMINI_HOME` 覆盖（与 agy 自身读取
/// `~/.gemini` 的行为对齐），其余情况回落到 `~/.gemini/antigravity-cli`。
pub fn antigravity_root(override_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = override_dir.filter(|d| !d.trim().is_empty()) {
        return crate::workspace_override::expand_tilde(dir).into();
    }
    if let Ok(home) = std::env::var("GEMINI_HOME")
        && !home.trim().is_empty()
    {
        return PathBuf::from(home).join("antigravity-cli");
    }
    dirs_home().join(".gemini").join("antigravity-cli")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
}

/// 总索引库路径。
pub fn summaries_db(override_dir: Option<&str>) -> PathBuf {
    antigravity_root(override_dir).join("conversation_summaries.db")
}

/// 单个会话正文库的路径。历史页的 `SessionSummary.path` 存的就是它。
pub fn conversation_db(override_dir: Option<&str>, conversation_id: &str) -> PathBuf {
    antigravity_root(override_dir)
        .join("conversations")
        .join(format!("{conversation_id}.db"))
}

/// 索引表里的一行。
pub struct ConversationRow {
    pub conversation_id: String,
    pub title: String,
    pub preview: String,
    pub step_count: i64,
    pub last_modified: Option<String>,
    pub last_user_input: Option<String>,
    pub workspace_uris: String,
}

/// 以只读方式打开 SQLite。
///
/// **必须只读**：`agy` 可能正开着同一个库，我们是来旁观的，不能因为读历史就给
/// 别人的库加写锁、建 `-wal`/`-shm`，更不能在崩溃时留下半截事务。
#[cfg(not(any(target_os = "ios", target_os = "android")))]
fn open_readonly(path: &Path) -> Option<rusqlite::Connection> {
    if !path.exists() {
        return None;
    }
    rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()
}

/// 读出索引表里属于 `want_cwd` 这个项目的会话。
///
/// 过滤按 `workspace_uris` 做：它是一段 JSON 数组文本，形如
/// `["file:///Users/x/proj"]`。这里不引 JSON 解析，直接比对解码后的路径集合。
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn list_conversations(override_dir: Option<&str>, want_cwd: &str) -> Vec<ConversationRow> {
    let Some(conn) = open_readonly(&summaries_db(override_dir)) else {
        return Vec::new();
    };
    // 被标记 killed 的会话在 agy 自己的列表里也不出现，这里保持一致。
    let sql = "select conversation_id, title, preview, step_count, \
               last_modified_time, last_user_input_time, workspace_uris \
               from conversation_summaries where killed = 0 \
               order by last_modified_time desc";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |row| {
        Ok(ConversationRow {
            conversation_id: row.get(0)?,
            title: row.get::<_, String>(1).unwrap_or_default(),
            preview: row.get::<_, String>(2).unwrap_or_default(),
            step_count: row.get::<_, i64>(3).unwrap_or_default(),
            last_modified: row.get::<_, Option<String>>(4).unwrap_or_default(),
            last_user_input: row.get::<_, Option<String>>(5).unwrap_or_default(),
            workspace_uris: row.get::<_, String>(6).unwrap_or_default(),
        })
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };
    rows.filter_map(Result::ok)
        .filter(|row| workspace_matches(&row.workspace_uris, want_cwd))
        .collect()
}

/// 移动端不编译原生 SQLite（见 Cargo.toml 的目标门），历史页在那边本来就不存在。
#[cfg(any(target_os = "ios", target_os = "android"))]
pub fn list_conversations(_override_dir: Option<&str>, _want_cwd: &str) -> Vec<ConversationRow> {
    Vec::new()
}

/// `workspace_uris` 是否覆盖了目标项目目录。
///
/// 只认「完全相等」，不做前缀匹配：`/a/proj` 与 `/a/proj-old` 互不相干，用前缀
/// 匹配会把隔壁项目的会话混进来。
pub fn workspace_matches(workspace_uris: &str, want_cwd: &str) -> bool {
    workspace_uris_paths(workspace_uris)
        .iter()
        .any(|path| path == want_cwd)
}

/// 从 `["file:///a/b", ...]` 这段文本里取出本地路径。
///
/// 手写扫描而不是拉 JSON 依赖：这里的形状固定且简单，真正需要处理的是
/// percent-encoding（路径里有空格/中文时 agy 会编码）。
pub fn workspace_uris_paths(workspace_uris: &str) -> Vec<String> {
    workspace_uris
        .split(['[', ']', ',', '"'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.strip_prefix("file://"))
        .map(percent_decode)
        .collect()
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

// ===================== steps 正文 =====================

/// agy 内部 `step_type` 里我们认得的两种对话轮次。其余（90 系统注入、132 工具
/// 结果等）不是用户可见的对话轮，一律跳过。
const STEP_TYPE_USER: i64 = 14;
const STEP_TYPE_ASSISTANT: i64 = 15;

/// 一条解析出来的轮次。字段刻意与 `session_history::Turn` 对齐，但不在这里直接
/// 构造它——那是展示层的模型，本模块只负责「把 agy 的库读成结构化数据」。
pub struct AntigravityTurn {
    pub is_user: bool,
    pub text: String,
    pub tools: Vec<String>,
    pub tool_paths: Vec<String>,
}

/// 读取单个会话正文库里的所有对话轮次，按 `idx` 升序。
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn read_turns(db_path: &Path) -> Option<Vec<AntigravityTurn>> {
    let conn = open_readonly(db_path)?;
    let mut stmt = conn
        .prepare("select step_type, step_payload from steps order by idx")
        .ok()?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0).unwrap_or_default(),
                row.get::<_, Vec<u8>>(1).unwrap_or_default(),
            ))
        })
        .ok()?;
    let mut turns = Vec::new();
    for row in rows {
        let Ok((step_type, payload)) = row else {
            continue;
        };
        if let Some(turn) = decode_step(step_type, &payload) {
            turns.push(turn);
        }
    }
    Some(turns)
}

/// 移动端不编译原生 SQLite，见 [`list_conversations`]。
#[cfg(any(target_os = "ios", target_os = "android"))]
pub fn read_turns(_db_path: &Path) -> Option<Vec<AntigravityTurn>> {
    None
}

/// 把一条 step 解成对话轮次。返回 `None` 表示「这条不是对话轮」或「解不出来」，
/// 两种情况对调用方是一样的：跳过即可。
fn decode_step(step_type: i64, payload: &[u8]) -> Option<AntigravityTurn> {
    if step_type != STEP_TYPE_USER && step_type != STEP_TYPE_ASSISTANT {
        return None;
    }
    let fields = decode_strings(payload)?;
    if step_type == STEP_TYPE_USER {
        let text = fields.get("19.2").or_else(|| fields.get("19.3.1"))?;
        let text = text.trim();
        return (!text.is_empty()).then(|| AntigravityTurn {
            is_user: true,
            text: text.to_string(),
            tools: Vec::new(),
            tool_paths: Vec::new(),
        });
    }

    // assistant：`20.1` 是最终回复正文；`20.3` 只是推理过程，和其它 agent 的
    // 展示口径不一致，不当作正文。没有正文但调了工具的轮次照样保留，否则
    // 「跑了一串工具」这段过程在历史里会凭空消失。
    let text = fields
        .get("20.1")
        .or_else(|| fields.get("20.8"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let mut tools = Vec::new();
    let mut tool_paths = Vec::new();
    if let Some(tool) = fields.get("20.7.2") {
        tools.push(tool.clone());
    }
    if let Some(args) = fields.get("20.7.3") {
        tool_paths = tool_arg_paths(args);
    }
    if text.is_empty() && tools.is_empty() {
        return None;
    }
    Some(AntigravityTurn {
        is_user: false,
        text,
        tools,
        tool_paths,
    })
}

/// 从工具入参 JSON 里挑出像文件路径的值。
///
/// 只认实测存在的结构化键，不从命令行里猜路径——猜错了写进交接记录，会把下一个
/// agent 引到不存在的文件上。`run_command` 的 `CommandLine` 是一整条 shell 命令，
/// 故意不取；`Cwd` 是工作目录，属于结构化路径，取。
fn tool_arg_paths(args_json: &str) -> Vec<String> {
    const PATH_KEYS: [&str; 5] = [
        "TargetFile",
        "AbsolutePath",
        "Cwd",
        "target_file",
        "file_path",
    ];
    let Ok(value) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in PATH_KEYS {
        if let Some(path) = value.get(key).and_then(|v| v.as_str())
            && !path.trim().is_empty()
            && !out.iter().any(|existing| existing == path)
        {
            out.push(path.to_string());
        }
    }
    out
}

// ===================== 最小 protobuf 遍历 =====================

/// 把 protobuf 报文里所有「可打印字符串」字段摊平成 `字段路径 -> 值`。
///
/// 这里**刻意不生成 .proto / 不引 prost**：agy 的 schema 是它的内部实现，没有公开
/// 定义可用，跟着它生成一份只会在对方改版时变成需要同步维护的死代码。我们只需要
/// 少数几个叶子字段的文本，按字段号路径取值足够了，且对未知字段天然免疫。
///
/// 同一路径重复出现时保留**第一个**：目标字段都是单值，重复只会出现在嵌套的历史
/// 副本里（例如工具结果又内嵌了一份原始 step）。
fn decode_strings(payload: &[u8]) -> Option<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    walk(payload, "", &mut out)?;
    Some(out)
}

/// 读一个 varint，返回 (值, 新游标)。
fn read_varint(buf: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *buf.get(i)?;
        i += 1;
        if shift >= 64 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some((value, i));
        }
        shift += 7;
    }
}

/// 递归遍历。返回 `None` = 这段字节不是合法 protobuf。
///
/// 长度分隔字段有歧义：既可能是嵌套消息，也可能是字符串/任意字节。策略是**先试
/// 着当嵌套消息解**，解得通就下钻；解不通再看它是不是可打印 UTF-8，是就收下当
/// 字符串；都不是就当不透明字节丢掉（例如 zstd 压缩过的工具输出）。
fn walk(buf: &[u8], prefix: &str, out: &mut BTreeMap<String, String>) -> Option<()> {
    let mut i = 0usize;
    while i < buf.len() {
        let (key, next) = read_varint(buf, i)?;
        i = next;
        let field = key >> 3;
        let wire = key & 7;
        if field == 0 {
            return None;
        }
        match wire {
            0 => {
                let (_, next) = read_varint(buf, i)?;
                i = next;
            }
            1 => i = i.checked_add(8).filter(|n| *n <= buf.len())?,
            5 => i = i.checked_add(4).filter(|n| *n <= buf.len())?,
            2 => {
                let (len, next) = read_varint(buf, i)?;
                let len = usize::try_from(len).ok()?;
                let start = next;
                let end = start.checked_add(len).filter(|n| *n <= buf.len())?;
                let data = &buf[start..end];
                i = end;
                let path = if prefix.is_empty() {
                    field.to_string()
                } else {
                    format!("{prefix}.{field}")
                };
                let mut nested = BTreeMap::new();
                if !data.is_empty() && walk(data, &path, &mut nested).is_some() {
                    for (k, v) in nested {
                        out.entry(k).or_insert(v);
                    }
                } else if let Some(text) = printable_utf8(data) {
                    out.entry(path).or_insert(text);
                }
            }
            _ => return None,
        }
    }
    Some(())
}

/// 这段字节是不是「像正文」的 UTF-8。控制字符（除常见空白）一律否决，避免把
/// 压缩块或二进制 id 当成文本塞进历史里。
fn printable_utf8(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?;
    text.chars()
        .all(|ch| matches!(ch, '\n' | '\t' | '\r') || !ch.is_control())
        .then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工编码一个 protobuf 字段，方便在测试里拼报文。
    fn field(number: u64, wire: u64, body: &[u8]) -> Vec<u8> {
        let mut out = varint((number << 3) | wire);
        out.extend_from_slice(body);
        out
    }

    fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    fn len_delimited(number: u64, body: &[u8]) -> Vec<u8> {
        let mut payload = varint(body.len() as u64);
        payload.extend_from_slice(body);
        field(number, 2, &payload)
    }

    #[test]
    fn 用户轮次取字段_19_2() {
        // step { 19 { 2: "你好" } }
        let inner = len_delimited(2, "你好".as_bytes());
        let payload = len_delimited(19, &inner);
        let turn = decode_step(STEP_TYPE_USER, &payload).expect("应解析出用户轮次");
        assert!(turn.is_user);
        assert_eq!(turn.text, "你好");
    }

    #[test]
    fn assistant_取最终正文而非推理过程() {
        // step { 20 { 1: "结论", 3: "推理" } }
        let mut inner = len_delimited(1, "结论".as_bytes());
        inner.extend(len_delimited(3, "推理".as_bytes()));
        let payload = len_delimited(20, &inner);
        let turn = decode_step(STEP_TYPE_ASSISTANT, &payload).expect("应解析出 assistant 轮次");
        assert!(!turn.is_user);
        assert_eq!(turn.text, "结论", "推理过程不应当作正文");
    }

    #[test]
    fn assistant_保留只调工具的轮次() {
        // step { 20 { 7 { 2: "run_command", 3: "{\"Cwd\":\"/tmp/x\"}" } } }
        let mut tool = len_delimited(2, b"run_command");
        tool.extend(len_delimited(3, br#"{"Cwd":"/tmp/x"}"#));
        let inner = len_delimited(7, &tool);
        let payload = len_delimited(20, &inner);
        let turn = decode_step(STEP_TYPE_ASSISTANT, &payload).expect("只调工具的轮次也要保留");
        assert_eq!(turn.tools, vec!["run_command".to_string()]);
        assert_eq!(turn.tool_paths, vec!["/tmp/x".to_string()]);
        assert!(turn.text.is_empty());
    }

    #[test]
    fn 跳过系统注入与工具结果() {
        let inner = len_delimited(1, "系统提示".as_bytes());
        let payload = len_delimited(103, &inner);
        assert!(decode_step(90, &payload).is_none(), "90 不是对话轮");
        assert!(decode_step(132, &payload).is_none(), "132 不是对话轮");
    }

    #[test]
    fn 不可打印字节不会被当成正文() {
        let inner = len_delimited(2, &[0x1b, 0x00, 0xff, 0xfe]);
        let payload = len_delimited(19, &inner);
        assert!(decode_step(STEP_TYPE_USER, &payload).is_none());
    }

    #[test]
    fn 工具入参不从命令行里猜路径() {
        let paths = tool_arg_paths(r#"{"CommandLine":"cat /etc/passwd","Cwd":"/tmp"}"#);
        assert_eq!(
            paths,
            vec!["/tmp".to_string()],
            "只取结构化的 Cwd，不从 CommandLine 里猜"
        );
    }

    #[test]
    fn workspace_过滤不做前缀匹配() {
        let uris = r#"["file:///Users/a/proj"]"#;
        assert!(workspace_matches(uris, "/Users/a/proj"));
        assert!(
            !workspace_matches(uris, "/Users/a/proj-old"),
            "隔壁项目不能混进来"
        );
        assert!(!workspace_matches(uris, "/Users/a"));
    }

    #[test]
    fn workspace_路径支持百分号编码() {
        let uris = r#"["file:///Users/a/my%20proj"]"#;
        assert!(workspace_matches(uris, "/Users/a/my proj"));
    }

    #[test]
    fn 多工作区会话对每个工作区都可见() {
        let uris = r#"["file:///Users/a/one","file:///Users/a/two"]"#;
        assert!(workspace_matches(uris, "/Users/a/one"));
        assert!(workspace_matches(uris, "/Users/a/two"));
    }
}
