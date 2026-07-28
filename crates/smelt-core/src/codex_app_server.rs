//! Codex 原生 app-server driver。对外复用现有 `AcpHandle`/`AcpEvent` 边界，
//! 让 smeltd 和 GUI 无需理解 provider 协议；这里负责把 thread/turn/item JSONL
//! 翻译成 Smelt 的通用会话事件。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    ElicitationContentValue, SessionId, StopReason, ToolCallId,
};

use crate::acp_chat::{ToolCallStatus, ToolKind, ToolOutputPart};
use crate::acp_conn::{
    AcpCommand, AcpEvent, AcpHandle, AcpLaunch, ElicitField, ElicitFieldKind, ElicitOption,
    ElicitationResponder, ModelState, PermissionResponder, ReadyKind, SessionConfigState,
};
use crate::acp_session::{ApprovalDetailsView, PermissionOptionKindView, PermissionOptionView};

fn send(writer: &Arc<Mutex<std::process::ChildStdin>>, value: serde_json::Value) -> bool {
    let mut writer = writer.lock().unwrap();
    writeln!(writer, "{value}").is_ok() && writer.flush().is_ok()
}

fn command_parts(command: &str) -> Result<Vec<String>, String> {
    let parts: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    if parts.is_empty() {
        Err("Codex app-server 启动命令为空".into())
    } else {
        Ok(parts)
    }
}

fn resolve_program(program: &str) -> std::path::PathBuf {
    if program.contains('/') {
        return program.into();
    }
    std::env::split_paths(crate::login_env::login_path())
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
        .unwrap_or_else(|| program.into())
}

pub fn spawn_codex_app_server(launch: AcpLaunch, spawn_gate: Option<Arc<RwLock<()>>>) -> AcpHandle {
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (event_tx, event_rx) = smol::channel::unbounded();
    let thread_name = format!("smelt-codex-{}", &launch.sid[..launch.sid.len().min(10)]);

    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            if let Err(error) = run(launch, spawn_gate, cmd_rx, event_tx.clone()) {
                let _ = event_tx.try_send(AcpEvent::Fatal(error));
            }
        })
        .expect("spawn codex app-server driver");

    AcpHandle {
        cmd_tx,
        event_rx,
        // app-server 会话靠 thread/resume 恢复。升级时不裸传有状态 fd，旧进程
        // 会正常 Shutdown，新进程按持久化 thread id 重连，避免冒充 ACP handoff。
        stdio: Arc::new(Mutex::new(None)),
    }
}

