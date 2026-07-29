//! 远程操作网关的核心逻辑（路由 + handler + HTML 模板），供两个地方使用：
//! - `crates/smeltd/src/bin/gateway.rs`：独立进程，命令行启动，自己管一个 `--bind`/`--port`
//! - `crates/smeltd/src/main.rs`：内嵌进守护，靠 `remote_start`/`remote_stop` op 按需开关
//!
//! 两边共用同一份 handler，避免同一套鉴权/转义/协议逻辑复制两次（CLAUDE.md 明令
//! 别复制）。这个模块本身**不碰 smeltd 主协议**：所有跟 smeltd 的交互都是走
//! `sock_path()` 连它自己的 unix socket，用既有的 `list`/`watch` op——不管是从独立
//! 进程调用还是从 smeltd 内部的这个模块调用，走的都是同一条路径，行为完全一致。
//!
//! 见 docs/remote-ops-roadmap.md（Phase 1/2）、docs/collaboration.md（安全底线）。

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::agent_status::AgentStatus;
use crate::attention::{AttentionItem, AttentionStore, apply_daemon_transition};
use crate::daemon_state::{DaemonPhase, DaemonSessionState};

const REFERENCE_PAGE: &str = include_str!("remote_gateway_page.html");
const LIST_PAGE: &str = include_str!("remote_gateway_list_page.html");
const CONSOLE_PAGE: &str = include_str!("remote_gateway_console_page.html");

/// 编译期打进二进制的 SPA（`build.rs` 保证 `remote-web/dist` 存在）。
/// Docker 只交付 smeltd、或 App 里漏拷 Resources 时，仍能出 Preact 面板。
#[derive(rust_embed::RustEmbed)]
#[folder = "../../remote-web/dist/"]
struct EmbeddedSpa;

/// 磁盘上的 SPA 目录（可选覆盖嵌入资源，便于开发热更）。
/// 顺序：`SMELT_REMOTE_WEB` → App `Resources/remote-web` → 同目录 `remote-web` → 仓库 dist。
fn remote_web_dist_fs() -> Option<PathBuf> {
    use std::sync::OnceLock;
    static CACHED: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let mut candidates: Vec<PathBuf> = Vec::new();
            if let Ok(p) = std::env::var("SMELT_REMOTE_WEB") {
                let p = PathBuf::from(p);
                if !p.as_os_str().is_empty() {
                    candidates.push(p);
                }
            }
            if let Ok(exe) = std::env::current_exe() {
                if let Some(macos_dir) = exe.parent() {
                    if let Some(contents) = macos_dir.parent() {
                        candidates.push(contents.join("Resources").join("remote-web"));
                    }
                    candidates.push(macos_dir.join("remote-web"));
                }
            }
            candidates.push(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("remote-web")
                    .join("dist"),
            );
            for c in candidates {
                if c.join("index.html").is_file() {
                    return Some(c);
                }
            }
            None
        })
        .clone()
}

/// 读 SPA 文件：磁盘优先（开发），否则用嵌入资源（Docker/DMG 可靠路径）。
fn spa_read(rel: &str) -> Option<Vec<u8>> {
    let rel = rel.trim_start_matches('/');
    if let Some(dir) = remote_web_dist_fs() {
        let p = dir.join(rel);
        if p.is_file() {
            if let Ok(b) = std::fs::read(p) {
                return Some(b);
            }
        }
    }
    EmbeddedSpa::get(rel).map(|f| f.data.into_owned())
}

fn spa_ready() -> bool {
    match spa_read("index.html") {
        Some(b) => {
            let s = String::from_utf8_lossy(&b);
            // build.rs 占位页不含 Vite assets 引用
            s.contains("/assets/") || s.contains("assets/")
        }
        None => false,
    }
}

pub fn sock_path() -> std::path::PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| "/tmp".into())
        .join(".smelt");
    dir.join("smeltd.sock")
}

#[derive(Clone)]
struct AppState {
    token: Arc<String>,
    /// 这个 token 是否有写权限（approve/deny/reply）。链接分享出去那一刻就是
    /// 授权动作，这里不再加一层"每次点击都要主人当面确认"——见
    /// smeltd.rs「远程操控」一节的授权模型说明。开没开由生成链接时的 GUI 开关
    /// 决定，`build_router` 只是如实转达。
    write_enabled: bool,
    mobile_lifecycle: Arc<MobileLifecycleHub>,
}

struct MobileLifecycleHub {
    state: Mutex<MobileLifecycleState>,
    updates: tokio::sync::broadcast::Sender<MobileLifecycleEvent>,
}

#[derive(Default)]
struct MobileLifecycleState {
    sessions: std::collections::HashMap<String, DaemonSessionState>,
    attention: AttentionStore,
}

#[derive(Clone)]
enum MobileLifecycleEvent {
    SessionsChanged,
    Attention(AttentionItem),
    AttentionResolved(String),
}

#[derive(Deserialize)]
struct AuthQuery {
    token: String,
}

#[derive(Deserialize)]
struct ActionBody {
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

/// 原始输入：UTF-8 字符串，控制字符用 JSON `\u00xx`（xterm onData 直接 stringify 即可）。
#[derive(Deserialize)]
struct InputBody {
    data: String,
}

#[derive(Deserialize)]
struct ResizeBody {
    cols: u16,
    rows: u16,
    #[serde(default)]
    cell_w: u16,
    #[serde(default)]
    cell_h: u16,
}

/// 组好整个网关的路由，鉴权用这一个 token（见 collaboration.md：一个网关/token 管
/// 这台机器上的全部活会话，泄漏一条链接的代价是明确的，不是没想到的疏漏）。
///
/// 前端：优先托管 `remote-web/dist`（Preact + Tailwind + xterm 的 CLI 面板）。
/// 未构建时回退内嵌 HTML（list / console / xterm）。
pub fn build_router(token: String, write_enabled: bool) -> Router {
    let mobile_lifecycle = MobileLifecycleHub::start();
    let state = AppState {
        token: Arc::new(token),
        write_enabled,
        mobile_lifecycle,
    };
    let mut r = Router::new()
        .route("/sessions", get(sessions_json_handler))
        .route("/s/{id}/stream", get(stream_handler))
        .route("/s/{id}/state-stream", get(state_stream_handler))
        .route("/s/{id}/action", axum::routing::post(action_handler))
        .route("/s/{id}/input", axum::routing::post(input_handler))
        .route("/s/{id}/resize", axum::routing::post(resize_handler))
        // ACP 路由（移动端用）
        .route("/acp/sessions", get(acp_sessions_handler))
        .route("/acp/ws", get(acp_ws_handler));

    if spa_ready() {
        // SPA：/ 与 /s/:id 都回 index.html（注入 write meta）；静态资源 /assets/*
        r = r
            .route("/", get(spa_index_handler))
            .route("/s/{id}", get(spa_index_handler_with_id))
            .route("/s/{id}/console", get(spa_index_handler_with_id))
            .route("/assets/{*path}", get(spa_asset_handler));
    } else {
        r = r
            .route("/", get(list_page_handler))
            .route("/s/{id}", get(page_handler))
            .route("/s/{id}/console", get(console_handler));
    }

    r.with_state(state)
}

/// 读 SPA index.html（磁盘或嵌入），注入 write 权限 meta。
fn spa_index_html(write_enabled: bool) -> Response {
    let Some(bytes) = spa_read("index.html") else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "remote-web 未构建：cd remote-web && npm ci && npm run build",
        )
            .into_response();
    };
    let mut raw = String::from_utf8_lossy(&bytes).into_owned();
    let meta = format!(
        r#"<meta name="smelt-write" content="{}" />"#,
        if write_enabled { "true" } else { "false" }
    );
    if raw.contains("</head>") {
        raw = raw.replacen("</head>", &format!("{meta}\n</head>"), 1);
    } else {
        raw = format!("{meta}\n{raw}");
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store, max-age=0"),
        ],
        raw,
    )
        .into_response()
}

async fn spa_index_handler(Query(q): Query<AuthQuery>, State(state): State<AppState>) -> Response {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    spa_index_html(state.write_enabled)
}

async fn spa_index_handler_with_id(
    Path(_id): Path<String>,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> Response {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    spa_index_html(state.write_enabled)
}

/// 托管 Vite 产物：/assets/...（磁盘或嵌入二进制）
async fn spa_asset_handler(Path(path): Path<String>) -> Response {
    if path.contains("..") || path.starts_with('/') {
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    }
    let rel = format!("assets/{path}");
    let Some(bytes) = spa_read(&rel) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let ct = match PathBuf::from(&path).extension().and_then(|e| e.to_str()) {
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, ct),
            (header::CACHE_CONTROL, "public, max-age=120"),
        ],
        bytes,
    )
        .into_response()
}

