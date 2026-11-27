//! `smeltd --peer-mcp`：内置极简 stdio MCP Server，供第三方 ACP 智能体跨对话协作。
//!
//! 只做两件事：
//! 1. 实现标准 MCP stdio JSON-RPC 2.0 握手与 `tools/list`、`tools/call`；
//! 2. 把调用原样经 `SMELT_PEER_SOCK` 转发给该会话绑定的 `PeerSocket`（来源由 `smeltd`
//!    按套接字认定，工具参数里没有来源字段）。
//!
//! 工具定义和回给模型的文字和 Pi 扩展是同一份，见 [`crate::peer_tools`]。
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

use crate::peer_tools;

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
        "tools/list" => Some(ok_response(
            id,
            json!({ "tools": tool_definitions_for_socket(&call_socket) }),
        )),
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
    if !peer_tools::is_tool(name) {
        return error_response(id, -32602, &format!("Unknown tool: {name}"));
    }
    let mut request = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    request.insert("op".into(), name.into());
    request.insert("toolCallId".into(), mcp_tool_call_id(&id).into());
    let reply = call_socket(&Value::Object(request.clone())).unwrap_or_else(|error| {
        // 没连上应答函数：照它没办成的样子说。
        peer_tools::present(
            name,
            &Value::Object(request),
            json!({"ok": false, "error": error}),
        )
    });
    let is_error = reply.get("ok").and_then(Value::as_bool) != Some(true);
    let text = reply
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut content = vec![json!({ "type": "text", "text": text })];
    // 界面从工具结果里认发给了谁、消息 id（`apply::maybe_attach_acp_peer_send`）。
    // 各家 ACP 转发 `structuredContent` 不一，放在第二段文本里最稳。
    if matches!(name, "session_send" | "session_spawn")
        && let Some(details) = reply.get("details")
    {
        content.push(json!({ "type": "text", "text": details.to_string() }));
    }
    ok_response(id, json!({ "content": content, "isError": is_error }))
}

/// 按 MCP `ToolAnnotations` 显式标出四个布尔提示，避免客户端按规范默认值
/// （`destructiveHint: true`、`openWorldHint: true`）把只读工具当成高风险操作弹窗审批。
fn tool_annotations(name: &str, title: &str) -> Value {
    let read_only = peer_tools::is_read_only(name);
    json!({
        "title": title,
        "readOnlyHint": read_only,
        "destructiveHint": false,
        "idempotentHint": read_only,
        "openWorldHint": false,
    })
}

/// MCP 没有 Pi 那样的「用法提示」位置：接在每个工具的说明后面。
fn tool_definitions_from(raw: &Value) -> Value {
    let tools = raw
        .as_array()
        .into_iter()
        .flatten()
        .map(|tool| {
            let name = tool["name"].as_str().unwrap_or_default();
            let title = tool["label"].as_str().unwrap_or(name);
            let mut description = tool["description"].as_str().unwrap_or_default().to_string();
            for line in tool["promptGuidelines"].as_array().into_iter().flatten() {
                if let Some(line) = line.as_str() {
                    description.push_str("\n- ");
                    description.push_str(line);
                }
            }
            json!({
                "name": name,
                "description": description,
                "inputSchema": tool["parameters"],
                "annotations": tool_annotations(name, title),
            })
        })
        .collect();
    Value::Array(tools)
}

fn tool_definitions_for_socket(call_socket: &impl Fn(&Value) -> Result<Value, String>) -> Value {
    if let Ok(reply) = call_socket(&json!({ "op": peer_tools::SESSION_TOOLS_OP }))
        && reply.get("ok").and_then(Value::as_bool) == Some(true)
        && let Some(tools) = reply.get("tools")
        && tools.is_array()
    {
        return tool_definitions_from(tools);
    }
    tool_definitions()
}

