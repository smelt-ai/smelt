//! Smelt peer messaging 的 MCP STDIO 适配器。
//!
//! 本进程不保存路由状态；工具调用逐次转发给 smeltd。调用者身份只取 Smelt 在启动
//! 会话时注入的环境变量，不允许模型在 tool arguments 里伪造 source session。

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use smelt_core::control_api::{
    AgentList, AgentListParams, AgentSend, AgentSendParams, ControlClientError,
};
use smelt_plugin_api::CommandId;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

const SMELT_LIST_AGENTS: &str = "smelt_list_agents";
const SMELT_SEND_MESSAGE: &str = "smelt_send_message";

fn socket_path() -> PathBuf {
    std::env::var_os("SMELT_SOCK")
        .map(PathBuf::from)
        .unwrap_or_else(smelt_core::daemon_state::smeltd_sock_path)
}

fn required_env(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            format!("{name} is not set; this MCP server must run inside a Smelt session")
        })
}

fn source_identity() -> Result<(String, String), String> {
    let source = required_env("SMELT_SESSION_ID")?;
    let token = required_env("SMELT_AGENT_TOKEN")?;
    Ok((source, token))
}

fn control_error_value(error: ControlClientError) -> Result<Value, String> {
    match error {
        ControlClientError::Protocol(error) => Ok(json!({
            "ok": false,
            "err": error.message,
            "code": error.code,
        })),
        error => Err(error.to_string()),
    }
}

fn daemon_list() -> Result<Value, String> {
    let socket_path = socket_path();
    let (source, token) = source_identity()?;
    daemon_list_at(&socket_path, &source, &token)
}

fn daemon_list_at(socket_path: &Path, source: &str, token: &str) -> Result<Value, String> {
    match smelt_core::control_client::call_at::<AgentList>(
        socket_path,
        Duration::from_secs(5),
        &AgentListParams {
            source_session_id: source.to_string(),
            source_token: token.to_string(),
        },
    ) {
        Ok(result) => Ok(json!({ "ok": true, "agents": result.agents })),
        Err(error) => control_error_value(error),
    }
}

fn daemon_send(command_id: &CommandId, target: &str, message: &str) -> Result<Value, String> {
    let socket_path = socket_path();
    let (source, token) = source_identity()?;
    daemon_send_at(&socket_path, &source, &token, command_id, target, message)
}

fn daemon_send_at(
    socket_path: &Path,
    source: &str,
    token: &str,
    command_id: &CommandId,
    target: &str,
    message: &str,
) -> Result<Value, String> {
    let params = AgentSendParams {
        source_session_id: source.to_string(),
        source_token: token.to_string(),
        command_id: command_id.clone(),
        target: target.to_string(),
        message: message.to_string(),
    };
    for attempt in 0..2 {
        match smelt_core::control_client::call_at::<AgentSend>(
            socket_path,
            Duration::from_secs(10),
            &params,
        ) {
            Ok(result) => {
                return Ok(json!({
                    "ok": true,
                    "message_id": result.message_id,
                    "target_session_id": result.target_session_id,
                    "command_id": command_id,
                }));
            }
            Err(ControlClientError::Transport(_) | ControlClientError::InvalidResponse(_))
                if attempt == 0 => {}
            Err(
                error @ (ControlClientError::Transport(_) | ControlClientError::InvalidResponse(_)),
            ) => {
                return Ok(json!({
                    "ok": false,
                    "code": "result_unknown",
                    "err": format!(
                        "smeltd did not return a valid result after an idempotent retry: {error}"
                    ),
                    "command_id": command_id,
                }));
            }
            Err(error) => {
                return control_error_value(error)
                    .map(|value| attach_command_id(value, command_id.as_str()));
            }
        }
    }
    unreachable!("the bounded retry loop always returns")
}

fn tools() -> Value {
    json!([
        {
            "name": SMELT_LIST_AGENTS,
            "description": "List active Smelt agent sessions that can receive cross-agent messages. Use the returned session_id as the unambiguous target.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": SMELT_SEND_MESSAGE,
            "description": "Deliver a message to another Smelt agent. The call confirms delivery only. When the peer replies with this same tool, its response arrives later in this conversation as a new peer message.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Target session_id, or provider:claude/provider:codex when exactly one matching session is available." },
                    "message": { "type": "string", "description": "Message to deliver." },
                    "command_id": { "type": "string", "minLength": 1, "description": "Optional stable idempotency key. Reuse this value to recover a result_unknown outcome without redelivering." }
                },
                "required": ["target", "message"],
                "additionalProperties": false
            }
        }
    ])
}