/// 把字符串安全地嵌进内联 `<script>` 里的 JS 字符串字面量：JSON 转义处理引号/
/// 反斜杠，额外把尖括号转成 Unicode 转义序列——防止 id/token 里带 `</script>`
/// 提前把这段脚本切断（HTML 解析器找 `</script` 是纯文本匹配，不管有没有在字符串里）。
fn js_string_literal(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".to_string())
        .replace('<', "\\u003c")
}

/// 把字符串安全地嵌进 HTML 正文/属性：转义 `& < > "`。会话列表页用它嵌 session id——
/// 现在 id 都是 GUI 用 `uuid::Uuid::new_v4()` 生成的（见 workspace/main.rs），字符集
/// 天然安全，这里是防御性的，防止以后 id 格式变了变成新的注入面。
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 远程列表里一条可 attach 的终端（smeltd 会话）。展示名优先走 GUI 的
/// `~/.smelt/workspace.json`（用户重命名 / 快捷启动标签），跟 PC 侧栏一致；
/// 没有 GUI 元数据时再回退 smeltd 的 title / launch / cwd 末段。
#[derive(Clone, serde::Serialize)]
struct SessionInfo {
    id: String,
    phase: String,
    pending_question: Option<String>,
    /// 列表主标题（名称，不是 uuid）。
    name: String,
    /// 项目分组名：cwd 目录末段（与 workspace 侧栏 `project_name_for_cwd` 同规则）。
    project: String,
    /// 多 pane 会话的父会话名（如 "services"）；单 pane 为 None，直接挂在项目下。
    parent_session: Option<String>,
    cwd: Option<String>,
}

/// GUI 侧一个叶子终端的展示元数据（从 workspace.json 扫出来）。
#[derive(Clone, Default)]
struct GuiLeafMeta {
    /// 会话级 custom_title（侧栏上那一行的名字，如 "services" / "claude-quant"）。
    session_title: Option<String>,
    /// 叶子级 custom_title 或 launch_label（嵌套时显示成子项，如 "frontend"）。
    pane_title: Option<String>,
    cwd: Option<String>,
    /// 同一 GUI 会话里有多个叶子 → 列表要嵌套在 session_title 下。
    multi_pane: bool,
    /// workspace.json 里的会话顺序，用来保持跟 PC 侧栏一致。
    session_ord: usize,
    leaf_ord: usize,
}

fn workspace_json_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| "/tmp".into())
        .join(".smelt")
        .join("workspace.json")
}

/// cwd → 项目分组名：取目录末段（跟 workspace/main.rs 的 `project_name_for_cwd` 对齐）。
fn project_name_for_cwd(cwd: &str) -> String {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("项目")
        .to_string()
}

/// 递归扫 workspace.json 的 layout 树，把每个有 id 的叶子记下来。
fn collect_gui_leaves(
    pane: &serde_json::Value,
    session_title: Option<&str>,
    multi_pane: bool,
    session_ord: usize,
    leaf_counter: &mut usize,
    out: &mut std::collections::HashMap<String, GuiLeafMeta>,
) {
    if let Some(leaf) = pane.get("Leaf") {
        let id = match leaf.get("id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return,
        };
        let pane_title = leaf
            .get("custom_title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| {
                leaf.get("launch_label")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            });
        let cwd = leaf.get("cwd").and_then(|v| v.as_str()).map(String::from);
        let leaf_ord = *leaf_counter;
        *leaf_counter += 1;
        out.insert(
            id,
            GuiLeafMeta {
                session_title: session_title.map(String::from),
                pane_title,
                cwd,
                multi_pane,
                session_ord,
                leaf_ord,
            },
        );
        return;
    }
    if let Some(split) = pane.get("Split") {
        if let Some(children) = split.get("children").and_then(|c| c.as_array()) {
            for child in children {
                collect_gui_leaves(
                    child,
                    session_title,
                    multi_pane,
                    session_ord,
                    leaf_counter,
                    out,
                );
            }
        }
    }
}

/// 读 `~/.smelt/workspace.json`，建 id → 展示元数据。读失败 / 文件不存在 → 空表，
/// 列表仍可用 smeltd 自带字段兜底（名称会差一些，但不崩）。
fn load_gui_leaf_meta() -> std::collections::HashMap<String, GuiLeafMeta> {
    let Ok(raw) = std::fs::read_to_string(workspace_json_path()) else {
        return std::collections::HashMap::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return std::collections::HashMap::new();
    };
    let mut out = std::collections::HashMap::new();
    let Some(sessions) = v.get("sessions").and_then(|s| s.as_array()) else {
        return out;
    };
    for (session_ord, sess) in sessions.iter().enumerate() {
        let session_title = sess
            .get("custom_title")
            .and_then(|t| t.as_str())
            .filter(|s| !s.is_empty());
        let Some(layout) = sess.get("layout") else {
            continue;
        };
        // 先数这个会话有几个带 id 的叶子，决定要不要嵌套显示。
        let mut count_ids = 0usize;
        fn count_leaves(pane: &serde_json::Value, n: &mut usize) {
            if let Some(leaf) = pane.get("Leaf") {
                if leaf
                    .get("id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty())
                {
                    *n += 1;
                }
            } else if let Some(children) = pane
                .get("Split")
                .and_then(|s| s.get("children"))
                .and_then(|c| c.as_array())
            {
                for c in children {
                    count_leaves(c, n);
                }
            }
        }
        count_leaves(layout, &mut count_ids);
        let multi_pane = count_ids > 1;
        let mut leaf_counter = 0usize;
        collect_gui_leaves(
            layout,
            session_title,
            multi_pane,
            session_ord,
            &mut leaf_counter,
            &mut out,
        );
    }
    out
}

fn load_gui_acp_titles() -> std::collections::HashMap<String, String> {
    let Ok(raw) = std::fs::read_to_string(workspace_json_path()) else {
        return std::collections::HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return std::collections::HashMap::new();
    };
    value["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|session| {
            let id = session["acp"]["sid"].as_str()?;
            let title = session["custom_title"].as_str()?.trim();
            (!title.is_empty()).then(|| (id.to_string(), title.to_string()))
        })
        .collect()
}

#[derive(Default)]
struct MobileWorkspaceOrder {
    projects: Vec<String>,
    sessions: std::collections::HashMap<String, (usize, usize)>,
}

fn mobile_workspace_order_from_value(value: &serde_json::Value) -> MobileWorkspaceOrder {
    let projects = value["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|project| project.as_str().map(String::from))
        .collect();
    let mut sessions = std::collections::HashMap::new();
    for (session_order, session) in value["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(sid) = session["acp"]["sid"].as_str() {
            sessions.insert(sid.to_string(), (session_order, 0));
        }
        let Some(layout) = session.get("layout") else {
            continue;
        };
        let mut leaf_order = 0;
        fn collect_ordered_leaf_ids(
            pane: &serde_json::Value,
            session_order: usize,
            leaf_order: &mut usize,
            out: &mut std::collections::HashMap<String, (usize, usize)>,
        ) {
            if let Some(leaf) = pane.get("Leaf") {
                if let Some(id) = leaf["id"].as_str() {
                    out.insert(id.to_string(), (session_order, *leaf_order));
                }
                *leaf_order += 1;
            } else if let Some(children) = pane
                .get("Split")
                .and_then(|split| split.get("children"))
                .and_then(|children| children.as_array())
            {
                for child in children {
                    collect_ordered_leaf_ids(child, session_order, leaf_order, out);
                }
            }
        }
        collect_ordered_leaf_ids(layout, session_order, &mut leaf_order, &mut sessions);
    }
    MobileWorkspaceOrder { projects, sessions }
}

fn load_mobile_workspace_order() -> MobileWorkspaceOrder {
    let Ok(raw) = std::fs::read_to_string(workspace_json_path()) else {
        return MobileWorkspaceOrder::default();
    };
    let Ok(value) = serde_json::from_str(&raw) else {
        return MobileWorkspaceOrder::default();
    };
    mobile_workspace_order_from_value(&value)
}

fn mobile_project_root(projects: &[String], cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let cwd = cwd.trim_end_matches('/');
    projects
        .iter()
        .map(|project| project.trim_end_matches('/'))
        .filter(|root| !root.is_empty() && (cwd == *root || cwd.starts_with(&format!("{root}/"))))
        .max_by_key(|root| root.len())
        .map(String::from)
}

