//! 远程操作网关的核心逻辑（路由 + handler + HTML 模板），供两个地方 `#[path]` 引入：
//! - `src/bin/gateway.rs`：独立进程，命令行启动，自己管一个 `--bind`/`--port`
//! - `src/bin/smeltd.rs`：内嵌进守护，靠 `remote_start`/`remote_stop` op 按需开关
//!
//! 两边共用同一份 handler，避免同一套鉴权/转义/协议逻辑复制两次（CLAUDE.md 明令
//! 别复制）。这个模块本身**不碰 smeltd 主协议**：所有跟 smeltd 的交互都是走
//! `sock_path()` 连它自己的 unix socket，用既有的 `list`/`watch` op——不管是从独立
//! 进程调用还是从 smeltd 内部的这个模块调用，走的都是同一条路径，行为完全一致。
//!
//! 见 docs/remote-ops-roadmap.md（Phase 1/2）、docs/collaboration.md（安全底线）。

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

const REFERENCE_PAGE: &str = include_str!("remote_gateway_page.html");
const LIST_PAGE: &str = include_str!("remote_gateway_list_page.html");

pub fn sock_path() -> std::path::PathBuf {
    let dir = dirs::home_dir().unwrap_or_else(|| "/tmp".into()).join(".smelt");
    dir.join("smeltd.sock")
}

#[derive(Clone)]
struct AppState {
    token: Arc<String>,
}

#[derive(Deserialize)]
struct AuthQuery {
    token: String,
}

/// 组好整个网关的路由，鉴权用这一个 token（见 collaboration.md：一个网关/token 管
/// 这台机器上的全部活会话，泄漏一条链接的代价是明确的，不是没想到的疏漏）。
pub fn build_router(token: String) -> Router {
    let state = AppState { token: Arc::new(token) };
    Router::new()
        .route("/", get(list_page_handler))
        .route("/sessions", get(sessions_json_handler))
        .route("/s/{id}", get(page_handler))
        .route("/s/{id}/stream", get(stream_handler))
        .with_state(state)
}

/// 把字符串安全地嵌进内联 `<script>` 里的 JS 字符串字面量：JSON 转义处理引号/
/// 反斜杠，额外把尖括号转成 Unicode 转义序列——防止 id/token 里带 `</script>`
/// 提前把这段脚本切断（HTML 解析器找 `</script` 是纯文本匹配，不管有没有在字符串里）。
fn js_string_literal(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string()).replace('<', "\\u003c")
}

/// 把字符串安全地嵌进 HTML 正文/属性：转义 `& < > "`。会话列表页用它嵌 session id——
/// 现在 id 都是 GUI 用 `uuid::Uuid::new_v4()` 生成的（见 workspace/main.rs），字符集
/// 天然安全，这里是防御性的，防止以后 id 格式变了变成新的注入面。
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 问 smeltd 要当前活会话 id 列表（复用既有的 `list` op，不碰 smeltd 主协议）。
/// 阻塞 IO，调用方需要丢进 `spawn_blocking`。
fn list_sessions() -> Vec<String> {
    let Ok(conn) = UnixStream::connect(sock_path()) else { return Vec::new() };
    let Ok(mut writer) = conn.try_clone() else { return Vec::new() };
    if writeln!(writer, "{}", serde_json::json!({ "op": "list" })).is_err() {
        return Vec::new();
    }
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return Vec::new();
    }
    serde_json::from_str::<serde_json::Value>(&line)
        .ok()
        .and_then(|v| {
            v["sessions"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        })
        .unwrap_or_default()
}

fn render_session_list(ids: &[String], token: &str) -> String {
    let rows = if ids.is_empty() {
        "<li class=\"empty\">目前没有活会话</li>".to_string()
    } else {
        ids.iter()
            .map(|id| {
                let id = html_escape(id);
                let token = html_escape(token);
                format!("<li><a href=\"/s/{id}?token={token}\">{id}</a></li>")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    LIST_PAGE.replace("__ROWS__", &rows)
}

async fn list_page_handler(Query(q): Query<AuthQuery>, State(state): State<AppState>) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    let ids = tokio::task::spawn_blocking(list_sessions).await.unwrap_or_default();
    Html(render_session_list(&ids, &q.token)).into_response()
}

async fn sessions_json_handler(
    Query(q): Query<AuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if q.token != *state.token {
        return (StatusCode::FORBIDDEN, "token 不对").into_response();
    }
    let ids = tokio::task::spawn_blocking(list_sessions).await.unwrap_or_default();
    Json(serde_json::json!({ "sessions": ids })).into_response()
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
        .replace("__TOKEN_JSON__", &js_string_literal(&q.token));
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
    ws.on_upgrade(move |socket| pump_watch(socket, id)).into_response()
}

/// 从阻塞的 smeltd watch 连接搬到这条 WS 上的一帧：Header 只在开头发一次，
/// 后面全是 Bytes——顺序必须保持（客户端先按 cols/rows 定尺寸，再写快照）。
enum Frame {
    Header { cols: u16, rows: u16 },
    Bytes(Vec<u8>),
}

/// 连 smeltd.sock 的只读 watch，把字节流转成 WS 二进制消息推给浏览器。
/// 只读：不接受浏览器发回来的任何字节——明确不做可写（见 remote-ops-roadmap Phase 5）。
async fn pump_watch(mut socket: WebSocket, id: String) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Frame>(64);
    // smeltd 那端是阻塞 IO，丢进阻塞线程池，不占用 tokio 的 async 执行器。
    let task = tokio::task::spawn_blocking(move || watch_and_forward(&id, tx));

    while let Some(frame) = rx.recv().await {
        let msg = match frame {
            Frame::Header { cols, rows } => {
                Message::Text(serde_json::json!({ "cols": cols, "rows": rows }).to_string().into())
            }
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
    let Ok(conn) = UnixStream::connect(sock_path()) else { return };
    let Ok(mut writer) = conn.try_clone() else { return };
    if writeln!(writer, "{}", serde_json::json!({ "op": "watch", "id": id })).is_err() {
        return;
    }
    let mut reader = BufReader::new(conn);

    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return; // 会话不存在：smeltd 直接关连接，什么都不发（见 handle_watch）
    }
    let Ok(header) = serde_json::from_str::<serde_json::Value>(&line) else { return };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 反射型 XSS 的核心防线：id/token 里带 `</script>` 不能提前把内联脚本切断。
    #[test]
    fn js_string_literal_escapes_script_breakout() {
        let evil = "</script><script>alert(1)</script>";
        let escaped = js_string_literal(evil);
        assert!(!escaped.contains("</script>"), "转义后仍含裸露的 </script>：{escaped}");
        assert!(escaped.contains("\\u003c"), "尖括号应被转成 \\u003c：{escaped}");
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
        assert!(!escaped.contains('<') && !escaped.contains('>'), "尖括号应被转义：{escaped}");
    }

    #[test]
    fn render_session_list_escapes_ids_and_handles_empty() {
        let empty = render_session_list(&[], "tok");
        assert!(empty.contains("没有活会话"));

        let evil_id = "<script>alert(1)</script>".to_string();
        let page = render_session_list(&[evil_id], "tok");
        assert!(!page.contains("<script>alert(1)</script>"), "未转义的 id 混进了列表页：{page}");
        assert!(page.contains("&lt;script&gt;"), "转义后的 id 应该出现在列表里：{page}");
    }
}