fn tool_definitions() -> Value {
    tool_definitions_from(&peer_tools::definitions())
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
                Ok(peer_tools::present(
                    "session_spawn",
                    req,
                    json!({
                        "ok": true,
                        "status": "accepted",
                        "id": "pm-1234567890ab",
                        "to": {"id": "acp-new", "title": "Codex 实现"}
                    }),
                ))
            },
        )
        .expect("tools/call response");
        assert_eq!(sent["result"]["isError"], false);
        assert!(sent["result"].get("structuredContent").is_none());
        // 先是写给模型的话，再是界面认的 details。
        let said = sent["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            said.contains("正在新建对话〈Codex 实现〉（acp-new）"),
            "{said}"
        );
        let details: Value =
            serde_json::from_str(sent["result"]["content"][1]["text"].as_str().unwrap())
                .expect("JSON details");
        assert_eq!(details["to"]["id"], "acp-new");
        assert_eq!(details["messageId"], "pm-1234567890ab");
        assert_eq!(details["spawned"], true);
        let forwarded = &seen.borrow()[1];
        assert_eq!(forwarded["op"], "session_spawn");
        assert_eq!(forwarded["agent"], "codex");
        assert_eq!(forwarded["toolCallId"], "mcp-id-4");
    }

    #[test]
    fn tools_list_uses_the_shared_definitions_with_guidelines_appended() {
        let tools = tool_definitions();
        let send = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "session_send")
            .unwrap();
        assert_eq!(send["inputSchema"]["required"], json!(["to", "text"]));
        let description = send["description"].as_str().unwrap();
        assert!(
            description.starts_with("给另一个 Smelt 对话"),
            "{description}"
        );
        assert!(
            description.contains("\n- When you finish a task"),
            "{description}"
        );
    }

    #[test]
    fn tools_list_marks_read_only_and_mutating_annotations() {
        let resp = handle_jsonrpc_line(
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/list","params":{}}"#,
            |_| Ok(json!({"ok": true})),
        )
        .expect("tools/list response");
        let tools = resp["result"]["tools"].as_array().unwrap();
        let by_name = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("missing tool {name}"))
        };

        for (name, title) in [("session_list", "列出对话"), ("session_read", "查看对话")] {
            assert_eq!(
                by_name(name)["annotations"],
                json!({
                    "title": title,
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false,
                }),
                "{name} should advertise read-only annotations"
            );
        }

        for (name, title) in [("session_send", "发给对话"), ("session_spawn", "新建对话")] {
            assert_eq!(
                by_name(name)["annotations"],
                json!({
                    "title": title,
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": false,
                    "openWorldHint": false,
                }),
                "{name} should advertise non-read-only annotations"
            );
        }
    }

    #[test]
    fn an_unreachable_socket_is_reported_as_a_refusal() {
        let reply = handle_jsonrpc_line(
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"session_send","arguments":{"to":"acp-b","text":"x"}}}"#,
            |_| Err("connection refused".to_string()),
        )
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], "connection refused");
        let details: Value =
            serde_json::from_str(reply["result"]["content"][1]["text"].as_str().unwrap()).unwrap();
        assert_eq!(details["to"]["id"], "acp-b");
    }

    #[test]
    fn tools_list_includes_group_tools_when_socket_advertises_them() {
        let resp = handle_jsonrpc_line(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}"#,
            |req| {
                assert_eq!(req["op"], peer_tools::SESSION_TOOLS_OP);
                Ok(json!({
                    "ok": true,
                    "tools": peer_tools::definitions_for(true),
                }))
            },
        )
        .expect("tools/list response");
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"group_post"));
        assert!(names.contains(&"group_yield"));
        assert!(names.contains(&"group_read"));
        assert!(names.contains(&"group_members"));
        assert!(names.contains(&"group_task_update"));
        let by_name = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(by_name("group_read")["annotations"]["readOnlyHint"], true);
        assert_eq!(
            by_name("group_members")["annotations"]["readOnlyHint"],
            true
        );
        assert_eq!(by_name("group_post")["annotations"]["readOnlyHint"], false);
        assert_eq!(by_name("group_yield")["annotations"]["readOnlyHint"], false);
        assert_eq!(
            by_name("group_task_update")["annotations"]["readOnlyHint"],
            false
        );
    }
}