/// 从 OSC 标题里剥掉 spinner / 状态前缀，只留人类可读的短名；剥空了就当没有。
fn clean_agent_title(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    // 常见 agent 标题：`✳ Claude Code` / spinner 前缀 / `… — working`
    let stripped = t
        .trim_start_matches(|c: char| {
            c.is_whitespace() || c == '✳' || c == '*' || ('\u{2800}'..='\u{28FF}').contains(&c) // braille spinners
        })
        .trim();
    let stripped = stripped
        .split(['—', '|', '·'])
        .next()
        .unwrap_or(stripped)
        .trim();
    if stripped.is_empty() || stripped.len() > 48 {
        None
    } else {
        Some(stripped.to_string())
    }
}

/// 给一条 smeltd 会话挑展示名：GUI 元数据 > launch > title > cwd 末段 > 短 id。
fn resolve_display_name(
    id: &str,
    cwd: Option<&str>,
    title: Option<&str>,
    launch: Option<&str>,
    gui: Option<&GuiLeafMeta>,
) -> (String, Option<String>, String) {
    let project = gui
        .and_then(|g| g.cwd.as_deref())
        .or(cwd)
        .map(project_name_for_cwd)
        .unwrap_or_else(|| "其他".to_string());

    let parent_session = gui.and_then(|g| {
        if g.multi_pane {
            g.session_title.clone()
        } else {
            None
        }
    });

    // 嵌套 pane：优先叶子名；否则用 session 名 + 序号感的短 id 太丑，用 pane/title。
    let name = if let Some(g) = gui {
        if g.multi_pane {
            g.pane_title
                .clone()
                .or_else(|| title.and_then(clean_agent_title))
                .or_else(|| launch.map(|l| l.to_string()))
                .unwrap_or_else(|| short_id(id))
        } else {
            g.session_title
                .clone()
                .or_else(|| g.pane_title.clone())
                .or_else(|| title.and_then(clean_agent_title))
                .or_else(|| launch.map(|l| l.to_string()))
                .or_else(|| cwd.map(project_name_for_cwd))
                .unwrap_or_else(|| short_id(id))
        }
    } else {
        title
            .and_then(clean_agent_title)
            .or_else(|| launch.map(|l| l.to_string()))
            .or_else(|| cwd.map(project_name_for_cwd))
            .unwrap_or_else(|| short_id(id))
    };

    (name, parent_session, project)
}

fn short_id(id: &str) -> String {
    let s: String = id.chars().take(8).collect();
    if s.is_empty() { "会话".into() } else { s }
}

/// 问 smeltd 要当前活会话列表 + 状态，再叠 workspace.json 的展示名。
/// 阻塞 IO，调用方需要丢进 `spawn_blocking`。
fn list_sessions_info() -> Vec<SessionInfo> {
    let Ok(conn) = UnixStream::connect(sock_path()) else {
        return Vec::new();
    };
    let Ok(mut writer) = conn.try_clone() else {
        return Vec::new();
    };
    if writeln!(writer, "{}", serde_json::json!({ "op": "list" })).is_err() {
        return Vec::new();
    }
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
        return Vec::new();
    };
    let empty = Vec::new();
    let ids = v["sessions"].as_array().unwrap_or(&empty);
    let states = v["states"].as_array().unwrap_or(&empty);
    let gui = load_gui_leaf_meta();

    let mut infos: Vec<(SessionInfo, usize, usize)> = ids
        .iter()
        .zip(states.iter().map(Some).chain(std::iter::repeat(None)))
        .filter_map(|(id, state)| {
            let id = id.as_str()?.to_string();
            let phase = state
                .and_then(|s| s["phase"].as_str())
                .unwrap_or("idle")
                .to_string();
            let pending_question = state
                .and_then(|s| s["pending_question"].as_str())
                .map(String::from);
            let cwd = state
                .and_then(|s| s["cwd"].as_str())
                .map(String::from)
                .or_else(|| gui.get(&id).and_then(|g| g.cwd.clone()));
            let title = state.and_then(|s| s["title"].as_str());
            let launch = state.and_then(|s| s["launch"].as_str());
            let g = gui.get(&id);
            let (name, parent_session, project) =
                resolve_display_name(&id, cwd.as_deref(), title, launch, g);
            let session_ord = g.map(|x| x.session_ord).unwrap_or(usize::MAX);
            let leaf_ord = g.map(|x| x.leaf_ord).unwrap_or(0);
            Some((
                SessionInfo {
                    id,
                    phase,
                    pending_question,
                    name,
                    project,
                    parent_session,
                    cwd,
                },
                session_ord,
                leaf_ord,
            ))
        })
        .collect();

    // 跟 PC 侧栏同一套：workspace 顺序优先，未入档的会话排在后面。
    infos.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then(a.2.cmp(&b.2))
            .then(a.0.project.cmp(&b.0.project))
            .then(a.0.name.cmp(&b.0.name))
    });
    infos.into_iter().map(|(info, _, _)| info).collect()
}

/// phase → (中文标签, 状态点颜色)，跟 remote_gateway_console_page.html 里 JS 那份
/// PHASE_LABEL 手动保持一致（一个是服务端渲染列表页用，一个是操作台页面
/// 实时刷新用，没法共用一份代码——不同语言）。
fn phase_label(phase: &str) -> (&'static str, &'static str) {
    match phase {
        "thinking" => ("思考中…", "#4a9eff"),
        "executing_tool" => ("执行工具中…", "#4a9eff"),
        "awaiting_approval" => ("等你批准", "#ef4444"),
        "waiting_for_user" => ("等你说话", "#f59e0b"),
        "dead" => ("已结束", "#666"),
        _ => ("空闲", "#666"),
    }
}

fn render_session_row(info: &SessionInfo, token: &str, nested: bool) -> String {
    let id = html_escape(&info.id);
    let token = html_escape(token);
    let name = html_escape(&info.name);
    let (label, color) = phase_label(&info.phase);
    let question = info
        .pending_question
        .as_deref()
        .map(|q| format!("<div class=\"question\">{}</div>", html_escape(q)))
        .unwrap_or_default();
    let nested_cls = if nested { " nested" } else { "" };
    format!(
        "<li class=\"session{nested_cls}\" data-phase=\"{phase}\">\
           <div class=\"row\">\
             <span class=\"dot\" style=\"background:{color}\"></span>\
             <a class=\"primary\" href=\"/s/{id}/console?token={token}\" title=\"{id}\">{name}</a>\
             <span class=\"label\">{label}</span>\
           </div>\
           {question}\
           <a class=\"secondary\" href=\"/s/{id}?token={token}\">完整终端 →</a>\
         </li>",
        phase = html_escape(&info.phase),
        nested_cls = nested_cls,
    )
}

/// 按「项目 →（可选）父会话 → 终端」渲染，形态对齐 PC 侧栏。
/// 组内顺序跟 `list_sessions_info` 一致（workspace.json 会话序），单 pane 与
/// 多 pane 组按首次出现交错，不会把所有单会话都堆到多 pane 前面。
fn render_session_list(infos: &[SessionInfo], token: &str) -> String {
    if infos.is_empty() {
        return LIST_PAGE.replace("__ROWS__", "<div class=\"empty\">目前没有活会话</div>");
    }

    let mut project_order: Vec<String> = Vec::new();
    for info in infos {
        if !project_order.iter().any(|p| p == &info.project) {
            project_order.push(info.project.clone());
        }
    }

    let mut html = String::new();
    for project in &project_order {
        let in_project: Vec<&SessionInfo> =
            infos.iter().filter(|i| &i.project == project).collect();
        html.push_str(&format!(
            "<section class=\"project\">\
               <div class=\"project-name\">📁 {}</div>\
               <ul class=\"session-list\">",
            html_escape(project)
        ));

        // 按 infos 顺序走一遍：遇到新 parent 开一组，遇到无 parent 直接出一行。
        let mut emitted_parents: Vec<String> = Vec::new();
        for info in &in_project {
            match &info.parent_session {
                None => {
                    html.push_str(&render_session_row(info, token, false));
                }
                Some(parent) => {
                    if emitted_parents.iter().any(|p| p == parent) {
                        continue; // 整组已经在首次遇到时画完
                    }
                    emitted_parents.push(parent.clone());
                    html.push_str(&format!(
                        "<li class=\"session-group\">\
                           <div class=\"group-name\">⊞ {}</div>\
                           <ul class=\"nested-list\">",
                        html_escape(parent)
                    ));
                    for child in &in_project {
                        if child.parent_session.as_deref() == Some(parent.as_str()) {
                            html.push_str(&render_session_row(child, token, true));
                        }
                    }
                    html.push_str("</ul></li>");
                }
            }
        }

        html.push_str("</ul></section>");
    }

    LIST_PAGE.replace("__ROWS__", &html)
}