fn run(
    launch: AcpLaunch,
    spawn_gate: Option<Arc<RwLock<()>>>,
    cmd_rx: smol::channel::Receiver<AcpCommand>,
    event_tx: smol::channel::Sender<AcpEvent>,
) -> Result<(), String> {
    let parts = command_parts(&launch.launch.command)?;
    let mut command = Command::new(resolve_program(&parts[0]));
    command.args(&parts[1..]);
    command.env("PATH", crate::login_env::login_path());
    command.envs(&launch.launch.env);
    if !launch.launch.env.contains_key("CODEX_HOME") {
        if let Some(codex_home) = crate::login_env::codex_home() {
            command.env("CODEX_HOME", codex_home);
        }
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let _permit = spawn_gate.as_ref().map(|gate| gate.read().unwrap());
    let mut child = command
        .spawn()
        .map_err(|e| format!("启动 Codex app-server 失败：{e}"))?;
    drop(_permit);

    let stdin = child.stdin.take().ok_or("Codex app-server 没有 stdin")?;
    let stdout = child.stdout.take().ok_or("Codex app-server 没有 stdout")?;
    let stderr = child.stderr.take().ok_or("Codex app-server 没有 stderr")?;
    let writer = Arc::new(Mutex::new(stdin));
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = line_tx.send(line);
        }
    });
    let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::default();
    let stderr_out = Arc::clone(&stderr_tail);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let mut tail = stderr_out.lock().unwrap();
            tail.push(line);
            if tail.len() > 40 {
                tail.remove(0);
            }
        }
    });

    send(
        &writer,
        serde_json::json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {"name":"smelt", "title":"Smelt", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi": true, "mcpServerOpenaiFormElicitation": true}
            }
        }),
    );
    wait_response(&line_rx, 1)?;
    send(
        &writer,
        serde_json::json!({"method":"initialized", "params":{}}),
    );

    let mut next_id = 2_i64;
    send(
        &writer,
        serde_json::json!({"id":next_id,"method":"model/list","params":{"limit":100}}),
    );
    let model_catalog = wait_response(&line_rx, next_id).unwrap_or_default();
    next_id += 1;
    let mut resumed = false;
    let thread_result = if let Some(session_id) = &launch.resume_session_id {
        send(
            &writer,
            serde_json::json!({"id":next_id,"method":"thread/resume","params":{"threadId":session_id.to_string()}}),
        );
        match wait_response(&line_rx, next_id) {
            Ok(value) => {
                resumed = true;
                value
            }
            Err(_) => {
                next_id += 1;
                send(
                    &writer,
                    serde_json::json!({"id":next_id,"method":"thread/start","params":{"cwd":launch.cwd}}),
                );
                wait_response(&line_rx, next_id)?
            }
        }
    } else {
        send(
            &writer,
            serde_json::json!({"id":next_id,"method":"thread/start","params":{"cwd":launch.cwd}}),
        );
        wait_response(&line_rx, next_id)?
    };
    let thread_id = thread_result
        .pointer("/result/thread/id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Codex app-server 未返回 thread id：{thread_result}"))?
        .to_string();
    let mut selected_model = thread_result
        .pointer("/result/model")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut selected_effort = thread_result
        .pointer("/result/reasoningEffort")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let _ = event_tx.try_send(AcpEvent::Ready {
        session_id: SessionId::new(thread_id.clone()),
        kind: if resumed {
            ReadyKind::ResumedKeepHistory
        } else {
            ReadyKind::Fresh
        },
        fallback_reason: None,
        supports_image: true,
    });
    publish_models(
        &model_catalog,
        selected_model.as_deref(),
        selected_effort.as_deref(),
        &event_tx,
    );

    let mut current_turn: Option<String> = None;
    let mut command_outputs = HashMap::<String, String>::new();
    loop {
        while let Ok(command) = cmd_rx.try_recv() {
            match command {
                AcpCommand::Prompt { text, images } => {
                    next_id += 1;
                    let mut input = vec![serde_json::json!({"type":"text","text":text})];
                    input.extend(images.into_iter().map(|image| {
                        serde_json::json!({"type":"image","url":format!("data:{};base64,{}", image.mime, image.data_b64)})
                    }));
                    let mut params = serde_json::json!({"threadId":thread_id,"input":input});
                    if let Some(model) = &selected_model {
                        params["model"] = serde_json::Value::String(model.clone());
                    }
                    if let Some(effort) = &selected_effort {
                        params["effort"] = serde_json::Value::String(effort.clone());
                    }
                    send(
                        &writer,
                        serde_json::json!({
                            "id":next_id,"method":"turn/start","params":params
                        }),
                    );
                }
                AcpCommand::Cancel => {
                    if let Some(turn_id) = &current_turn {
                        next_id += 1;
                        send(
                            &writer,
                            serde_json::json!({
                                "id":next_id,"method":"turn/interrupt","params":{"threadId":thread_id,"turnId":turn_id}
                            }),
                        );
                    }
                }
                AcpCommand::SetConfigOption {
                    config_id,
                    value_id,
                } => {
                    match config_id.as_str() {
                        "model" => {
                            selected_model = Some(value_id);
                            selected_effort = None;
                        }
                        "reasoning_effort" => selected_effort = Some(value_id),
                        _ => continue,
                    }
                    publish_models(
                        &model_catalog,
                        selected_model.as_deref(),
                        selected_effort.as_deref(),
                        &event_tx,
                    );
                }
                AcpCommand::Shutdown => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
            }
        }
        if cmd_rx.is_closed() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }

        match line_rx.recv_timeout(Duration::from_millis(30)) {
            Ok(line) => {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                handle_message(
                    value,
                    &writer,
                    &event_tx,
                    &thread_id,
                    &mut current_turn,
                    &mut command_outputs,
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let tail = stderr_tail.lock().unwrap().join("\n");
                return Err(if tail.is_empty() {
                    "Codex app-server 连接已关闭".into()
                } else {
                    format!("Codex app-server 连接已关闭\n--- stderr ---\n{tail}")
                });
            }
        }
    }
}

fn wait_response(
    lines: &std::sync::mpsc::Receiver<String>,
    id: i64,
) -> Result<serde_json::Value, String> {
    loop {
        let line = lines
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| format!("等待 Codex app-server 响应 {id} 超时"))?;
        let value: serde_json::Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if value.get("id").and_then(|v| v.as_i64()) == Some(id) {
            if value.get("error").is_some() {
                return Err(value.to_string());
            }
            return Ok(value);
        }
    }
}