fn required_argument<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    arguments[key]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing or empty `{key}`"))
}

fn call_tool(
    name: &str,
    arguments: &Value,
    command_id: Option<&CommandId>,
) -> Result<Value, String> {
    match name {
        SMELT_LIST_AGENTS => daemon_list(),
        SMELT_SEND_MESSAGE => daemon_send(
            command_id.ok_or_else(|| "missing send command id".to_string())?,
            required_argument(arguments, "target")?,
            required_argument(arguments, "message")?,
        ),
        _ => Err(format!("unknown tool `{name}`")),
    }
}

fn attach_command_id(mut value: Value, command_id: &str) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "command_id".to_string(),
            Value::String(command_id.to_string()),
        );
        value
    } else {
        json!({
            "ok": false,
            "err": "smeltd returned a non-object result",
            "command_id": command_id,
        })
    }
}

fn tool_result(result: Result<Value, String>, command_id: Option<&str>) -> Value {
    let value = match result {
        Ok(value) => match command_id {
            Some(command_id) => attach_command_id(value, command_id),
            None => value,
        },
        Err(error) => {
            let mut value = json!({ "ok": false, "err": error });
            if let Some(command_id) = command_id {
                value = attach_command_id(value, command_id);
            }
            value
        }
    };
    let is_error = value["ok"].as_bool() == Some(false);
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        }],
        "structuredContent": value,
        "isError": is_error,
    })
}

fn send_command_id(arguments: &Value) -> (String, Result<CommandId, String>) {
    let value = arguments.get("command_id");
    let command_id = match value {
        None => format!("agent-send-{}", uuid::Uuid::new_v4().simple()),
        Some(Value::String(command_id)) => command_id.clone(),
        Some(value) => {
            let displayed = value.to_string();
            return (
                displayed.clone(),
                Err(format!(
                    "invalid `command_id` {displayed}: expected a string"
                )),
            );
        }
    };
    let parsed = CommandId::new(command_id.clone())
        .map_err(|error| format!("invalid `command_id` {command_id:?}: {error}"));
    (command_id, parsed)
}

fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn handle_request(request: &Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request["method"].as_str()?;
    id.as_ref()?;
    let id = id.unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": {
                "name": "smelt-agent-bus",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "Use smelt_list_agents before cross-agent messaging. Use smelt_send_message for every outbound peer message. An incoming peer message includes source_session; reply to it with smelt_send_message targeting that session when a response is needed. Do not create acknowledgement loops."
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let Some(name) = request["params"]["name"].as_str() else {
                return Some(error_response(id, -32602, "missing tool name"));
            };
            let arguments = request["params"]
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if name == SMELT_SEND_MESSAGE {
                let (command_id_text, command_id) = send_command_id(&arguments);
                let result = command_id
                    .and_then(|command_id| call_tool(name, &arguments, Some(&command_id)));
                tool_result(result, Some(&command_id_text))
            } else {
                tool_result(call_tool(name, &arguments, None), None)
            }
        }
        _ => {
            return Some(error_response(
                id,
                -32601,
                format!("unknown method `{method}`"),
            ));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => handle_request(&request),
            Err(error) => Some(error_response(Value::Null, -32700, error.to_string())),
        };
        if let Some(response) = response
            && (writeln!(stdout, "{response}").is_err() || stdout.flush().is_err())
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::thread;

    /// Unix socket 路径必须短于 `SUN_LEN`（macOS 上是 104 字节），而这个上限
    /// 算的是完整路径。放在 `target/` 下再拼一个完整 UUID，只要仓库本身的路径
    /// 稍深就会超限，bind 直接失败——所以落在临时目录，名字也只取 UUID 前段。
    fn test_socket_path() -> PathBuf {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        std::env::temp_dir().join(format!("smcp-{}.sock", &nonce[..12]))
    }

    fn read_request(stream: &UnixStream) -> Value {
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn initialize_advertises_tools() {
        let response = handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" }
        }))
        .unwrap();
        assert_eq!(response["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert!(response["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_has_two_tools() {
        let response = handle_request(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list"
        }))
        .unwrap();
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 2);
        let names: Vec<_> = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert_eq!(names, ["smelt_list_agents", "smelt_send_message"]);
        let send = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == SMELT_SEND_MESSAGE)
            .unwrap();
        assert!(send["inputSchema"]["properties"]["command_id"].is_object());
        assert_eq!(
            send["inputSchema"]["required"],
            json!(["target", "message"])
        );
    }

    #[test]
    fn send_tool_reuses_supplied_command_id_in_structured_argument_errors() {
        let response = handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": SMELT_SEND_MESSAGE,
                "arguments": {
                    "command_id": "agent-send-user-supplied",
                    "message": "hello"
                }
            }
        }))
        .unwrap();
        let result = &response["result"];
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["command_id"],
            "agent-send-user-supplied"
        );
        assert!(
            result["structuredContent"]["err"]
                .as_str()
                .unwrap()
                .contains("target")
        );
    }

    #[test]
    fn send_tool_generates_command_id_before_validating_arguments() {
        let response = handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": SMELT_SEND_MESSAGE,
                "arguments": {}
            }
        }))
        .unwrap();
        let command_id = response["result"]["structuredContent"]["command_id"]
            .as_str()
            .unwrap();
        assert!(command_id.starts_with("agent-send-"));
        assert_eq!(response["result"]["isError"], true);
    }

    #[test]
    fn notifications_do_not_get_responses() {
        assert!(
            handle_request(&json!({
                "jsonrpc": "2.0", "method": "notifications/initialized"
            }))
            .is_none()
        );
    }

    #[test]
    fn control_domain_error_has_tool_error_shape() {
        let value = control_error_value(ControlClientError::Protocol(
            smelt_core::control_api::ControlError::new(
                smelt_core::control_api::ControlErrorCode::OperationFailed,
                "target is busy",
            ),
        ))
        .unwrap();

        assert_eq!(value["ok"], false);
        assert_eq!(value["code"], "operation_failed");
        assert_eq!(value["err"], "target is busy");
    }

    #[test]
    fn list_uses_the_control_api() {
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = thread::spawn(move || {
            let (mut request, _) = listener.accept().unwrap();
            let value = read_request(&request);
            assert_eq!(value["method"], "agent.list");
            assert_eq!(value["params"]["source_session_id"], "source");
            assert_eq!(value["params"]["source_token"], "token");
            let request_id = value["request_id"].as_str().unwrap();
            let response = smelt_core::control_api::ControlResponse::ok(
                request_id,
                smelt_core::control_api::AgentListResult { agents: Vec::new() },
            );
            writeln!(request, "{}", serde_json::to_value(response).unwrap()).unwrap();
        });

        let result = daemon_list_at(&socket_path, "source", "token").unwrap();

        assert_eq!(result["ok"], true);
        server.join().unwrap();
        std::fs::remove_file(socket_path).unwrap();
    }

    #[test]
    fn unknown_control_result_retries_once_with_the_same_command_id() {
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = thread::spawn(move || {
            let (first, _) = listener.accept().unwrap();
            let first_request = read_request(&first);
            assert_eq!(first_request["method"], "agent.send");
            drop(first);

            let (mut retry, _) = listener.accept().unwrap();
            let retry_request = read_request(&retry);
            assert_eq!(
                retry_request["params"]["command_id"],
                first_request["params"]["command_id"]
            );
            let request_id = retry_request["request_id"].as_str().unwrap();
            let response = smelt_core::control_api::ControlResponse::ok(
                request_id,
                smelt_core::control_api::AgentSendResult {
                    message_id: retry_request["params"]["command_id"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                    target_session_id: "target".to_string(),
                },
            );
            writeln!(retry, "{}", serde_json::to_value(response).unwrap()).unwrap();
        });

        let command_id = CommandId::new("agent-send-stable").unwrap();
        let result = daemon_send_at(
            &socket_path,
            "source",
            "token",
            &command_id,
            "target",
            "hello",
        )
        .unwrap();

        assert_eq!(result["message_id"], command_id.as_str());
        assert_eq!(result["command_id"], command_id.as_str());
        server.join().unwrap();
        std::fs::remove_file(socket_path).unwrap();
    }

    #[test]
    fn result_unknown_keeps_the_command_id_for_explicit_recovery() {
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (request, _) = listener.accept().unwrap();
                drop(request);
            }
        });

        let command_id = CommandId::new("agent-send-recover-me").unwrap();
        let result = daemon_send_at(
            &socket_path,
            "source",
            "token",
            &command_id,
            "target",
            "hello",
        )
        .unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "result_unknown");
        assert_eq!(result["command_id"], command_id.as_str());
        server.join().unwrap();
        std::fs::remove_file(socket_path).unwrap();
    }
}