async fn list_page_handler(
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    let infos = tokio::task::spawn_blocking(list_sessions_info)
        .await
        .unwrap_or_default();
    Html(render_session_list(&infos, &q.token)).into_response()
}

async fn sessions_json_handler(
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    let infos = tokio::task::spawn_blocking(list_sessions_info)
        .await
        .unwrap_or_default();
    Json(serde_json::json!({ "sessions": infos })).into_response()
}

async fn page_handler(
    Path(id): Path<String>,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    let page = REFERENCE_PAGE
        .replace("__ID_JSON__", &js_string_literal(&id))
        .replace("__TOKEN_JSON__", &js_string_literal(&q.token))
        .replace(
            "__WRITE_ENABLED__",
            if state.write_enabled { "true" } else { "false" },
        );
    Html(page).into_response()
}

async fn stream_handler(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    ws.on_upgrade(move |socket| pump_watch(socket, id))
        .into_response()
}

/// Phase 5+6：手机友好的"操作台"——大状态 + 问题文案，不嵌 xterm（roadmap 原则 3：
/// 「不绑死 xterm.js」）。`write_enabled` 决定页面要不要显示 approve/deny/reply
/// 按钮——纯布尔值，不是用户输入，直接拼字面量，不走 js_string_literal。
async fn console_handler(
    Path(id): Path<String>,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    // 展示名从 workspace 元数据 / list 同源逻辑解析，避免操作台只显示丑陋 uuid。
    let id_for_meta = id.clone();
    let meta = tokio::task::spawn_blocking(move || {
        let gui = load_gui_leaf_meta();
        let infos = list_sessions_info();
        infos
            .into_iter()
            .find(|i| i.id == id_for_meta)
            .map(|i| (i.name, i.project, i.parent_session))
            .or_else(|| {
                let g = gui.get(&id_for_meta);
                let (name, parent, project) = resolve_display_name(
                    &id_for_meta,
                    g.and_then(|x| x.cwd.as_deref()),
                    None,
                    None,
                    g,
                );
                Some((name, project, parent))
            })
            .unwrap_or_else(|| (short_id(&id_for_meta), "会话".into(), None))
    })
    .await
    .unwrap_or_else(|_| (short_id(&id), "会话".into(), None));

    let (name, project, parent) = meta;
    let subtitle = match parent {
        Some(p) if p != name => format!("{project} · {p}"),
        _ => project,
    };
    let page = CONSOLE_PAGE
        .replace("__ID_JSON__", &js_string_literal(&id))
        .replace("__TOKEN_JSON__", &js_string_literal(&q.token))
        .replace("__NAME_JSON__", &js_string_literal(&name))
        .replace("__SUBTITLE_JSON__", &js_string_literal(&subtitle))
        .replace(
            "__WRITE_ENABLED__",
            if state.write_enabled { "true" } else { "false" },
        );
    Html(page).into_response()
}

async fn state_stream_handler(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    ws.on_upgrade(move |socket| pump_state(socket, id))
        .into_response()
}

/// 操作台的状态流：连 smeltd 的 `subscribe`（全量订阅），按 id 过滤只转发这一个
/// 会话的变化。首帧快照里如果已经有这个 id，也转发一次，页面一打开就有内容，
/// 不用干等下一次状态变化。
async fn pump_state(mut socket: WebSocket, id: String) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<serde_json::Value>(16);
    let task = tokio::task::spawn_blocking(move || subscribe_and_forward(&id, tx));

    while let Some(state) = rx.recv().await {
        if socket
            .send(Message::Text(state.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }
    let _ = task.await;
    drop(socket);
}

/// 阻塞线程里跑：连 smeltd 的 subscribe，逐行解析，只把匹配这个 id 的状态塞进
/// channel——subscribe 本身是全量订阅（见 smeltd.rs 的 Subscribers），过滤是
/// 网关自己做的，不改 smeltd 协议。
fn subscribe_and_forward(id: &str, tx: tokio::sync::mpsc::Sender<serde_json::Value>) {
    let Ok(conn) = UnixStream::connect(sock_path()) else {
        return;
    };
    let Ok(mut writer) = conn.try_clone() else {
        return;
    };
    if writeln!(writer, "{}", serde_json::json!({ "op": "subscribe" })).is_err() {
        return;
    }
    let reader = BufReader::new(conn);
    for line in reader.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(sessions) = v.get("sessions").and_then(|s| s.as_array()) {
            if let Some(state) = sessions
                .iter()
                .find(|s| s.get("id").and_then(|i| i.as_str()) == Some(id))
            {
                if tx.blocking_send(state.clone()).is_err() {
                    return;
                }
            }
        } else if let Some(session) = v.get("session") {
            if session.get("id").and_then(|i| i.as_str()) == Some(id) {
                if tx.blocking_send(session.clone()).is_err() {
                    return;
                }
            }
        }
    }
}

async fn action_handler(
    Path(id): Path<String>,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
    Json(body): Json<ActionBody>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    if !state.write_enabled {
        return (StatusCode::FORBIDDEN, "这条链接没有写权限").into_response();
    }
    let result =
        tokio::task::spawn_blocking(move || send_action(&id, &body.kind, body.text.as_deref()))
            .await
            .unwrap_or_else(|_| serde_json::json!({ "ok": false, "err": "内部错误" }));
    Json(result).into_response()
}

/// 阻塞：连 smeltd 发一次 `action` op，读一行回执。
fn read_smeltd_reply(conn: UnixStream) -> serde_json::Value {
    let mut line = String::new();
    match BufReader::new(conn).read_line(&mut line) {
        Ok(0) | Err(_) => {
            // 老版本 smeltd 不认识 op 时直接关连接、不回一行——以前会变成含糊的「响应解析失败」
            return serde_json::json!({
                "ok": false,
                "err": "守护没有响应（多半版本偏旧，请在 Mac 设置里「无缝升级」smeltd 后再试）"
            });
        }
        Ok(_) => {}
    }
    let line = line.trim();
    if line.is_empty() {
        return serde_json::json!({
            "ok": false,
            "err": "守护返回空响应（多半版本偏旧，请无缝升级 smeltd）"
        });
    }
    serde_json::from_str(line).unwrap_or_else(|_| {
        serde_json::json!({ "ok": false, "err": format!("守护响应无法解析：{}", &line[..line.len().min(80)]) })
    })
}

fn send_action(id: &str, kind: &str, text: Option<&str>) -> serde_json::Value {
    let Ok(mut conn) = UnixStream::connect(sock_path()) else {
        return serde_json::json!({ "ok": false, "err": "连不上守护" });
    };
    let req = serde_json::json!({ "op": "action", "id": id, "kind": kind, "text": text });
    if writeln!(conn, "{req}").is_err() {
        return serde_json::json!({ "ok": false, "err": "发送失败" });
    }
    read_smeltd_reply(conn)
}

/// Phase 6 补齐：原始键盘/粘贴。`write_enabled` 与 action 同一把锁；**不做** phase
/// 门闩（工作延续：Ctrl+C / TUI 导航 / agent 忙时补一句都必须能进）。门闩只留给
/// action 防误点批准。
async fn input_handler(
    Path(id): Path<String>,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
    Json(body): Json<InputBody>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    if !state.write_enabled {
        return (StatusCode::FORBIDDEN, "这条链接没有写权限").into_response();
    }
    if body.data.is_empty() {
        return Json(serde_json::json!({ "ok": false, "err": "需要非空 data" })).into_response();
    }
    let result = tokio::task::spawn_blocking(move || send_input(&id, &body.data))
        .await
        .unwrap_or_else(|_| serde_json::json!({ "ok": false, "err": "内部错误" }));
    Json(result).into_response()
}

fn send_input(id: &str, data: &str) -> serde_json::Value {
    let Ok(mut conn) = UnixStream::connect(sock_path()) else {
        return serde_json::json!({ "ok": false, "err": "连不上守护" });
    };
    let req = serde_json::json!({ "op": "input", "id": id, "data": data });
    if writeln!(conn, "{req}").is_err() {
        return serde_json::json!({ "ok": false, "err": "发送失败" });
    }
    read_smeltd_reply(conn)
}

/// 手机按视口改 PTY 尺寸。不要求 write_enabled——只读观战也需要正确排版，
/// 否则桌面大窗口镜像过来底部永远空一截（不是 xterm 画坏了）。
async fn resize_handler(
    Path(id): Path<String>,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
    Json(body): Json<ResizeBody>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    if body.cols == 0 || body.rows == 0 {
        return Json(serde_json::json!({ "ok": false, "err": "cols/rows 必须 > 0" }))
            .into_response();
    }
    // 防离谱尺寸
    let cols = body.cols.min(300);
    let rows = body.rows.min(200);
    let result =
        tokio::task::spawn_blocking(move || send_resize(&id, cols, rows, body.cell_w, body.cell_h))
            .await
            .unwrap_or_else(|_| serde_json::json!({ "ok": false, "err": "内部错误" }));
    Json(result).into_response()
}