fn publish_models(
    response: &serde_json::Value,
    selected_model: Option<&str>,
    selected_effort: Option<&str>,
    event_tx: &smol::channel::Sender<AcpEvent>,
) {
    let models = response
        .pointer("/result/data")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let visible: Vec<&serde_json::Value> = models
        .iter()
        .filter(|model| {
            !model
                .get("hidden")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .collect();
    let selected = selected_model
        .and_then(|id| {
            visible
                .iter()
                .copied()
                .find(|model| model.get("model").and_then(|v| v.as_str()) == Some(id))
        })
        .or_else(|| {
            visible
                .iter()
                .copied()
                .find(|model| model.get("isDefault").and_then(|v| v.as_bool()) == Some(true))
        })
        .or_else(|| visible.first().copied());
    let Some(selected) = selected else { return };
    let selected_id = selected
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let selected_name = selected
        .get("displayName")
        .and_then(|v| v.as_str())
        .unwrap_or(selected_id);
    let _ = event_tx.try_send(AcpEvent::Model(ModelState {
        config_id: "model".into(),
        current_name: selected_name.into(),
        options: visible
            .iter()
            .filter_map(|model| {
                Some((
                    model.get("model")?.as_str()?.to_string(),
                    model.get("displayName")?.as_str()?.to_string(),
                ))
            })
            .collect(),
    }));

    let efforts = selected
        .get("supportedReasoningEfforts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if efforts.is_empty() {
        let _ = event_tx.try_send(AcpEvent::ConfigOptions(Vec::new()));
        return;
    }
    let current = selected_effort
        .or_else(|| {
            selected
                .get("defaultReasoningEffort")
                .and_then(|v| v.as_str())
        })
        .unwrap_or("medium");
    fn effort_label(effort: &str) -> &str {
        match effort {
            "none" => "无",
            "minimal" => "极低",
            "low" => "低",
            "medium" => "中",
            "high" => "高",
            "xhigh" => "极高",
            other => other,
        }
    }
    let _ = event_tx.try_send(AcpEvent::ConfigOptions(vec![SessionConfigState {
        config_id: "reasoning_effort".into(),
        name: "推理强度".into(),
        description: Some("Codex 模型的推理强度".into()),
        current_name: effort_label(current).into(),
        options: efforts
            .iter()
            .filter_map(|option| option.get("reasoningEffort").and_then(|v| v.as_str()))
            .map(|effort| (effort.into(), effort_label(effort).into()))
            .collect(),
    }]));
}

fn handle_message(
    value: serde_json::Value,
    writer: &Arc<Mutex<std::process::ChildStdin>>,
    event_tx: &smol::channel::Sender<AcpEvent>,
    thread_id: &str,
    current_turn: &mut Option<String>,
    command_outputs: &mut HashMap<String, String>,
) {
    let Some(method) = value.get("method").and_then(|v| v.as_str()) else {
        return;
    };
    let params = value.get("params").cloned().unwrap_or_default();
    match method {
        "turn/started" => {
            *current_turn = params
                .pointer("/turn/id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                let _ = event_tx.try_send(AcpEvent::AgentChunk {
                    thought: false,
                    text: delta.into(),
                });
            }
        }
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
            if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                let _ = event_tx.try_send(AcpEvent::AgentChunk {
                    thought: true,
                    text: delta.into(),
                });
            }
        }
        "item/started" => {
            if let Some(item) = params.get("item") {
                if let Some((id, title, kind)) = tool_started(item) {
                    let _ = event_tx.try_send(AcpEvent::ToolStarted { id, title, kind });
                }
            }
        }
        "item/commandExecution/outputDelta" => {
            if params.get("threadId").and_then(|v| v.as_str()) == Some(thread_id)
                && let (Some(item_id), Some(delta)) = (
                    params.get("itemId").and_then(|v| v.as_str()),
                    params.get("delta").and_then(|v| v.as_str()),
                )
            {
                command_outputs
                    .entry(item_id.to_string())
                    .or_default()
                    .push_str(delta);
                let _ = event_tx.try_send(AcpEvent::ToolOutputDelta {
                    id: item_id.to_string(),
                    delta: delta.to_string(),
                });
            }
        }
        "item/completed" => {
            if let Some(item) = params.get("item") {
                let streamed_output = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .and_then(|id| command_outputs.remove(id));
                if let Some((id, status, output)) = tool_finished(item, streamed_output) {
                    let _ = event_tx.try_send(AcpEvent::ToolFinished { id, status, output });
                }
            }
        }
        "turn/plan/updated" => {
            let entries: Vec<serde_json::Value> = params
                .get("plan")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .map(|step| {
                    serde_json::json!({
                        "content": step.get("step").and_then(|v| v.as_str()).unwrap_or_default(),
                        "status": match step.get("status").and_then(|v| v.as_str()) {
                            Some("inProgress") => "in_progress",
                            Some("completed") => "completed",
                            _ => "pending",
                        }
                    })
                })
                .collect();
            if let Ok(plan) = serde_json::from_value(serde_json::json!({"entries":entries})) {
                let _ = event_tx.try_send(AcpEvent::Plan(plan));
            }
        }
        "thread/tokenUsage/updated" => {
            let usage = params.get("tokenUsage").cloned().unwrap_or_default();
            let used = usage
                .pointer("/total/totalTokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let size = usage
                .get("modelContextWindow")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cached_read = usage
                .pointer("/last/cachedInputTokens")
                .and_then(|v| v.as_u64());
            let _ = event_tx.try_send(AcpEvent::Usage {
                used,
                size,
                cached_read,
            });
        }
        "thread/status/changed" => {
            if params.pointer("/status/type").and_then(|v| v.as_str()) == Some("systemError") {
                let _ = event_tx.try_send(AcpEvent::Status("Codex thread 进入系统错误状态".into()));
            }
        }
        "thread/closed" => {
            let _ = event_tx.try_send(AcpEvent::Fatal("Codex thread 已关闭".into()));
        }
        "turn/completed" => {
            *current_turn = None;
            let status = params.pointer("/turn/status").and_then(|v| v.as_str());
            if status == Some("failed") {
                let message = params
                    .pointer("/turn/error/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Codex turn 执行失败");
                let _ = event_tx.try_send(AcpEvent::Status(message.into()));
            }
            let reason = if status == Some("interrupted") {
                serde_json::from_value(serde_json::json!("cancelled"))
                    .unwrap_or(StopReason::EndTurn)
            } else {
                StopReason::EndTurn
            };
            let _ = event_tx.try_send(AcpEvent::TurnEnded(reason));
        }
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            approval(value, writer, event_tx, thread_id);
        }
        "item/permissions/requestApproval" => {
            permissions_approval(value, writer, event_tx, thread_id);
        }
        "tool/requestUserInput" => {
            request_user_input(value, writer, event_tx, thread_id);
        }
        "mcpServer/elicitation/request" => {
            mcp_elicitation(value, writer, event_tx, thread_id);
        }
        "error" => {
            let message = params
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .unwrap_or("Codex turn 失败");
            let _ = event_tx.try_send(AcpEvent::AgentChunk {
                thought: false,
                text: format!("\n错误：{message}"),
            });
        }
        _ => {}
    }
}

