//! `smeltd --peer-mcp`：内置极简 stdio MCP Server，供第三方 ACP 智能体跨对话协作。
//!
//! 只做两件事：
//! 1. 实现标准 MCP stdio JSON-RPC 2.0 握手与 `tools/list`、`tools/call`；
//! 2. 把调用原样经 `SMELT_PEER_SOCK` 转发给该会话绑定的 `PeerSocket`（来源由 `smeltd`
//!    按套接字认定，工具参数里没有来源字段）。
//!
//! 关键兼容性要求（已实测 `agy acp` 1.5.14）：
//! - 对任何不认识的请求方法（如 MCP 2026-07-28 探测 `server/discover`、`resources/list`、
//!   `prompts/list` 等）**必须立即返回标准 JSON-RPC `-32601 Method not found`**，
//!   绝不静默丢弃，否则客户端会挂到超时取消。
//! - 通知消息（无 `id`）不回包。

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};
use smelt_core::background_tasks::{SMELT_PEER_SOCK_ENV, call_peer_socket};

const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

pub(crate) fn run() -> i32 {
    let Some(sock) = std::env::var_os(SMELT_PEER_SOCK_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        eprintln!("smeltd --peer-mcp: {SMELT_PEER_SOCK_ENV} is not set");
        return 2;
    };

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(response) = handle_jsonrpc_line(trimmed, |req| call_peer_socket(&sock, req))
            && (writeln!(out, "{response}").is_err() || out.flush().is_err())
        {
            break;
        }
    }
    0
}

pub(crate) fn handle_jsonrpc_line(
    line: &str,
    call_socket: impl Fn(&Value) -> Result<Value, String>,
) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": {
                    "code": -32700,
                    "message": format!("Parse error: {error}"),
                }
            }));
        }
    };
    let id = message.get("id")?.clone();
    if id.is_null() && message.get("method").is_none() {
        return None;
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(error_response(
            id,
            -32600,
            "Invalid Request: missing method",
        ));
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            let _ = call_socket(&json!({"op": "mcp_hello"}));
            let protocol_version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            Some(ok_response(
                id,
                json!({
                    "protocolVersion": protocol_version,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "smelt-peers",
                        "version": smelt_version::VERSION,
                    }
                }),
            ))
        }
        "ping" => Some(ok_response(id, json!({}))),
        "tools/list" => Some(ok_response(id, json!({ "tools": tool_definitions() }))),
        "tools/call" => Some(handle_tool_call(id, &params, call_socket)),
        other => Some(error_response(
            id,
            -32601,
            &format!("Method not found: {other}"),
        )),
    }
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

fn mcp_tool_call_id(id: &Value) -> String {
    match id {
        Value::String(raw) if !raw.trim().is_empty() => format!("mcp-id-{}", raw.trim()),
        Value::Number(num) => format!("mcp-id-{num}"),
        _ => format!("mcp-{}", uuid::Uuid::new_v4().simple()),
    }
}