fn send_resize(id: &str, cols: u16, rows: u16, cell_w: u16, cell_h: u16) -> serde_json::Value {
    let Ok(mut conn) = UnixStream::connect(sock_path()) else {
        return serde_json::json!({ "ok": false, "err": "连不上守护" });
    };
    let req = serde_json::json!({
        "op": "resize",
        "id": id,
        "cols": cols,
        "rows": rows,
        "cell_w": cell_w,
        "cell_h": cell_h,
    });
    if writeln!(conn, "{req}").is_err() {
        return serde_json::json!({ "ok": false, "err": "发送失败" });
    }
    read_smeltd_reply(conn)
}

/// 从阻塞的 smeltd watch 连接搬到这条 WS 上的一帧：Header 只在开头发一次，
/// 后面全是 Bytes——顺序必须保持（客户端先按 cols/rows 定尺寸，再写快照）。
enum Frame {
    Header { cols: u16, rows: u16 },
    Bytes(Vec<u8>),
}

/// 连 smeltd.sock 的只读 watch，把字节流转成 WS 二进制消息推给浏览器。
/// stream WS 本身只读画面；可写走独立的 `POST …/input`（见 input_handler），不混在这条
/// WS 上——避免和 fan-out 的只读旁观语义缠在一起。
async fn pump_watch(mut socket: WebSocket, id: String) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Frame>(64);
    // smeltd 那端是阻塞 IO，丢进阻塞线程池，不占用 tokio 的 async 执行器。
    let task = tokio::task::spawn_blocking(move || watch_and_forward(&id, tx));

    while let Some(frame) = rx.recv().await {
        let msg = match frame {
            Frame::Header { cols, rows } => Message::Text(
                serde_json::json!({ "cols": cols, "rows": rows })
                    .to_string()
                    .into(),
            ),
            Frame::Bytes(b) => Message::Binary(b.into()),
        };
        if socket.send(msg).await.is_err() {
            break;
        }
    }
    let _ = task.await;
    drop(socket); // WS 连接随 drop 关闭，不需要显式 close 帧
}

/// 阻塞线程里跑：连 smeltd、发 watch、读 header、snapshot、后续实时字节，
/// 都塞进 channel 交给上面那个 async 循环转发。
fn watch_and_forward(id: &str, tx: tokio::sync::mpsc::Sender<Frame>) {
    let Ok(conn) = UnixStream::connect(sock_path()) else {
        return;
    };
    let Ok(mut writer) = conn.try_clone() else {
        return;
    };
    if writeln!(writer, "{}", serde_json::json!({ "op": "watch", "id": id })).is_err() {
        return;
    }
    let mut reader = BufReader::new(conn);

    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return; // 会话不存在：smeltd 直接关连接，什么都不发（见 handle_watch）
    }
    let Ok(header) = serde_json::from_str::<serde_json::Value>(&line) else {
        return;
    };
    let cols = header["cols"].as_u64().unwrap_or(80) as u16;
    let rows = header["rows"].as_u64().unwrap_or(24) as u16;
    let replay_len = header["replay_len"].as_u64().unwrap_or(0) as usize;

    if tx.blocking_send(Frame::Header { cols, rows }).is_err() {
        return;
    }

    if replay_len > 0 {
        let mut snap = vec![0u8; replay_len];
        if reader.read_exact(&mut snap).is_err() {
            return;
        }
        if tx.blocking_send(Frame::Bytes(snap)).is_err() {
            return;
        }
    }

    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.blocking_send(Frame::Bytes(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// ACP 路由（移动端）
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

impl MobileLifecycleHub {
    fn start() -> Arc<Self> {
        let (updates, _) = tokio::sync::broadcast::channel(128);
        let hub = Arc::new(Self {
            state: Mutex::new(MobileLifecycleState::default()),
            updates,
        });
        let weak = Arc::downgrade(&hub);
        std::thread::spawn(move || mobile_lifecycle_subscription(weak));
        hub
    }

    fn apply_snapshot(&self, sessions: Vec<DaemonSessionState>) {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        let mut resolved = Vec::new();
        let ids: std::collections::HashSet<_> =
            sessions.iter().map(|session| session.id.clone()).collect();
        for session in &sessions {
            let had_unresolved_action = state.attention.has_unresolved_action(&session.id);
            let previous = state.sessions.get(&session.id).map(|old| old.phase);
            apply_daemon_transition(&mut state.attention, previous, session, now);
            if had_unresolved_action && !state.attention.has_unresolved_action(&session.id) {
                resolved.push(session.id.clone());
            }
        }
        let stale: Vec<_> = state
            .sessions
            .keys()
            .filter(|id| !ids.contains(*id))
            .cloned()
            .collect();
        for id in stale {
            if state.attention.has_unresolved_action(&id) {
                resolved.push(id.clone());
            }
            state.attention.remove_session(&id);
        }
        state.sessions = sessions
            .into_iter()
            .map(|session| (session.id.clone(), session))
            .collect();
        drop(state);
        let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
        for session_id in resolved {
            let _ = self
                .updates
                .send(MobileLifecycleEvent::AttentionResolved(session_id));
        }
    }

    fn apply_update(&self, session: DaemonSessionState) {
        let mut state = self.state.lock().unwrap();
        let had_unresolved_action = state.attention.has_unresolved_action(&session.id);
        let previous = state.sessions.get(&session.id).map(|old| old.phase);
        let attention =
            apply_daemon_transition(&mut state.attention, previous, &session, Instant::now());
        let resolved = had_unresolved_action && !state.attention.has_unresolved_action(&session.id);
        let session_id = session.id.clone();
        state.sessions.insert(session.id.clone(), session);
        drop(state);
        let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
        if let Some(item) = attention {
            let _ = self.updates.send(MobileLifecycleEvent::Attention(item));
        }
        if resolved {
            let _ = self
                .updates
                .send(MobileLifecycleEvent::AttentionResolved(session_id));
        }
    }

    fn mark_read(&self, session_id: &str) -> bool {
        let changed = self
            .state
            .lock()
            .unwrap()
            .attention
            .mark_read(session_id)
            .is_some();
        if changed {
            let _ = self.updates.send(MobileLifecycleEvent::SessionsChanged);
        }
        changed
    }

    fn summaries(&self) -> Vec<AcpSessionSummary> {
        let gui_titles = load_gui_acp_titles();
        let workspace_order = load_mobile_workspace_order();
        let state = self.state.lock().unwrap();
        let mut summaries: Vec<_> = state
            .sessions
            .values()
            .filter_map(|session| {
                acp_summary_from_daemon(session, &gui_titles, &workspace_order, &state.attention)
            })
            .collect();
        summaries.sort_by(|a, b| {
            a.project_order
                .cmp(&b.project_order)
                .then(a.session_order.cmp(&b.session_order))
                .then(a.leaf_order.cmp(&b.leaf_order))
                .then(a.title.cmp(&b.title))
                .then(a.id.cmp(&b.id))
        });
        summaries
    }
}

fn mobile_lifecycle_subscription(hub: Weak<MobileLifecycleHub>) {
    while hub.strong_count() > 0 {
        if let Ok(mut stream) = UnixStream::connect(sock_path()) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            if writeln!(stream, "{}", serde_json::json!({ "op": "subscribe" })).is_ok() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                loop {
                    if hub.strong_count() == 0 {
                        return;
                    }
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => {
                            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                                continue;
                            };
                            let Some(hub) = hub.upgrade() else {
                                return;
                            };
                            if let Some(sessions) = value.get("sessions") {
                                if let Ok(sessions) = serde_json::from_value(sessions.clone()) {
                                    hub.apply_snapshot(sessions);
                                }
                            } else if let Some(session) = value.get("session") {
                                if let Ok(session) = serde_json::from_value(session.clone()) {
                                    hub.apply_update(session);
                                }
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => break,
                    }
                }
            }
        }
        if hub.strong_count() == 0 {
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// ACP 会话摘要（移动端列表用）
#[derive(serde::Serialize)]
struct AcpSessionSummary {
    id: String,
    title: String,
    phase: String,
    status: String,
    agent: String,
    cwd: Option<String>,
    project_root: Option<String>,
    project_title: Option<String>,
    project_order: u32,
    session_order: u32,
    leaf_order: u32,
    updated_at: i64,
    detail: Option<String>,
    unread: bool,
    attention: Option<AttentionItem>,
}

fn daemon_phase_name(phase: DaemonPhase) -> &'static str {
    match phase {
        DaemonPhase::Thinking => "thinking",
        DaemonPhase::ExecutingTool => "executing_tool",
        DaemonPhase::AwaitingApproval => "awaiting_approval",
        DaemonPhase::WaitingForUser => "waiting_for_user",
        DaemonPhase::Succeeded => "succeeded",
        DaemonPhase::Failed => "failed",
        DaemonPhase::Idle => "idle",
        DaemonPhase::Dead => "dead",
    }
}

fn mobile_status_name(phase: DaemonPhase, unread: bool) -> &'static str {
    match AgentStatus::from_daemon_phase(phase) {
        Some(AgentStatus::WaitingApproval) => "waiting_approval",
        Some(AgentStatus::NeedsAttention) => "needs_attention",
        Some(AgentStatus::Running) => "running",
        Some(AgentStatus::Done) if unread => "done",
        Some(AgentStatus::Done) | Some(AgentStatus::Idle) | None => "idle",
    }
}

fn agent_from_launch(launch: &str) -> &'static str {
    if launch.contains("claude") {
        "claude"
    } else if launch.contains("copilot") {
        "copilot"
    } else if launch.contains("codex") {
        "codex"
    } else if launch.contains("grok") {
        "grok"
    } else {
        "other"
    }
}