fn request_user_input(
    value: serde_json::Value,
    writer: &Arc<Mutex<std::process::ChildStdin>>,
    event_tx: &smol::channel::Sender<AcpEvent>,
    expected_thread: &str,
) {
    let Some(request_id) = value.get("id").cloned() else {
        return;
    };
    let params = value.get("params").cloned().unwrap_or_default();
    if params.get("threadId").and_then(|v| v.as_str()) != Some(expected_thread) {
        return;
    }
    let questions = params
        .get("questions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut fields = Vec::new();
    for question in questions {
        let Some(key) = question.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let title = question
            .get("question")
            .and_then(|v| v.as_str())
            .or_else(|| question.get("header").and_then(|v| v.as_str()))
            .unwrap_or(key);
        let options: Vec<ElicitOption> = question
            .get("options")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|option| {
                let label = option.get("label")?.as_str()?.to_string();
                Some(ElicitOption {
                    value: ElicitationContentValue::String(label.clone()),
                    label,
                })
            })
            .collect();
        fields.push(ElicitField {
            key: key.into(),
            title: title.into(),
            kind: if options.is_empty() {
                ElicitFieldKind::Text {
                    secret: question
                        .get("isSecret")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                }
            } else {
                ElicitFieldKind::Select(options)
            },
        });
    }
    if fields.is_empty() {
        send(
            writer,
            serde_json::json!({"id":request_id,"result":{"answers":{}}}),
        );
        return;
    }
    let response_writer = Arc::clone(writer);
    let responder = ElicitationResponder::external(move |content| {
        let answers = content
            .unwrap_or_default()
            .into_iter()
            .map(|(key, value)| {
                let values = match value {
                    ElicitationContentValue::String(value) => vec![value],
                    ElicitationContentValue::StringArray(values) => values,
                    ElicitationContentValue::Boolean(value) => vec![value.to_string()],
                    _ => Vec::new(),
                };
                (key, serde_json::json!({"answers":values}))
            })
            .collect::<serde_json::Map<_, _>>();
        send(
            &response_writer,
            serde_json::json!({"id":request_id,"result":{"answers":answers}}),
        );
    });
    let message = params
        .get("questions")
        .and_then(|v| v.as_array())
        .and_then(|v| v.first())
        .and_then(|v| v.get("header"))
        .and_then(|v| v.as_str())
        .unwrap_or("Codex 需要补充信息")
        .to_string();
    let _ = event_tx.try_send(AcpEvent::Elicitation {
        message,
        fields,
        responder,
        raw_request_line: None,
    });
}