fn handle_tool_call(
    id: Value,
    params: &Value,
    call_socket: impl Fn(&Value) -> Result<Value, String>,
) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return error_response(id, -32602, "tools/call requires params.name");
    };
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let request = match name {
        "session_list" => {
            let mut req = serde_json::Map::new();
            req.insert("op".into(), "session_list".into());
            if let Some(query) = args.get("query") {
                req.insert("query".into(), query.clone());
            }
            if let Some(include_agents) = args.get("includeAgents") {
                req.insert("includeAgents".into(), include_agents.clone());
            }
            Value::Object(req)
        }
        "session_read" => {
            let mut req = serde_json::Map::new();
            req.insert("op".into(), "session_read".into());
            if let Some(session_id) = args.get("sessionId") {
                req.insert("sessionId".into(), session_id.clone());
            }
            if let Some(limit) = args.get("limit") {
                req.insert("limit".into(), limit.clone());
            }
            Value::Object(req)
        }
        "session_send" => {
            let mut req = serde_json::Map::new();
            req.insert("op".into(), "session_send".into());
            req.insert("toolCallId".into(), mcp_tool_call_id(&id).into());
            for key in ["to", "text", "kind", "replyTo"] {
                if let Some(value) = args.get(key) {
                    req.insert(key.into(), value.clone());
                }
            }
            Value::Object(req)
        }
        "session_spawn" => {
            let mut req = serde_json::Map::new();
            req.insert("op".into(), "session_spawn".into());
            req.insert("toolCallId".into(), mcp_tool_call_id(&id).into());
            for key in ["title", "task", "agent", "cwd"] {
                if let Some(value) = args.get(key) {
                    req.insert(key.into(), value.clone());
                }
            }
            Value::Object(req)
        }
        other => {
            return error_response(id, -32602, &format!("Unknown tool: {other}"));
        }
    };

    match call_socket(&request) {
        Ok(reply) => {
            let is_error = reply.get("ok").and_then(Value::as_bool) != Some(true);
            let text = reply.to_string();
            ok_response(
                id,
                json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": is_error,
                }),
            )
        }
        Err(error) => {
            let reply = json!({ "ok": false, "error": error });
            ok_response(
                id,
                json!({
                    "content": [{ "type": "text", "text": reply.to_string() }],
                    "isError": true,
                }),
            )
        }
    }
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "session_list",
            "description": "List other Smelt conversations (Pi, Codex, Claude Code, Antigravity, custom agents, and remote machines) you can message, plus available agent options for session_spawn.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional substring filter on conversation title or id."
                    },
                    "includeAgents": {
                        "type": "boolean",
                        "description": "Include availableAgents (engines, product agents, custom profiles) for session_spawn. Defaults to true."
                    }
                }
            }
        },
        {
            "name": "session_read",
            "description": "Read the latest assistant replies from another Smelt conversation without waking or interrupting it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sessionId": {
                        "type": "string",
                        "description": "Target conversation id (e.g. acp-… or <runtime_id>/acp-…)."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 5,
                        "description": "Number of recent assistant replies to read (default 1, max 5)."
                    }
                },
                "required": ["sessionId"]
            }
        },
        {
            "name": "session_send",
            "description": "Send a message, task, or completion result to another Smelt conversation. Returns immediately once accepted; replies arrive asynchronously as <peer-message>.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "Target conversation id from session_list."
                    },
                    "text": {
                        "type": "string",
                        "description": "Message body. Be explicit about what work is requested or what result is being returned."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["message", "task", "completion"],
                        "description": "message (default), task (delegate work), or completion (return result for a task)."
                    },
                    "replyTo": {
                        "type": "string",
                        "description": "Message id (pm-…) being answered."
                    }
                },
                "required": ["to", "text"]
            }
        },
        {
            "name": "session_spawn",
            "description": "Start a new Smelt conversation in the sidebar and hand it a task. Optionally choose any available agent (e.g. codex, claude, antigravity, agent:<id>, profile:<id>) from session_list.availableAgents.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Short sidebar title for the new conversation."
                    },
                    "task": {
                        "type": "string",
                        "description": "Self-contained task prompt to hand to the new conversation."
                    },
                    "agent": {
                        "type": "string",
                        "description": "Optional agent option id from session_list.availableAgents (e.g. codex, claude, agent:<id>, profile:<id>). Defaults to the same agent as this conversation."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory inside an open workspace project. Defaults to this conversation's working directory."
                    }
                },
                "required": ["title", "task"]
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn unknown_method_like_agy_server_discover_returns_method_not_found() {
        let resp = handle_jsonrpc_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{}}"#,
            |_| Ok(json!({"ok": true})),
        )
        .expect("requests with id must always receive a response");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn notifications_without_id_produce_no_response() {
        assert!(
            handle_jsonrpc_line(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
                |_| Ok(json!({"ok": true})),
            )
            .is_none()
        );
    }

    #[test]
    fn initialize_sends_mcp_hello_and_tools_call_forwards_to_peer_socket() {
        let seen = RefCell::new(Vec::<Value>::new());
        let init = handle_jsonrpc_line(
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
            |req| {
                seen.borrow_mut().push(req.clone());
                Ok(json!({"ok": true}))
            },
        )
        .expect("initialize response");
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(seen.borrow()[0]["op"], "mcp_hello");

        let list = handle_jsonrpc_line(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#,
            |_| Ok(json!({"ok": true})),
        )
        .expect("tools/list response");
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(
            names,
            [
                "session_list",
                "session_read",
                "session_send",
                "session_spawn"
            ]
        );

        let sent = handle_jsonrpc_line(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"session_spawn","arguments":{"title":"Codex 实现","task":"写单测","agent":"codex"}}}"#,
            |req| {
                seen.borrow_mut().push(req.clone());
                Ok(json!({
                    "ok": true,
                    "status": "accepted",
                    "id": "pm-1234567890ab",
                    "to": {"id": "acp-new", "title": "Codex 实现"}
                }))
            },
        )
        .expect("tools/call response");
        assert_eq!(sent["result"]["isError"], false);
        assert!(sent["result"].get("structuredContent").is_none());
        let text_payload: Value =
            serde_json::from_str(sent["result"]["content"][0]["text"].as_str().unwrap())
                .expect("JSON text payload");
        assert_eq!(text_payload["to"]["id"], "acp-new");
        let forwarded = &seen.borrow()[1];
        assert_eq!(forwarded["op"], "session_spawn");
        assert_eq!(forwarded["agent"], "codex");
        assert_eq!(forwarded["toolCallId"], "mcp-id-4");
    }
}