fn acp_summary_from_daemon(
    session: &DaemonSessionState,
    gui_titles: &std::collections::HashMap<String, String>,
    workspace_order: &MobileWorkspaceOrder,
    attention_store: &AttentionStore,
) -> Option<AcpSessionSummary> {
    let launch = session.launch.as_deref()?;
    let attention = attention_store.unread(&session.id).cloned();
    let unread = attention.is_some();
    let title = gui_titles
        .get(&session.id)
        .cloned()
        .or_else(|| session.title.clone().filter(|title| !title.is_empty()))
        .or_else(|| {
            session
                .cwd
                .as_ref()
                .and_then(|path| std::path::Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .map(String::from)
        })
        .unwrap_or_else(|| session.id.clone());
    let (session_order, leaf_order) = workspace_order
        .sessions
        .get(&session.id)
        .copied()
        .unwrap_or((usize::MAX, usize::MAX));
    let project_root = session.cwd.as_deref().and_then(|cwd| {
        mobile_project_root(&workspace_order.projects, cwd)
            .or_else(|| Some(cwd.trim_end_matches('/').to_string()))
    });
    let project_order = project_root
        .as_deref()
        .and_then(|root| {
            workspace_order
                .projects
                .iter()
                .position(|project| project.trim_end_matches('/') == root)
        })
        .unwrap_or_else(|| workspace_order.projects.len().saturating_add(session_order));
    let project_title = project_root
        .as_deref()
        .and_then(|root| std::path::Path::new(root).file_name())
        .and_then(|name| name.to_str())
        .map(String::from);
    Some(AcpSessionSummary {
        id: session.id.clone(),
        title,
        phase: daemon_phase_name(session.phase).to_string(),
        status: mobile_status_name(session.phase, unread).to_string(),
        agent: agent_from_launch(launch).to_string(),
        cwd: session.cwd.clone(),
        project_root,
        project_title,
        project_order: project_order.min(u32::MAX as usize) as u32,
        session_order: session_order.min(u32::MAX as usize) as u32,
        leaf_order: leaf_order.min(u32::MAX as usize) as u32,
        updated_at: session.updated_at.min(i64::MAX as u64) as i64,
        detail: session.detail_line(),
        unread,
        attention,
    })
}

/// GET /acp/sessions - 列出所有 ACP 会话
async fn acp_sessions_handler(
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "invalid token"})),
        )
            .into_response();
    }

    let sessions = state.mobile_lifecycle.summaries();

    Json(serde_json::json!({
        "sessions": sessions
    }))
    .into_response()
}

/// WebSocket 消息类型（移动端 → 服务端）
#[derive(serde::Deserialize)]
#[serde(tag = "method")]
enum AcpWsRequest {
    #[serde(rename = "subscribe")]
    Subscribe { params: SubscribeParams },
    #[serde(rename = "unsubscribe")]
    Unsubscribe,
    #[serde(rename = "sendMessage")]
    SendMessage { params: SendMessageParams },
    #[serde(rename = "respondApproval")]
    RespondApproval { params: ApprovalParams },
    #[serde(rename = "chooseElicitation")]
    ChooseElicitation { params: ElicitationChoiceParams },
    #[serde(rename = "updateElicitationText")]
    UpdateElicitationText { params: ElicitationTextParams },
    #[serde(rename = "submitElicitation")]
    SubmitElicitation { params: SessionActionParams },
    #[serde(rename = "dismissElicitation")]
    DismissElicitation { params: SessionActionParams },
    #[serde(rename = "listSessions")]
    ListSessions,
    #[serde(rename = "markRead")]
    MarkRead {
        #[allow(dead_code)]
        params: MarkReadParams,
    },
}

#[derive(serde::Deserialize)]
struct SubscribeParams {
    #[serde(rename = "sessionId")]
    session_id: String,
}

#[derive(serde::Deserialize)]
struct SendMessageParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    content: String,
}

#[derive(serde::Deserialize)]
struct ApprovalParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "toolCallId")]
    tool_call_id: String,
    #[serde(rename = "optionKey")]
    option_key: String,
    #[serde(rename = "customText")]
    custom_text: Option<String>,
}

#[derive(serde::Deserialize)]
struct ElicitationChoiceParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "fieldIndex")]
    field_index: usize,
    #[serde(rename = "optionIndex")]
    option_index: usize,
}

#[derive(serde::Deserialize)]
struct ElicitationTextParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "fieldIndex")]
    field_index: usize,
    value: String,
}

#[derive(serde::Deserialize)]
struct SessionActionParams {
    #[serde(rename = "sessionId")]
    session_id: String,
}

#[derive(serde::Deserialize)]
struct MarkReadParams {
    #[allow(dead_code)]
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// GET /acp/ws - ACP WebSocket 连接（移动端用）
async fn acp_ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    ws.on_upgrade(move |socket| acp_ws_pump(socket, state))
        .into_response()
}