fn mcp_elicitation(
    value: serde_json::Value,
    writer: &Arc<Mutex<std::process::ChildStdin>>,
    event_tx: &smol::channel::Sender<AcpEvent>,
    expected_thread: &str,
) {
    let Some(request_id) = value.get("id").cloned() else {
        return;
    };
    let params = value.get("params").cloned().unwrap_or_default();
    if params.get("threadId").and_then(|v| v.as_str()) != Some(expected_thread) {
        return;
    }
    let is_url = params.get("mode").and_then(|v| v.as_str()) == Some("url");
    let fields = if is_url {
        params.get("url").and_then(|v| v.as_str()).map(|url| {
            vec![ElicitField {
                key: "url".into(),
                title: "在浏览器中完成".into(),
                kind: ElicitFieldKind::ExternalUrl(url.into()),
            }]
        })
    } else if matches!(
        params.get("mode").and_then(|v| v.as_str()),
        Some("form" | "openai/form")
    ) {
        parse_json_elicitation_fields(params.get("requestedSchema"))
    } else {
        None
    };
    let Some(fields) = fields else {
        send(
            writer,
            serde_json::json!({"id":request_id,"result":{"action":"decline"}}),
        );
        return;
    };
    let response_writer = Arc::clone(writer);
    let responder = ElicitationResponder::external(move |content| {
        let Some(content) = content else {
            send(
                &response_writer,
                serde_json::json!({"id":request_id,"result":{"action":"cancel"}}),
            );
            return;
        };
        if is_url {
            send(
                &response_writer,
                serde_json::json!({"id":request_id,"result":{"action":"accept"}}),
            );
            return;
        }
        let content = content
            .into_iter()
            .map(|(key, value)| {
                let value = match value {
                    ElicitationContentValue::String(value) => serde_json::Value::String(value),
                    ElicitationContentValue::StringArray(values) => serde_json::json!(values),
                    ElicitationContentValue::Boolean(value) => serde_json::Value::Bool(value),
                    _ => serde_json::Value::Null,
                };
                (key, value)
            })
            .collect::<serde_json::Map<_, _>>();
        send(
            &response_writer,
            serde_json::json!({"id":request_id,"result":{"action":"accept","content":content}}),
        );
    });
    let _ = event_tx.try_send(AcpEvent::Elicitation {
        message: params
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("MCP 需要补充信息")
            .into(),
        fields,
        responder,
        raw_request_line: None,
    });
}

fn parse_json_elicitation_fields(schema: Option<&serde_json::Value>) -> Option<Vec<ElicitField>> {
    let schema = schema?;
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    let properties = schema.get("properties")?.as_object()?;
    let mut fields = Vec::new();
    for (key, property) in properties {
        let title = property
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or(key)
            .to_string();
        let kind = if property.get("type").and_then(|v| v.as_str()) == Some("boolean") {
            Some(ElicitFieldKind::Select(vec![
                ElicitOption {
                    value: ElicitationContentValue::Boolean(true),
                    label: "是".into(),
                },
                ElicitOption {
                    value: ElicitationContentValue::Boolean(false),
                    label: "否".into(),
                },
            ]))
        } else if let Some(values) = property.get("enum").and_then(|v| v.as_array()) {
            Some(ElicitFieldKind::Select(
                values
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|v| ElicitOption {
                        value: ElicitationContentValue::String(v.into()),
                        label: v.into(),
                    })
                    .collect(),
            ))
        } else if let Some(values) = property.pointer("/items/enum").and_then(|v| v.as_array()) {
            Some(ElicitFieldKind::MultiSelect(
                values
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|v| ElicitOption {
                        value: ElicitationContentValue::String(v.into()),
                        label: v.into(),
                    })
                    .collect(),
            ))
        } else if property.get("type").and_then(|v| v.as_str()) == Some("string") {
            Some(ElicitFieldKind::Text { secret: false })
        } else {
            None
        };
        match kind {
            Some(kind) => fields.push(ElicitField {
                key: key.clone(),
                title,
                kind,
            }),
            None if required.contains(&key.as_str()) => return None,
            None => {}
        }
    }
    (!fields.is_empty()).then_some(fields)
}

fn tool_started(item: &serde_json::Value) -> Option<(String, String, ToolKind)> {
    let id = item.get("id")?.as_str()?.to_string();
    match item.get("type")?.as_str()? {
        "commandExecution" => Some((
            id,
            item.get("command")?.as_str()?.to_string(),
            ToolKind::Execute,
        )),
        "fileChange" => Some((id, "修改文件".into(), ToolKind::Edit)),
        "mcpToolCall" => Some((
            id,
            format!(
                "{} / {}",
                item.get("server")?.as_str()?,
                item.get("tool")?.as_str()?
            ),
            ToolKind::Other,
        )),
        "webSearch" => Some((
            id,
            item.get("query")?.as_str()?.to_string(),
            ToolKind::Search,
        )),
        "dynamicToolCall" => Some((id, item.get("tool")?.as_str()?.into(), ToolKind::Other)),
        "collabAgentToolCall" => Some((
            id,
            format!("协作代理：{}", item.get("tool")?.as_str()?),
            ToolKind::Collaborate,
        )),
        "subAgentActivity" => Some((id, "子代理活动".into(), ToolKind::Collaborate)),
        "imageView" => Some((
            id,
            format!("查看图片 {}", item.get("path")?.as_str()?),
            ToolKind::Read,
        )),
        "sleep" => Some((id, "等待".into(), ToolKind::Wait)),
        "imageGeneration" => Some((id, "生成图片".into(), ToolKind::Image)),
        "enteredReviewMode" => Some((id, "进入代码审查".into(), ToolKind::Review)),
        "exitedReviewMode" => Some((id, "完成代码审查".into(), ToolKind::Review)),
        "contextCompaction" => Some((id, "压缩上下文".into(), ToolKind::Compact)),
        _ => None,
    }
}

fn tool_finished(
    item: &serde_json::Value,
    streamed_output: Option<String>,
) -> Option<(String, ToolCallStatus, Vec<ToolOutputPart>)> {
    let id = item.get("id")?.as_str()?.to_string();
    let kind = item.get("type")?.as_str()?;
    if !matches!(
        kind,
        "commandExecution"
            | "fileChange"
            | "mcpToolCall"
            | "webSearch"
            | "dynamicToolCall"
            | "collabAgentToolCall"
            | "subAgentActivity"
            | "imageView"
            | "sleep"
            | "imageGeneration"
            | "enteredReviewMode"
            | "exitedReviewMode"
            | "contextCompaction"
    ) {
        return None;
    }
    let status = match item.get("status").and_then(|v| v.as_str()) {
        Some("failed" | "declined" | "error") => ToolCallStatus::Failed,
        Some("inProgress" | "running") => ToolCallStatus::InProgress,
        _ => ToolCallStatus::Completed,
    };
    let mut output = Vec::new();
    if let Some(text) = item
        .get("aggregatedOutput")
        .and_then(|v| v.as_str())
        .filter(|text| !text.is_empty())
        .or(streamed_output.as_deref())
    {
        output.push(ToolOutputPart::Text(text.into()));
    }
    if kind == "fileChange" {
        for change in item
            .get("changes")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(diff) = change.get("diff").and_then(|v| v.as_str()) {
                output.push(ToolOutputPart::Text(diff.into()));
            } else if let Some(new_text) = change.get("newText").and_then(|v| v.as_str()) {
                output.push(ToolOutputPart::Diff {
                    path: change
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("文件")
                        .into(),
                    old_text: change
                        .get("oldText")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    new_text: new_text.into(),
                });
            }
        }
    }
    for key in ["result", "review", "error", "savedPath"] {
        if let Some(value) = item.get(key).filter(|value| !value.is_null()) {
            output.push(ToolOutputPart::Text(
                value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string()),
            ));
        }
    }
    Some((id, status, output))
}

fn approval(
    value: serde_json::Value,
    writer: &Arc<Mutex<std::process::ChildStdin>>,
    event_tx: &smol::channel::Sender<AcpEvent>,
    expected_thread: &str,
) {
    let Some(request_id) = value.get("id").cloned() else {
        return;
    };
    let method = value
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let params = value.get("params").cloned().unwrap_or_default();
    if params.get("threadId").and_then(|v| v.as_str()) != Some(expected_thread) {
        return;
    }
    let item_id = params
        .get("itemId")
        .and_then(|v| v.as_str())
        .unwrap_or("approval")
        .to_string();
    let (question, details) = if method.contains("commandExecution") {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("执行命令")
            .to_string();
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        (
            command.clone(),
            ApprovalDetailsView::Command {
                command,
                cwd,
                reason,
            },
        )
    } else {
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let grant_root = params
            .get("grantRoot")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        (
            reason.clone().unwrap_or_else(|| "应用文件修改".into()),
            ApprovalDetailsView::FileChange { reason, grant_root },
        )
    };
    let options = approval_options(&params);
    let response_writer = Arc::clone(writer);
    let responder = PermissionResponder::external(move |decision| {
        send(
            &response_writer,
            serde_json::json!({"id":request_id,"result":{"decision":decision}}),
        );
    });
    let _ = event_tx.try_send(AcpEvent::Permission {
        question,
        tool_call_id: ToolCallId::new(item_id),
        pub_options: options,
        responder,
        details,
        raw_request_line: None,
    });
}