/// ACP WebSocket 主循环
async fn acp_ws_pump(socket: WebSocket, state: AppState) {
    use futures::stream::StreamExt;
    use tokio::sync::mpsc;

    let (mut ws_tx, mut ws_rx) = socket.split();
    let write_enabled = state.write_enabled;
    let mut lifecycle_rx = state.mobile_lifecycle.updates.subscribe();

    // 发送欢迎消息
    let welcome = serde_json::json!({
        "type": "connected",
        "writeEnabled": write_enabled,
    });
    if futures::SinkExt::send(&mut ws_tx, Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // 用于从后台任务接收 smeltd 推送
    let (daemon_tx, mut daemon_rx) = mpsc::channel::<String>(64);

    // 每个 watcher 都有自己的停止信号；watch channel 一旦变成 true 不会自动复位，
    // 不能跨多次 subscribe 复用。
    let mut current_subscription: Option<(
        String,
        tokio::sync::watch::Sender<bool>,
        tokio::task::JoinHandle<()>,
    )> = None;

    loop {
        tokio::select! {
            // 接收来自移动端的消息
            msg = ws_rx.next() => {
                let Some(Ok(msg)) = msg else {
                    break;
                };

                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => break,
                    _ => continue,
                };

                let Ok(req): Result<AcpWsRequest, _> = serde_json::from_str(&text) else {
                    let err = serde_json::json!({"type": "error", "error": "invalid request"});
                    let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(err.to_string().into())).await;
                    continue;
                };

                match req {
                    AcpWsRequest::ListSessions => {
                        let sessions = state.mobile_lifecycle.summaries();
                        let resp = serde_json::json!({
                            "type": "sessions",
                            "sessions": sessions,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::Subscribe { params } => {
                        // 停止旧的订阅
                        if let Some((_, stop_tx, handle)) = current_subscription.take() {
                            let _ = stop_tx.send(true);
                            handle.abort();
                        }

                        // 启动新的订阅
                        let session_id = params.session_id.clone();
                        let tx = daemon_tx.clone();
                        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

                        let handle = tokio::task::spawn_blocking(move || {
                            acp_watch_loop(&session_id, tx, stop_rx);
                        });

                        current_subscription =
                            Some((params.session_id.clone(), stop_tx, handle));

                        let resp = serde_json::json!({
                            "type": "subscribed",
                            "sessionId": params.session_id,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::Unsubscribe => {
                        if let Some((_, stop_tx, handle)) = current_subscription.take() {
                            let _ = stop_tx.send(true);
                            handle.abort();
                        }
                        let resp = serde_json::json!({"type": "unsubscribed"});
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::SendMessage { params } => {
                        if !write_enabled {
                            let err = serde_json::json!({"type": "error", "error": "write not enabled"});
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(err.to_string().into())).await;
                            continue;
                        }

                        let session_id = params.session_id.clone();
                        let content = params.content.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            send_acp_message(&session_id, &content)
                        }).await;

                        let resp = match result {
                            Ok(Ok(())) => serde_json::json!({"type": "messageSent", "ok": true}),
                            Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
                            Err(error) => serde_json::json!({
                                "type": "error",
                                "error": format!("failed to dispatch message: {error}"),
                            }),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::RespondApproval { params } => {
                        if !write_enabled {
                            let err = serde_json::json!({"type": "error", "error": "write not enabled"});
                            let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(err.to_string().into())).await;
                            continue;
                        }

                        let session_id = params.session_id.clone();
                        let option_key = params.option_key.clone();
                        let custom_text = params.custom_text.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            respond_acp_approval(
                                &session_id,
                                &params.tool_call_id,
                                &option_key,
                                custom_text.as_deref(),
                            )
                        }).await;

                        let resp = match result {
                            Ok(Ok(())) => serde_json::json!({"type": "approvalResponded", "ok": true}),
                            Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
                            Err(error) => serde_json::json!({
                                "type": "error",
                                "error": format!("failed to dispatch approval: {error}"),
                            }),
                        };
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::ChooseElicitation { params } => {
                        let action = serde_json::json!({
                            "ElicitationChoose": {
                                "field_ix": params.field_index,
                                "opt_ix": params.option_index,
                            }
                        });
                        let resp = dispatch_mobile_action(
                            write_enabled,
                            params.session_id,
                            action,
                            "elicitation choice",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::UpdateElicitationText { params } => {
                        let action = serde_json::json!({
                            "ElicitationText": {
                                "field_ix": params.field_index,
                                "value": params.value,
                            }
                        });
                        let resp = dispatch_mobile_action(
                            write_enabled,
                            params.session_id,
                            action,
                            "elicitation text",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::SubmitElicitation { params } => {
                        let resp = dispatch_mobile_action(
                            write_enabled,
                            params.session_id,
                            serde_json::json!("ElicitationSubmit"),
                            "elicitation submission",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::DismissElicitation { params } => {
                        let resp = dispatch_mobile_action(
                            write_enabled,
                            params.session_id,
                            serde_json::json!("ElicitationDismiss"),
                            "elicitation dismissal",
                        ).await;
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    AcpWsRequest::MarkRead { params } => {
                        let changed = state.mobile_lifecycle.mark_read(&params.session_id);
                        let resp = serde_json::json!({
                            "type": "markedRead",
                            "ok": true,
                            "changed": changed,
                            "sessionId": params.session_id,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                }
            }
            lifecycle = lifecycle_rx.recv() => {
                match lifecycle {
                    Ok(MobileLifecycleEvent::SessionsChanged) => {
                        let resp = serde_json::json!({
                            "type": "sessions",
                            "sessions": state.mobile_lifecycle.summaries(),
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    Ok(MobileLifecycleEvent::Attention(item)) => {
                        let resp = serde_json::json!({"type": "attention", "item": item});
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    Ok(MobileLifecycleEvent::AttentionResolved(session_id)) => {
                        let resp = serde_json::json!({
                            "type": "attentionResolved",
                            "sessionId": session_id,
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let resp = serde_json::json!({
                            "type": "sessions",
                            "sessions": state.mobile_lifecycle.summaries(),
                        });
                        let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(resp.to_string().into())).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // 接收来自 smeltd 的推送，直接透传原始格式
            Some(line) = daemon_rx.recv() => {
                // 直接转发 smeltd 的原始 JSON（与 PC GUI 一致）
                let _ = futures::SinkExt::send(&mut ws_tx, Message::Text(line.into())).await;
            }
        }
    }

    // 清理
    if let Some((_, stop_tx, handle)) = current_subscription.take() {
        let _ = stop_tx.send(true);
        handle.abort();
    }
}

async fn dispatch_mobile_action(
    write_enabled: bool,
    session_id: String,
    action: serde_json::Value,
    description: &'static str,
) -> serde_json::Value {
    if !write_enabled {
        return serde_json::json!({"type": "error", "error": "write not enabled"});
    }
    match tokio::task::spawn_blocking(move || send_acp_action(&session_id, action)).await {
        Ok(Ok(())) => serde_json::json!({"type": "actionCompleted", "ok": true}),
        Ok(Err(error)) => serde_json::json!({"type": "error", "error": error}),
        Err(error) => serde_json::json!({
            "type": "error",
            "error": format!("failed to dispatch {description}: {error}"),
        }),
    }
}

/// 后台任务：监听 smeltd 的 acp_watch 推送
fn acp_watch_loop(
    session_id: &str,
    tx: tokio::sync::mpsc::Sender<String>,
    stop: tokio::sync::watch::Receiver<bool>,
) {
    let Ok(mut stream) = UnixStream::connect(sock_path()) else {
        return;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(100)));

    // 发送 acp_watch 请求
    let req = serde_json::json!({
        "op": "acp_watch",
        "id": session_id,
    });
    if writeln!(stream, "{}", req).is_err() {
        return;
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    loop {
        // 检查是否需要停止
        if *stop.borrow() {
            break;
        }

        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    // 发送到 WebSocket 任务
                    if tx.blocking_send(trimmed.to_string()).is_err() {
                        break;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // 超时，继续循环检查 stop
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
}

fn send_acp_action(session_id: &str, action: serde_json::Value) -> Result<(), String> {
    let mut stream =
        UnixStream::connect(sock_path()).map_err(|e| format!("connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("set timeout failed: {e}"))?;

    let req = serde_json::json!({
        "op": "acp_action",
        "id": session_id,
        "action": action,
    });
    writeln!(stream, "{req}").map_err(|e| format!("write failed: {e}"))?;

    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|e| format!("read failed: {e}"))?;
    let response: serde_json::Value =
        serde_json::from_str(&response).map_err(|e| format!("invalid response: {e}"))?;
    if response["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(response["error"]
            .as_str()
            .unwrap_or("ACP action failed")
            .to_string())
    }
}

/// 发送 ACP 消息，不占用或替换 PC GUI 的 control client。
fn send_acp_message(session_id: &str, content: &str) -> Result<(), String> {
    send_acp_action(
        session_id,
        serde_json::json!({
            "Prompt": {
                "text": content,
                "images": [],
            }
        }),
    )
}

/// 响应 ACP 审批请求
fn respond_acp_approval(
    session_id: &str,
    tool_call_id: &str,
    option_key: &str,
    _custom_text: Option<&str>,
) -> Result<(), String> {
    send_acp_action(
        session_id,
        serde_json::json!({
            "PermissionSelect": {
                "tool_call_id": tool_call_id,
                "option_id": option_key,
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mobile_daemon_state(phase: DaemonPhase) -> DaemonSessionState {
        DaemonSessionState {
            id: "session-mobile".into(),
            phase,
            title: Some("Mobile agent".into()),
            launch: Some("codex app-server".into()),
            cwd: Some("/tmp/mobile-project".into()),
            updated_at: 42,
            structured_events: true,
            ..Default::default()
        }
    }

    fn mobile_lifecycle_hub_for_test() -> MobileLifecycleHub {
        let (updates, _) = tokio::sync::broadcast::channel(16);
        MobileLifecycleHub {
            state: Mutex::new(MobileLifecycleState::default()),
            updates,
        }
    }

    #[test]
    fn mobile_workspace_order_uses_pc_project_and_session_order() {
        let value = serde_json::json!({
            "projects": ["/repo/two", "/repo/one"],
            "sessions": [
                {
                    "layout": {"Leaf": {"id": "terminal-first"}},
                    "acp": null
                },
                {
                    "layout": {
                        "Split": {
                            "children": [
                                {"Leaf": {"id": "terminal-a"}},
                                {"Leaf": {"id": "terminal-b"}}
                            ]
                        }
                    },
                    "acp": {"sid": "acp-second"}
                }
            ]
        });

        let order = mobile_workspace_order_from_value(&value);
        assert_eq!(order.projects, vec!["/repo/two", "/repo/one"]);
        assert_eq!(order.sessions["terminal-first"], (0, 0));
        assert_eq!(order.sessions["acp-second"], (1, 0));
        assert_eq!(order.sessions["terminal-a"], (1, 0));
        assert_eq!(order.sessions["terminal-b"], (1, 1));
    }

    #[test]
    fn mobile_project_root_matches_deepest_pc_project() {
        let projects = vec!["/repo".into(), "/repo/packages/app".into()];
        assert_eq!(
            mobile_project_root(&projects, "/repo/packages/app/src"),
            Some("/repo/packages/app".into())
        );
        assert_eq!(mobile_project_root(&projects, "/repo-other"), None);
    }

    #[test]
    fn mobile_lifecycle_marks_completed_attention_read_without_changing_phase() {
        let hub = mobile_lifecycle_hub_for_test();
        hub.apply_snapshot(vec![mobile_daemon_state(DaemonPhase::Thinking)]);
        hub.apply_update(mobile_daemon_state(DaemonPhase::Succeeded));

        let before = hub.summaries().pop().unwrap();
        assert_eq!(before.phase, "succeeded");
        assert_eq!(before.status, "done");
        assert!(before.unread);
        assert_eq!(
            before.attention.unwrap().kind,
            crate::attention::AttentionKind::Success
        );

        assert!(hub.mark_read("session-mobile"));
        let after = hub.summaries().pop().unwrap();
        assert_eq!(after.phase, "succeeded");
        assert_eq!(after.status, "idle");
        assert!(!after.unread);
        assert!(after.attention.is_none());
    }

    #[test]
    fn mobile_lifecycle_keeps_action_status_after_mark_read() {
        let hub = mobile_lifecycle_hub_for_test();
        hub.apply_snapshot(vec![mobile_daemon_state(DaemonPhase::Thinking)]);
        hub.apply_update(mobile_daemon_state(DaemonPhase::AwaitingApproval));

        assert!(hub.mark_read("session-mobile"));
        let summary = hub.summaries().pop().unwrap();
        assert_eq!(summary.status, "waiting_approval");
        assert!(!summary.unread);
    }

    #[test]
    fn mobile_lifecycle_broadcasts_when_an_action_is_resolved_elsewhere() {
        let hub = mobile_lifecycle_hub_for_test();
        let mut updates = hub.updates.subscribe();
        hub.apply_snapshot(vec![mobile_daemon_state(DaemonPhase::Thinking)]);
        hub.apply_update(mobile_daemon_state(DaemonPhase::AwaitingApproval));
        hub.apply_update(mobile_daemon_state(DaemonPhase::Thinking));

        let mut resolved = Vec::new();
        while let Ok(event) = updates.try_recv() {
            if let MobileLifecycleEvent::AttentionResolved(session_id) = event {
                resolved.push(session_id);
            }
        }
        assert_eq!(resolved, vec!["session-mobile"]);
    }

    /// 反射型 XSS 的核心防线：id/token 里带 `</script>` 不能提前把内联脚本切断。
    #[test]
    fn js_string_literal_escapes_script_breakout() {
        let evil = "</script><script>alert(1)</script>";
        let escaped = js_string_literal(evil);
        assert!(
            !escaped.contains("</script>"),
            "转义后仍含裸露的 </script>：{escaped}"
        );
        assert!(
            escaped.contains("\\u003c"),
            "尖括号应被转成 \\u003c：{escaped}"
        );
    }

    #[test]
    fn js_string_literal_escapes_quotes_and_backslashes() {
        let evil = "\"; alert(1); //\\";
        let escaped = js_string_literal(evil);
        // 必须是一个合法的、被双引号包住的 JS 字符串字面量。
        assert!(escaped.starts_with('"') && escaped.ends_with('"'));
        // 反序列化回来应该精确等于原字符串（转义没丢信息、没被破坏）。
        let roundtrip: String = serde_json::from_str(&escaped).unwrap();
        assert_eq!(roundtrip, evil);
    }

    /// 会话列表页把 id 嵌进 HTML 正文/属性——防的是 HTML 注入，不是 JS 字符串逃逸，
    /// 转义规则跟 js_string_literal 不一样，得单独测。
    #[test]
    fn html_escape_neutralizes_tag_breakout() {
        let evil = "<img src=x onerror=alert(1)>";
        let escaped = html_escape(evil);
        assert!(
            !escaped.contains('<') && !escaped.contains('>'),
            "尖括号应被转义：{escaped}"
        );
    }

    #[test]
    fn render_session_list_escapes_ids_and_handles_empty() {
        let empty = render_session_list(&[], "tok");
        assert!(empty.contains("没有活会话"));

        let evil = SessionInfo {
            id: "<script>alert(1)</script>".to_string(),
            phase: "idle".to_string(),
            pending_question: Some("<b>问题</b>".to_string()),
            name: "<img onerror=1>".to_string(),
            project: "proj<script>".to_string(),
            parent_session: None,
            cwd: None,
        };
        let page = render_session_list(&[evil], "tok");
        assert!(
            !page.contains("<script>alert(1)</script>"),
            "未转义的 id 混进了列表页：{page}"
        );
        assert!(
            !page.contains("<img onerror=1>"),
            "未转义的 name 混进了列表页：{page}"
        );
        assert!(
            page.contains("&lt;img"),
            "转义后的 name 应该出现在列表里：{page}"
        );
        assert!(
            !page.contains("<b>问题</b>"),
            "未转义的 pending_question 混进了列表页：{page}"
        );
        // 列表主标题是 name，不是裸 uuid
        assert!(page.contains("primary"), "应有主链接：{page}");
    }

    #[test]
    fn project_name_for_cwd_takes_last_segment() {
        assert_eq!(project_name_for_cwd("/Users/x/Desktop/my/smelt"), "smelt");
        assert_eq!(project_name_for_cwd("/tmp/"), "tmp");
        assert_eq!(project_name_for_cwd(""), "项目");
    }

    #[test]
    fn resolve_display_name_prefers_gui_session_title() {
        let gui = GuiLeafMeta {
            session_title: Some("claude-quant".into()),
            pane_title: None,
            cwd: Some("/p/quant-above-all".into()),
            multi_pane: false,
            session_ord: 0,
            leaf_ord: 0,
        };
        let (name, parent, project) = resolve_display_name(
            "uuid-here",
            Some("/p/quant-above-all"),
            None,
            None,
            Some(&gui),
        );
        assert_eq!(name, "claude-quant");
        assert!(parent.is_none());
        assert_eq!(project, "quant-above-all");
    }

    #[test]
    fn resolve_display_name_nests_multi_pane_under_session() {
        let gui = GuiLeafMeta {
            session_title: Some("services".into()),
            pane_title: Some("frontend".into()),
            cwd: Some("/p/quant-above-all".into()),
            multi_pane: true,
            session_ord: 0,
            leaf_ord: 0,
        };
        let (name, parent, _) =
            resolve_display_name("uuid", Some("/p/quant-above-all"), None, None, Some(&gui));
        assert_eq!(name, "frontend");
        assert_eq!(parent.as_deref(), Some("services"));
    }

    #[test]
    fn render_session_list_groups_by_project_and_shows_names() {
        let infos = vec![
            SessionInfo {
                id: "id-a".into(),
                phase: "idle".into(),
                pending_question: None,
                name: "claude-quant".into(),
                project: "quant-above-all".into(),
                parent_session: None,
                cwd: None,
            },
            SessionInfo {
                id: "id-b".into(),
                phase: "idle".into(),
                pending_question: None,
                name: "frontend".into(),
                project: "quant-above-all".into(),
                parent_session: Some("services".into()),
                cwd: None,
            },
            SessionInfo {
                id: "id-c".into(),
                phase: "waiting_for_user".into(),
                pending_question: Some("继续吗？".into()),
                name: "grok".into(),
                project: "smelt".into(),
                parent_session: None,
                cwd: None,
            },
        ];
        let page = render_session_list(&infos, "tok");
        assert!(page.contains("quant-above-all"), "应有项目组：{page}");
        assert!(page.contains("claude-quant"), "应显示会话名：{page}");
        assert!(page.contains("services"), "应有多 pane 父会话：{page}");
        assert!(page.contains("frontend"), "应有嵌套 pane 名：{page}");
        assert!(page.contains("grok"), "应有另一项目会话：{page}");
        assert!(!page.contains(">id-a<"), "主链接不该是裸 id：{page}");
    }

    /// 未知 phase（比如以后 smeltd 加了新枚举值，网关还没更新）不该 panic，
    /// 退化成一个能看的默认值。
    #[test]
    fn phase_label_falls_back_on_unknown_phase() {
        let (label, _color) = phase_label("some_future_phase_we_dont_know_yet");
        assert!(!label.is_empty());
    }

    #[test]
    fn phase_label_covers_all_known_phases() {
        for phase in [
            "thinking",
            "executing_tool",
            "awaiting_approval",
            "waiting_for_user",
            "idle",
            "dead",
        ] {
            let (label, color) = phase_label(phase);
            assert!(!label.is_empty() && color.starts_with('#'), "phase={phase}");
        }
    }
}