fn approval_options(params: &serde_json::Value) -> Vec<PermissionOptionView> {
    let available: Vec<&str> = params
        .get("availableDecisions")
        .and_then(|value| value.as_array())
        .map(|values| values.iter().filter_map(|value| value.as_str()).collect())
        .unwrap_or_else(|| vec!["accept", "acceptForSession", "decline"]);
    let mut options = Vec::new();
    for decision in available {
        let option = match decision {
            "accept" => PermissionOptionView {
                option_id: decision.into(),
                name: "允许一次".into(),
                kind: PermissionOptionKindView::AllowOnce,
            },
            "acceptForSession" => PermissionOptionView {
                option_id: decision.into(),
                name: "本次会话始终允许".into(),
                kind: PermissionOptionKindView::AllowAlways,
            },
            "decline" => PermissionOptionView {
                option_id: decision.into(),
                name: "拒绝".into(),
                kind: PermissionOptionKindView::RejectOnce,
            },
            "cancel" => PermissionOptionView {
                option_id: decision.into(),
                name: "拒绝并停止".into(),
                kind: PermissionOptionKindView::RejectAlways,
            },
            _ => continue,
        };
        options.push(option);
    }
    options
}

fn permissions_approval(
    value: serde_json::Value,
    writer: &Arc<Mutex<std::process::ChildStdin>>,
    event_tx: &smol::channel::Sender<AcpEvent>,
    expected_thread: &str,
) {
    let Some(request_id) = value.get("id").cloned() else {
        return;
    };
    let params = value.get("params").cloned().unwrap_or_default();
    if params.get("threadId").and_then(|v| v.as_str()) != Some(expected_thread) {
        return;
    }
    let item_id = params
        .get("itemId")
        .and_then(|v| v.as_str())
        .unwrap_or("permissions")
        .to_string();
    let requested = params
        .get("permissions")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let reason = params
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("Codex 请求额外的文件系统或网络权限");
    let summary = format!(
        "{reason}\n{}",
        serde_json::to_string_pretty(&requested).unwrap_or_default()
    );
    let options = vec![
        PermissionOptionView {
            option_id: "accept".into(),
            name: "允许本轮".into(),
            kind: PermissionOptionKindView::AllowOnce,
        },
        PermissionOptionView {
            option_id: "acceptForSession".into(),
            name: "本次会话始终允许".into(),
            kind: PermissionOptionKindView::AllowAlways,
        },
        PermissionOptionView {
            option_id: "decline".into(),
            name: "拒绝".into(),
            kind: PermissionOptionKindView::RejectOnce,
        },
    ];
    let response_writer = Arc::clone(writer);
    let responder = PermissionResponder::external(move |decision| {
        let (permissions, scope) = match decision.as_str() {
            "accept" => (requested, "turn"),
            "acceptForSession" => (requested, "session"),
            _ => (serde_json::json!({}), "turn"),
        };
        send(
            &response_writer,
            serde_json::json!({
                "id":request_id,"result":{"permissions":permissions,"scope":scope}
            }),
        );
    });
    let _ = event_tx.try_send(AcpEvent::Permission {
        question: reason.into(),
        tool_call_id: ToolCallId::new(item_id),
        pub_options: options,
        responder,
        details: ApprovalDetailsView::Permissions { summary },
        raw_request_line: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn approval_options_respect_server_decisions() {
        let params = serde_json::json!({"availableDecisions":["accept", "decline"]});
        let options = approval_options(&params);
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].option_id, "accept");
        assert_eq!(options[1].option_id, "decline");
    }

    #[test]
    fn command_items_map_to_native_tool_events() {
        let item = serde_json::json!({
            "id":"item-1", "type":"commandExecution", "command":"cargo test", "status":"inProgress"
        });
        let (id, title, kind) = tool_started(&item).unwrap();
        assert_eq!(id, "item-1");
        assert_eq!(title, "cargo test");
        assert_eq!(kind, ToolKind::Execute);
    }

    #[test]
    fn command_completion_uses_streamed_output_when_aggregate_is_null() {
        let item = serde_json::json!({
            "id":"item-1",
            "type":"commandExecution",
            "command":"rg needle",
            "status":"completed",
            "aggregatedOutput":null
        });
        let (_, status, output) =
            tool_finished(&item, Some("match.rs:10:needle\n".into())).unwrap();
        assert_eq!(status, ToolCallStatus::Completed);
        assert!(matches!(
            output.as_slice(),
            [ToolOutputPart::Text(text)] if text == "match.rs:10:needle\n"
        ));
    }

    #[test]
    fn command_completion_prefers_nonempty_aggregate_over_streamed_copy() {
        let item = serde_json::json!({
            "id":"item-1",
            "type":"commandExecution",
            "command":"printf done",
            "status":"completed",
            "aggregatedOutput":"done"
        });
        let (_, _, output) = tool_finished(&item, Some("done".into())).unwrap();
        assert!(matches!(
            output.as_slice(),
            [ToolOutputPart::Text(text)] if text == "done"
        ));
    }

    #[test]
    fn model_catalog_publishes_model_and_reasoning_controls() {
        let (tx, rx) = smol::channel::unbounded();
        let response = serde_json::json!({"result":{"data":[{
            "model":"gpt-test", "displayName":"GPT Test", "hidden":false,
            "isDefault":true, "defaultReasoningEffort":"medium",
            "supportedReasoningEfforts":[
                {"reasoningEffort":"low","description":"fast"},
                {"reasoningEffort":"medium","description":"balanced"}
            ]
        }]}});
        publish_models(&response, Some("gpt-test"), Some("low"), &tx);
        assert!(
            matches!(rx.try_recv(), Ok(AcpEvent::Model(model)) if model.current_name == "GPT Test")
        );
        assert!(
            matches!(rx.try_recv(), Ok(AcpEvent::ConfigOptions(options)) if options[0].current_name == "低")
        );
    }

    #[test]
    fn mcp_form_schema_maps_select_multi_select_and_boolean() {
        let fields = parse_json_elicitation_fields(Some(&serde_json::json!({
            "type":"object",
            "required":["choice","confirmed"],
            "properties":{
                "choice":{"type":"string","enum":["a","b"]},
                "tags":{"type":"array","items":{"type":"string","enum":["x","y"]}},
                "confirmed":{"type":"boolean"}
            }
        })))
        .unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(
            fields
                .iter()
                .filter(|field| matches!(field.kind, ElicitFieldKind::Select(_)))
                .count(),
            2
        );
        assert_eq!(
            fields
                .iter()
                .filter(|field| matches!(field.kind, ElicitFieldKind::MultiSelect(_)))
                .count(),
            1
        );
    }

    #[test]
    fn fake_app_server_runs_thread_and_turn_lifecycle() {
        let path = std::env::temp_dir().join(format!(
            "smelt-fake-app-server-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            r#"#!/bin/sh
read init
echo '{"id":1,"result":{"userAgent":"fake"}}'
read initialized
read models
echo '{"id":2,"result":{"data":[{"model":"gpt-test","displayName":"GPT Test","hidden":false,"isDefault":true,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"balanced"}]}]}}'
read start
echo '{"id":3,"result":{"thread":{"id":"thread-test"},"model":"gpt-test","reasoningEffort":"medium"}}'
read turn
echo '{"method":"turn/started","params":{"turn":{"id":"turn-test"}}}'
echo '{"method":"item/agentMessage/delta","params":{"threadId":"thread-test","turnId":"turn-test","itemId":"msg-1","delta":"hello"}}'
echo '{"method":"turn/completed","params":{"turn":{"id":"turn-test","status":"completed"}}}'
while read line; do :; done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let handle = spawn_codex_app_server(
            AcpLaunch {
                launch: crate::agent_kind::AcpLaunchSpec::from_command(
                    path.to_string_lossy().into_owned(),
                ),
                cwd: Some("/tmp".into()),
                sid: "acp-test".into(),
                resume_session_id: None,
                resume_needs_transcript_check: false,
            },
            None,
        );
        let recv = || {
            smol::block_on(smol::future::race(
                async { handle.event_rx.recv().await.ok() },
                async {
                    smol::Timer::after(Duration::from_secs(3)).await;
                    None
                },
            ))
            .expect("driver event")
        };
        assert!(matches!(recv(), AcpEvent::Ready { .. }));
        assert!(matches!(recv(), AcpEvent::Model(_)));
        assert!(matches!(recv(), AcpEvent::ConfigOptions(_)));
        handle
            .cmd_tx
            .try_send(AcpCommand::Prompt {
                text: "hi".into(),
                images: Vec::new(),
            })
            .unwrap();
        assert!(matches!(recv(), AcpEvent::AgentChunk { text, .. } if text == "hello"));
        assert!(matches!(recv(), AcpEvent::TurnEnded(_)));
        let _ = handle.cmd_tx.try_send(AcpCommand::Shutdown);
        drop(handle);
        let _ = std::fs::remove_file(path);
    }
}
