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

fn codex_start_params(cwd: &Option<String>, extended_history: bool) -> serde_json::Value {
    let mut params = serde_json::json!({"cwd": cwd});
    if extended_history {
        params["persistExtendedHistory"] = serde_json::Value::Bool(true);
    }
    params
}

fn codex_resume_params(session_id: &SessionId, extended_history: bool) -> serde_json::Value {
    let mut params = serde_json::json!({"threadId": session_id.to_string()});
    if extended_history {
        params["persistExtendedHistory"] = serde_json::Value::Bool(true);
    }
    params
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

#[derive(Clone, Debug)]
struct HookTrustListing {
    key: String,
    command: String,
    current_hash: String,
    trust_status: String,
}

fn hook_trust_listings(response: &serde_json::Value) -> Vec<HookTrustListing> {
    response
        .pointer("/result/data")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .flat_map(|scope| {
            scope
                .get("hooks")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
        })
        .filter_map(|hook| {
            Some(HookTrustListing {
                key: hook.get("key")?.as_str()?.to_string(),
                command: hook.get("command")?.as_str()?.to_string(),
                current_hash: hook.get("currentHash")?.as_str()?.to_string(),
                trust_status: hook.get("trustStatus")?.as_str()?.to_string(),
            })
        })
        .collect()
}

fn matching_hook_trust_listings(
    response: &serde_json::Value,
    hooks_path: &std::path::Path,
    expected_commands: &[String],
) -> Result<Vec<HookTrustListing>, String> {
    let path_prefix = format!("{}:", hooks_path.to_string_lossy());
    let expected: std::collections::HashSet<&str> =
        expected_commands.iter().map(String::as_str).collect();
    let mut matched: Vec<_> = hook_trust_listings(response)
        .into_iter()
        .filter(|hook| {
            hook.key.starts_with(&path_prefix) && expected.contains(hook.command.as_str())
        })
        .collect();
    matched.sort_by(|a, b| a.key.cmp(&b.key));
    matched.dedup_by(|a, b| a.key == b.key);
    let commands: std::collections::HashSet<&str> =
        matched.iter().map(|hook| hook.command.as_str()).collect();
    if matched.len() != expected.len() || commands != expected {
        return Err(format!(
            "Codex hooks/list 只匹配到 {}/{} 个 Smelt hooks",
            matched.len(),
            expected.len()
        ));
    }
    Ok(matched)
}

/// 通过 Codex app-server 自己的 RPC 信任 Smelt 管理的 hooks。只接受指定 hooks.json
/// 路径下、命令与 expected_commands 完全一致的条目；hash 只使用 hooks/list 返回值，
/// 写入后再次 list 验证，不复制 Codex 的私有 hash 算法。
pub fn grant_codex_hook_trust(
    hooks_path: &std::path::Path,
    cwd: &std::path::Path,
    expected_commands: &[String],
) -> Result<usize, String> {
    if expected_commands.is_empty() {
        return Err("没有待信任的 Codex hooks".into());
    }
    let mut command = Command::new(resolve_program("codex"));
    command
        .arg("app-server")
        .current_dir(cwd)
        .env("PATH", crate::login_env::login_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(codex_home) = crate::login_env::codex_home() {
        command.env("CODEX_HOME", codex_home);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动 Codex app-server 信任会话失败：{error}"))?;
    let result = (|| {
        let stdin = child.stdin.take().ok_or("Codex app-server 没有 stdin")?;
        let stdout = child.stdout.take().ok_or("Codex app-server 没有 stdout")?;
        let stderr = child.stderr.take().ok_or("Codex app-server 没有 stderr")?;
        std::thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
        let writer = Arc::new(Mutex::new(stdin));
        let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = line_tx.send(line);
            }
        });

        if !send(
            &writer,
            serde_json::json!({
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {"name":"smelt", "title":"Smelt", "version":env!("CARGO_PKG_VERSION")},
                    "capabilities": {"experimentalApi":true}
                }
            }),
        ) {
            return Err("写入 Codex app-server initialize 失败".into());
        }
        wait_response(&line_rx, 1)?;
        if !send(
            &writer,
            serde_json::json!({"method":"initialized","params":{}}),
        ) {
            return Err("写入 Codex app-server initialized 失败".into());
        }
        if !send(
            &writer,
            serde_json::json!({"id":2,"method":"hooks/list","params":{"cwds":[cwd]}}),
        ) {
            return Err("写入 Codex app-server hooks/list 失败".into());
        }
        let before = wait_response(&line_rx, 2)?;
        let matched = matching_hook_trust_listings(&before, hooks_path, expected_commands)?;
        let needing_trust: Vec<_> = matched
            .iter()
            .filter(|hook| hook.trust_status != "trusted")
            .collect();
        if !needing_trust.is_empty() {
            let value = needing_trust
                .iter()
                .map(|hook| {
                    (
                        hook.key.clone(),
                        serde_json::json!({"trusted_hash":hook.current_hash}),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            if !send(
                &writer,
                serde_json::json!({
                    "id":3,
                    "method":"config/batchWrite",
                    "params":{
                        "edits":[{"keyPath":"hooks.state","value":value,"mergeStrategy":"upsert"}],
                        "reloadUserConfig":true
                    }
                }),
            ) {
                return Err("写入 Codex app-server config/batchWrite 失败".into());
            }
            wait_response(&line_rx, 3)?;
        }
        let verify_id = if needing_trust.is_empty() { 3 } else { 4 };
        if !send(
            &writer,
            serde_json::json!({"id":verify_id,"method":"hooks/list","params":{"cwds":[cwd]}}),
        ) {
            return Err("写入 Codex app-server hooks/list 复核请求失败".into());
        }
        let after = wait_response(&line_rx, verify_id)?;
        let verified = matching_hook_trust_listings(&after, hooks_path, expected_commands)?;
        if verified.iter().any(|hook| hook.trust_status != "trusted") {
            return Err("Codex hooks/list 复核后仍有 Smelt hook 未信任".into());
        }
        Ok(needing_trust.len())
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
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
    send(
        &writer,
        serde_json::json!({"id":next_id,"method":"permissionProfile/list","params":{"cwd":launch.cwd}}),
    );
    let permission_catalog = wait_response(&line_rx, next_id).unwrap_or_default();
    next_id += 1;
    send(
        &writer,
        serde_json::json!({"id":next_id,"method":"collaborationMode/list","params":{}}),
    );
    let collaboration_catalog = wait_response(&line_rx, next_id).unwrap_or_default();
    next_id += 1;
    send(
        &writer,
        serde_json::json!({"id":next_id,"method":"config/read","params":{"cwd":launch.cwd}}),
    );
    let effective_config = wait_response(&line_rx, next_id).unwrap_or_default();
    next_id += 1;
    let mut resumed = false;
    let thread_result = if let Some(session_id) = &launch.resume_session_id {
        send(
            &writer,
            serde_json::json!({
                "id": next_id,
                "method": "thread/resume",
                "params": codex_resume_params(session_id, true),
            }),
        );
        let mut resume_result = wait_response(&line_rx, next_id);
        if resume_result.is_err() {
            // 旧版 app-server 可能不认识扩展历史参数。重试 legacy resume，不能
            // 因为能力协商失败就直接新建线程，造成上下文悄悄丢失。
            next_id += 1;
            send(
                &writer,
                serde_json::json!({
                    "id": next_id,
                    "method": "thread/resume",
                    "params": codex_resume_params(session_id, false),
                }),
            );
            resume_result = wait_response(&line_rx, next_id);
        }
        match resume_result {
            Ok(value) => {
                resumed = true;
                value
            }
            Err(_) => {
                next_id += 1;
                send(
                    &writer,
                    serde_json::json!({
                        "id": next_id,
                        "method": "thread/start",
                        "params": codex_start_params(&launch.cwd, true),
                    }),
                );
                match wait_response(&line_rx, next_id) {
                    Ok(value) => value,
                    Err(_) => {
                        next_id += 1;
                        send(
                            &writer,
                            serde_json::json!({
                                "id": next_id,
                                "method": "thread/start",
                                "params": codex_start_params(&launch.cwd, false),
                            }),
                        );
                        wait_response(&line_rx, next_id)?
                    }
                }
            }
        }
    } else {
        send(
            &writer,
            serde_json::json!({
                "id": next_id,
                "method": "thread/start",
                "params": codex_start_params(&launch.cwd, true),
            }),
        );
        match wait_response(&line_rx, next_id) {
            Ok(value) => value,
            Err(_) => {
                next_id += 1;
                send(
                    &writer,
                    serde_json::json!({
                        "id": next_id,
                        "method": "thread/start",
                        "params": codex_start_params(&launch.cwd, false),
                    }),
                );
                wait_response(&line_rx, next_id)?
            }
        }
    };
    if resumed {
        replay_codex_thread(&thread_result, &event_tx);
    }
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
    let mut selected_permission = thread_result
        .pointer("/result/activePermissionProfile/id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut selected_collaboration = effective_config
        .pointer("/result/config/collaboration_mode/mode")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    let mut selected_personality = effective_config
        .pointer("/result/config/personality")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_string();
    let mut selected_service_tier = thread_result
        .pointer("/result/serviceTier")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut selected_summary = effective_config
        .pointer("/result/config/model_reasoning_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("auto")
        .to_string();
    let _ = event_tx.try_send(AcpEvent::Ready {
        session_id: SessionId::new(thread_id.clone()),
        kind: if resumed {
            ReadyKind::ResumedWithReplay
        } else {
            ReadyKind::Fresh
        },
        supports_image: true,
    });
    publish_codex_config(
        &model_catalog,
        &permission_catalog,
        &collaboration_catalog,
        selected_model.as_deref(),
        selected_effort.as_deref(),
        selected_permission.as_deref(),
        &selected_collaboration,
        &selected_personality,
        selected_service_tier.as_deref(),
        &selected_summary,
        &event_tx,
    );
    let _ = event_tx.try_send(AcpEvent::AvailableCommands(codex_commands()));

    let mut current_turn: Option<String> = None;
    let mut command_outputs = HashMap::<String, String>::new();
    let mut pending_command_responses = HashMap::<i64, PendingCommandResponse>::new();
    loop {
        while let Ok(command) = cmd_rx.try_recv() {
            match command {
                AcpCommand::Prompt { text, images } => {
                    if images.is_empty()
                        && handle_slash_command(
                            &text,
                            &writer,
                            &event_tx,
                            &thread_id,
                            &mut next_id,
                            &mut selected_collaboration,
                            selected_model.as_deref(),
                            selected_effort.as_deref(),
                            selected_permission.as_deref(),
                            &launch.cwd,
                            &mut pending_command_responses,
                        )
                    {
                        publish_codex_config(
                            &model_catalog,
                            &permission_catalog,
                            &collaboration_catalog,
                            selected_model.as_deref(),
                            selected_effort.as_deref(),
                            selected_permission.as_deref(),
                            &selected_collaboration,
                            &selected_personality,
                            selected_service_tier.as_deref(),
                            &selected_summary,
                            &event_tx,
                        );
                        continue;
                    }
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
                        "permissions" => selected_permission = Some(value_id),
                        "collaboration_mode" => selected_collaboration = value_id,
                        "personality" => selected_personality = value_id,
                        "service_tier" => selected_service_tier = Some(value_id),
                        "reasoning_summary" => selected_summary = value_id,
                        _ => continue,
                    }
                    next_id += 1;
                    let params = config_update_params(
                        &thread_id,
                        &config_id,
                        selected_model.as_deref(),
                        selected_effort.as_deref(),
                        selected_permission.as_deref(),
                        &selected_collaboration,
                        &selected_personality,
                        selected_service_tier.as_deref(),
                        &selected_summary,
                        &collaboration_catalog,
                    );
                    send(
                        &writer,
                        serde_json::json!({"id":next_id,"method":"thread/settings/update","params":params}),
                    );
                    publish_codex_config(
                        &model_catalog,
                        &permission_catalog,
                        &collaboration_catalog,
                        selected_model.as_deref(),
                        selected_effort.as_deref(),
                        selected_permission.as_deref(),
                        &selected_collaboration,
                        &selected_personality,
                        selected_service_tier.as_deref(),
                        &selected_summary,
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
                if let Some(id) = value.get("id").and_then(|value| value.as_i64())
                    && let Some(command) = pending_command_responses.remove(&id)
                {
                    finish_local_command(&event_tx, format_command_response(command, &value));
                    continue;
                }
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

fn codex_commands() -> Vec<(String, String)> {
    vec![
        ("compact".into(), "压缩当前线程上下文".into()),
        ("review".into(), "审查工作区未提交的改动".into()),
        ("plan".into(), "切换计划模式：/plan [任务]".into()),
        ("status".into(), "查看会话配置与线程信息".into()),
        ("diff".into(), "查看工作区当前改动".into()),
        ("init".into(), "创建项目 AGENTS.md 指引".into()),
        ("rename".into(), "重命名当前线程：/rename 名称".into()),
        ("goal".into(), "查看或设置长期目标：/goal [目标]".into()),
        ("mcp".into(), "查看已连接的 MCP 服务".into()),
        ("skills".into(), "查看当前可用 Skills".into()),
        ("apps".into(), "查看当前可用 Apps".into()),
    ]
}

#[derive(Debug, Clone, Copy)]
enum PendingCommandResponse {
    Compact,
    Rename,
    GoalGet,
    GoalSet,
    Mcp,
    Skills,
    Apps,
}

fn finish_local_command(event_tx: &smol::channel::Sender<AcpEvent>, text: String) {
    let _ = event_tx.try_send(AcpEvent::AgentChunk {
        thought: false,
        text,
    });
    let _ = event_tx.try_send(AcpEvent::TurnEnded(StopReason::EndTurn));
}

#[allow(clippy::too_many_arguments)]
fn handle_slash_command(
    text: &str,
    writer: &Arc<Mutex<std::process::ChildStdin>>,
    event_tx: &smol::channel::Sender<AcpEvent>,
    thread_id: &str,
    next_id: &mut i64,
    selected_collaboration: &mut String,
    selected_model: Option<&str>,
    selected_effort: Option<&str>,
    selected_permission: Option<&str>,
    cwd: &Option<String>,
    pending: &mut HashMap<i64, PendingCommandResponse>,
) -> bool {
    let trimmed = text.trim();
    let (command, rest) = trimmed
        .split_once(char::is_whitespace)
        .map_or((trimmed, ""), |(command, rest)| (command, rest.trim()));
    match command {
        "/compact" if rest.is_empty() => {
            send_pending_command(
                writer,
                next_id,
                pending,
                PendingCommandResponse::Compact,
                "thread/compact/start",
                serde_json::json!({"threadId":thread_id}),
            );
            true
        }
        "/review" if rest.is_empty() => {
            *next_id += 1;
            send(
                writer,
                serde_json::json!({"id":*next_id,"method":"review/start","params":{"threadId":thread_id,"target":{"type":"uncommittedChanges"},"delivery":"inline"}}),
            );
            true
        }
        "/plan" => {
            *selected_collaboration = "plan".into();
            *next_id += 1;
            let collaboration = serde_json::json!({
                "mode":"plan",
                "settings":{
                    "model":selected_model.unwrap_or_default(),
                    "reasoning_effort":selected_effort,
                    "developer_instructions":null
                }
            });
            send(
                writer,
                serde_json::json!({"id":*next_id,"method":"thread/settings/update","params":{"threadId":thread_id,"collaborationMode":collaboration}}),
            );
            if rest.is_empty() {
                let _ = event_tx.try_send(AcpEvent::AgentChunk {
                    thought: false,
                    text: "已切换到计划模式。".into(),
                });
                let _ = event_tx.try_send(AcpEvent::TurnEnded(StopReason::EndTurn));
            } else {
                *next_id += 1;
                send(
                    writer,
                    serde_json::json!({"id":*next_id,"method":"turn/start","params":{"threadId":thread_id,"input":[{"type":"text","text":rest}],"collaborationMode":collaboration}}),
                );
            }
            true
        }
        "/status" if rest.is_empty() => {
            finish_local_command(
                event_tx,
                format!(
                    "线程：{thread_id}\n模型：{}\n推理强度：{}\n权限：{}\n协作模式：{}",
                    selected_model.unwrap_or("默认"),
                    selected_effort.unwrap_or("默认"),
                    selected_permission.map(permission_label).unwrap_or("默认"),
                    collaboration_label(selected_collaboration, selected_collaboration),
                ),
            );
            true
        }
        "/diff" if rest.is_empty() => {
            finish_local_command(event_tx, workspace_diff(cwd.as_deref()));
            true
        }
        "/init" if rest.is_empty() => {
            *next_id += 1;
            send(
                writer,
                serde_json::json!({
                    "id":*next_id,
                    "method":"turn/start",
                    "params":{
                        "threadId":thread_id,
                        "input":[{"type":"text","text":"Create an AGENTS.md file with instructions for Codex in this repository."}],
                        "model":selected_model,
                        "effort":selected_effort
                    }
                }),
            );
            true
        }
        "/rename" if !rest.is_empty() => {
            send_pending_command(
                writer,
                next_id,
                pending,
                PendingCommandResponse::Rename,
                "thread/name/set",
                serde_json::json!({"threadId":thread_id,"name":rest}),
            );
            true
        }
        "/rename" => {
            finish_local_command(event_tx, "用法：/rename 新名称".into());
            true
        }
        "/goal" => {
            let (kind, method, params) = if rest.is_empty() {
                (
                    PendingCommandResponse::GoalGet,
                    "thread/goal/get",
                    serde_json::json!({"threadId":thread_id}),
                )
            } else {
                (
                    PendingCommandResponse::GoalSet,
                    "thread/goal/set",
                    serde_json::json!({"threadId":thread_id,"objective":rest,"status":"active"}),
                )
            };
            send_pending_command(writer, next_id, pending, kind, method, params);
            true
        }
        "/mcp" if rest.is_empty() => {
            send_pending_command(
                writer,
                next_id,
                pending,
                PendingCommandResponse::Mcp,
                "mcpServerStatus/list",
                serde_json::json!({"limit":100}),
            );
            true
        }
        "/skills" if rest.is_empty() => {
            send_pending_command(
                writer,
                next_id,
                pending,
                PendingCommandResponse::Skills,
                "skills/list",
                serde_json::json!({"cwds":cwd.iter().collect::<Vec<_>>()}),
            );
            true
        }
        "/apps" if rest.is_empty() => {
            send_pending_command(
                writer,
                next_id,
                pending,
                PendingCommandResponse::Apps,
                "apps/list",
                serde_json::json!({"threadId":thread_id,"limit":100}),
            );
            true
        }
        "/status" | "/diff" | "/init" | "/mcp" | "/skills" | "/apps" => {
            finish_local_command(event_tx, format!("{command} 不接受参数。"));
            true
        }
        _ => false,
    }
}

fn send_pending_command(
    writer: &Arc<Mutex<std::process::ChildStdin>>,
    next_id: &mut i64,
    pending: &mut HashMap<i64, PendingCommandResponse>,
    kind: PendingCommandResponse,
    method: &str,
    params: serde_json::Value,
) {
    *next_id += 1;
    pending.insert(*next_id, kind);
    send(
        writer,
        serde_json::json!({"id":*next_id,"method":method,"params":params}),
    );
}

fn format_command_response(kind: PendingCommandResponse, response: &serde_json::Value) -> String {
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("app-server 请求失败");
        return format!("命令执行失败：{message}");
    }
    let result = response.get("result").unwrap_or(response);
    match kind {
        PendingCommandResponse::Compact => "对话上下文已压缩。".into(),
        PendingCommandResponse::Rename => "线程已重命名。".into(),
        PendingCommandResponse::GoalSet => "长期目标已更新。".into(),
        PendingCommandResponse::GoalGet => {
            let Some(goal) = result.get("goal").filter(|goal| !goal.is_null()) else {
                return "当前线程没有长期目标。".into();
            };
            format!(
                "目标：{}\n状态：{}\nToken：{}{}",
                goal.get("objective")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
                goal.get("status").and_then(|v| v.as_str()).unwrap_or("-"),
                goal.get("tokensUsed").and_then(|v| v.as_u64()).unwrap_or(0),
                goal.get("tokenBudget")
                    .and_then(|v| v.as_u64())
                    .map(|budget| format!(" / {budget}"))
                    .unwrap_or_default(),
            )
        }
        PendingCommandResponse::Mcp => {
            let rows: Vec<String> = result
                .get("data")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
                .map(|server| {
                    let name = server
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("未命名");
                    let auth = server
                        .get("authStatus")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let tools = server
                        .get("tools")
                        .and_then(|v| v.as_object())
                        .map_or(0, serde_json::Map::len);
                    format!("- {name}：{tools} 个工具，认证 {auth}")
                })
                .collect();
            list_result("MCP 服务", rows)
        }
        PendingCommandResponse::Skills => {
            let rows: Vec<String> = result
                .get("data")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
                .flat_map(|entry| {
                    entry
                        .get("skills")
                        .and_then(|value| value.as_array())
                        .into_iter()
                        .flatten()
                })
                .filter(|skill| skill.get("enabled").and_then(|v| v.as_bool()) != Some(false))
                .map(|skill| {
                    let name = skill
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("未命名");
                    let description = skill
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    format!("- {name}：{description}")
                })
                .collect();
            list_result("可用 Skills", rows)
        }
        PendingCommandResponse::Apps => {
            let rows: Vec<String> = result
                .get("data")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
                .map(|app| {
                    let name = app.get("name").and_then(|v| v.as_str()).unwrap_or("未命名");
                    let state = if app.get("isAccessible").and_then(|v| v.as_bool()) == Some(true) {
                        "可用"
                    } else {
                        "未连接"
                    };
                    format!("- {name}：{state}")
                })
                .collect();
            list_result("Apps", rows)
        }
    }
}

fn list_result(title: &str, rows: Vec<String>) -> String {
    if rows.is_empty() {
        format!("{title}：无")
    } else {
        format!("{title}：\n{}", rows.join("\n"))
    }
}

fn workspace_diff(cwd: Option<&str>) -> String {
    let Some(cwd) = cwd else {
        return "当前会话没有工作目录。".into();
    };
    let output = Command::new("git")
        .args(["-C", cwd, "diff", "--no-ext-diff", "HEAD"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let diff = String::from_utf8_lossy(&output.stdout);
            if diff.trim().is_empty() {
                "工作区没有已跟踪文件改动。".into()
            } else {
                format!("```diff\n{}\n```", diff.trim_end())
            }
        }
        Ok(output) => format!(
            "读取 diff 失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => format!("启动 git diff 失败：{error}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn config_update_params(
    thread_id: &str,
    changed: &str,
    model: Option<&str>,
    effort: Option<&str>,
    permission: Option<&str>,
    collaboration: &str,
    personality: &str,
    service_tier: Option<&str>,
    summary: &str,
    collaboration_catalog: &serde_json::Value,
) -> serde_json::Value {
    let mut params = serde_json::json!({"threadId":thread_id});
    let value = match changed {
        "model" => model.map(|v| ("model", serde_json::json!(v))),
        "reasoning_effort" => effort.map(|v| ("effort", serde_json::json!(v))),
        "permissions" => permission.map(|v| ("permissions", serde_json::json!(v))),
        "personality" => Some(("personality", serde_json::json!(personality))),
        "service_tier" => service_tier.map(|v| ("serviceTier", serde_json::json!(v))),
        "reasoning_summary" => Some(("summary", serde_json::json!(summary))),
        "collaboration_mode" => {
            let preset = collaboration_catalog
                .pointer("/result/data")
                .and_then(|v| v.as_array())
                .and_then(|items| {
                    items.iter().find(|item| {
                        item.get("mode").and_then(|v| v.as_str()) == Some(collaboration)
                    })
                });
            let preset_model = preset
                .and_then(|item| item.get("model"))
                .and_then(|v| v.as_str())
                .or(model)
                .unwrap_or_default();
            let preset_effort = preset
                .and_then(|item| item.get("reasoning_effort"))
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| serde_json::json!(effort));
            Some((
                "collaborationMode",
                serde_json::json!({
                    "mode":collaboration,
                    "settings":{
                        "model":preset_model,
                        "reasoning_effort":preset_effort,
                        "developer_instructions":null
                    }
                }),
            ))
        }
        _ => None,
    };
    if let Some((key, value)) = value {
        params[key] = value;
    }
    params
}

#[allow(clippy::too_many_arguments)]
fn publish_codex_config(
    response: &serde_json::Value,
    permission_catalog: &serde_json::Value,
    collaboration_catalog: &serde_json::Value,
    selected_model: Option<&str>,
    selected_effort: Option<&str>,
    selected_permission: Option<&str>,
    selected_collaboration: &str,
    selected_personality: &str,
    selected_service_tier: Option<&str>,
    selected_summary: &str,
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
    let mut configs = Vec::new();
    push_config(
        &mut configs,
        "reasoning_effort",
        "推理强度",
        current,
        efforts
            .iter()
            .filter_map(|option| option.get("reasoningEffort").and_then(|v| v.as_str()))
            .map(|value| (value.to_string(), effort_label(value).to_string()))
            .collect(),
    );

    let permission_options: Vec<_> = permission_catalog
        .pointer("/result/data")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter(|item| item.get("allowed").and_then(|v| v.as_bool()) != Some(false))
        .filter_map(|item| {
            let id = item.get("id")?.as_str()?;
            Some((id.to_string(), permission_label(id).to_string()))
        })
        .collect();
    let current_permission = selected_permission.map(str::to_string).or_else(|| {
        permission_options
            .iter()
            .find(|(id, _)| id == ":workspace")
            .or_else(|| permission_options.first())
            .map(|(id, _)| id.clone())
    });
    if let Some(current) = current_permission {
        push_config(
            &mut configs,
            "permissions",
            "权限",
            &current,
            permission_options,
        );
    }

    let collaboration_options: Vec<_> = collaboration_catalog
        .pointer("/result/data")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let mode = item.get("mode")?.as_str()?;
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or(mode);
            Some((
                mode.to_string(),
                collaboration_label(mode, name).to_string(),
            ))
        })
        .collect();
    push_config(
        &mut configs,
        "collaboration_mode",
        "协作模式",
        selected_collaboration,
        collaboration_options,
    );

    if selected
        .get("supportsPersonality")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        push_config(
            &mut configs,
            "personality",
            "个性",
            selected_personality,
            vec![
                ("none".into(), "无".into()),
                ("friendly".into(), "友好".into()),
                ("pragmatic".into(), "务实".into()),
            ],
        );
    }

    let tiers: Vec<_> = selected
        .get("serviceTiers")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|tier| {
            Some((
                tier.get("id")?.as_str()?.to_string(),
                tier.get("name")?.as_str()?.to_string(),
            ))
        })
        .collect();
    if let Some(current) = selected_service_tier
        .or_else(|| selected.get("defaultServiceTier").and_then(|v| v.as_str()))
    {
        push_config(&mut configs, "service_tier", "服务档位", current, tiers);
    }

    push_config(
        &mut configs,
        "reasoning_summary",
        "推理摘要",
        selected_summary,
        vec![
            ("auto".into(), "自动".into()),
            ("concise".into(), "简洁".into()),
            ("detailed".into(), "详细".into()),
            ("none".into(), "关闭".into()),
        ],
    );
    let _ = event_tx.try_send(AcpEvent::ConfigOptions(configs));
}

fn push_config(
    configs: &mut Vec<SessionConfigState>,
    id: &str,
    name: &str,
    current: &str,
    options: Vec<(String, String)>,
) {
    if options.len() < 2 {
        return;
    }
    let current_name = options
        .iter()
        .find(|(value, _)| value == current)
        .map(|(_, name)| name.clone())
        .unwrap_or_else(|| current.to_string());
    configs.push(SessionConfigState {
        config_id: id.into(),
        name: name.into(),
        description: None,
        current_name,
        options,
    });
}

fn permission_label(id: &str) -> &str {
    match id {
        ":read-only" => "只读",
        ":workspace" => "工作区",
        ":full-access" => "完整访问",
        other => other,
    }
}

fn collaboration_label<'a>(mode: &'a str, fallback: &'a str) -> &'a str {
    match mode {
        "default" => "默认",
        "plan" => "计划",
        _ => fallback,
    }
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
            if let Some((used, size, cached_read)) = parse_token_usage(&usage) {
                let _ = event_tx.try_send(AcpEvent::Usage {
                    used,
                    size,
                    cached_read,
                });
            }
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

fn parse_token_usage(usage: &serde_json::Value) -> Option<(u64, u64, Option<u64>)> {
    // `total` 是线程生命周期累计量，不能拿来除以单次上下文窗口；长会话会轻易
    // 得出几千个百分点。`last` 才是当前回合占用的上下文口径。
    let used = usage.pointer("/last/totalTokens")?.as_u64()?;
    let size = usage.get("modelContextWindow")?.as_u64()?;
    (size > 0).then(|| {
        (
            used,
            size,
            usage
                .pointer("/last/cachedInputTokens")
                .and_then(|v| v.as_u64()),
        )
    })
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

/// `thread/resume` 不会逐条重发历史通知。若调用方提供了分页历史，优先消费
/// `initialTurnsPage.data`；当前兼容路径则读取 `thread.turns` 摘要，再翻译成与
/// ACP `session/load` 相同的事件流。
fn replay_codex_thread(response: &serde_json::Value, event_tx: &smol::channel::Sender<AcpEvent>) {
    let _ = event_tx.try_send(AcpEvent::HistoryReplayStarted);
    let items = response
        .pointer("/result/initialTurnsPage/data")
        .and_then(|value| value.as_array())
        .or_else(|| {
            response
                .pointer("/result/thread/turns")
                .and_then(|value| value.as_array())
        })
        .into_iter()
        .flatten()
        .filter_map(|turn| turn.get("items").and_then(|value| value.as_array()))
        .flatten();

    for item in items {
        match item.get("type").and_then(|value| value.as_str()) {
            Some("userMessage") => {
                let text = item
                    .get("content")
                    .and_then(|value| value.as_array())
                    .into_iter()
                    .flatten()
                    .filter(|part| {
                        part.get("type").and_then(|value| value.as_str()) == Some("text")
                    })
                    .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    let _ = event_tx.try_send(AcpEvent::UserChunk(text));
                }
            }
            Some("agentMessage") => {
                if let Some(text) = item
                    .get("text")
                    .and_then(|value| value.as_str())
                    .filter(|text| !text.is_empty())
                {
                    let _ = event_tx.try_send(AcpEvent::AgentChunk {
                        thought: false,
                        text: text.to_string(),
                    });
                }
            }
            Some("reasoning") => {
                let parts = item
                    .get("summary")
                    .and_then(|value| value.as_array())
                    .filter(|parts| !parts.is_empty())
                    .or_else(|| item.get("content").and_then(|value| value.as_array()));
                let text = parts
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    let _ = event_tx.try_send(AcpEvent::AgentChunk {
                        thought: true,
                        text,
                    });
                }
            }
            _ => {
                if let Some((id, title, kind)) = tool_started(item) {
                    let _ = event_tx.try_send(AcpEvent::ToolStarted { id, title, kind });
                }
                if let Some((id, status, output)) = tool_finished(item, None) {
                    let _ = event_tx.try_send(AcpEvent::ToolFinished { id, status, output });
                }
            }
        }
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
    fn hook_trust_matching_requires_exact_path_and_commands() {
        let response = serde_json::json!({"result":{"data":[{"hooks":[
            {
                "key":"/tmp/smelt-hooks.json:Stop:0:0",
                "command":"SMELT_HOOK_EVENT=Stop smelt-notify",
                "currentHash":"sha256:stop",
                "trustStatus":"pending"
            },
            {
                "key":"/tmp/smelt-hooks.json:SessionStart:0:0",
                "command":"SMELT_HOOK_EVENT=SessionStart smelt-notify",
                "currentHash":"sha256:start",
                "trustStatus":"trusted"
            },
            {
                "key":"/tmp/other-hooks.json:Stop:0:0",
                "command":"SMELT_HOOK_EVENT=Stop smelt-notify",
                "currentHash":"sha256:other",
                "trustStatus":"pending"
            },
            {
                "key":"/tmp/smelt-hooks.json:Stop:1:0",
                "command":"curl example.invalid",
                "currentHash":"sha256:foreign",
                "trustStatus":"pending"
            }
        ]}]}});
        let commands = vec![
            "SMELT_HOOK_EVENT=Stop smelt-notify".to_string(),
            "SMELT_HOOK_EVENT=SessionStart smelt-notify".to_string(),
        ];
        let matched = matching_hook_trust_listings(
            &response,
            std::path::Path::new("/tmp/smelt-hooks.json"),
            &commands,
        )
        .unwrap();
        assert_eq!(matched.len(), 2);
        assert!(matched.iter().all(|hook| {
            hook.key.starts_with("/tmp/smelt-hooks.json:") && commands.contains(&hook.command)
        }));

        let missing = vec!["SMELT_HOOK_EVENT=Missing smelt-notify".to_string()];
        assert!(
            matching_hook_trust_listings(
                &response,
                std::path::Path::new("/tmp/smelt-hooks.json"),
                &missing,
            )
            .is_err()
        );
    }

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
    fn resumed_thread_is_replayed_in_protocol_order() {
        let (tx, rx) = smol::channel::unbounded();
        let response = serde_json::json!({"result":{
            "thread":{"turns":[{"items":[
                {"id":"summary","type":"agentMessage","text":"summary must not win"}
            ]}]},
            "initialTurnsPage":{"data":[{"items":[
                {"id":"user-1","type":"userMessage","content":[{"type":"text","text":"question"}]},
                {"id":"reason-1","type":"reasoning","summary":["thinking"]},
                {"id":"agent-1","type":"agentMessage","text":"answer"},
                {"id":"tool-1","type":"commandExecution","command":"pwd","status":"completed","aggregatedOutput":"/repo"}
            ]}]}
        }});

        replay_codex_thread(&response, &tx);
        drop(tx);
        let events = smol::block_on(async {
            let mut events = Vec::new();
            while let Ok(event) = rx.recv().await {
                events.push(event);
            }
            events
        });

        assert!(matches!(events[0], AcpEvent::HistoryReplayStarted));
        assert!(matches!(&events[1], AcpEvent::UserChunk(text) if text == "question"));
        assert!(matches!(
            &events[2],
            AcpEvent::AgentChunk { thought: true, text } if text == "thinking"
        ));
        assert!(matches!(
            &events[3],
            AcpEvent::AgentChunk { thought: false, text } if text == "answer"
        ));
        assert!(matches!(&events[4], AcpEvent::ToolStarted { id, .. } if id == "tool-1"));
        assert!(matches!(&events[5], AcpEvent::ToolFinished { id, .. } if id == "tool-1"));
        assert_eq!(events.len(), 6);
    }

    #[test]
    fn codex_history_params_enable_full_extended_replay_with_legacy_fallbacks() {
        let session_id = SessionId::new("thread-1");
        let enhanced = codex_resume_params(&session_id, true);
        assert_eq!(enhanced["threadId"], "thread-1");
        assert_eq!(enhanced["persistExtendedHistory"], true);
        assert!(enhanced.get("excludeTurns").is_none());
        assert!(enhanced.get("initialTurnsPage").is_none());

        let legacy = codex_resume_params(&session_id, false);
        assert_eq!(legacy, serde_json::json!({"threadId":"thread-1"}));
        assert_eq!(
            codex_start_params(&Some("/repo".into()), true),
            serde_json::json!({"cwd":"/repo","persistExtendedHistory":true})
        );
    }

    #[test]
    fn message_flow_commands_are_complete_without_duplicating_quick_actions() {
        let commands = codex_commands();
        let names: Vec<&str> = commands.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "compact", "review", "plan", "status", "diff", "init", "rename", "goal", "mcp",
                "skills", "apps"
            ]
        );
    }

    #[test]
    fn command_responses_render_useful_message_flow_text() {
        let goal = serde_json::json!({"result":{"goal":{
            "objective":"完成迁移", "status":"active", "tokensUsed":12, "tokenBudget":100
        }}});
        assert_eq!(
            format_command_response(PendingCommandResponse::GoalGet, &goal),
            "目标：完成迁移\n状态：active\nToken：12 / 100"
        );

        let mcp = serde_json::json!({"result":{"data":[{
            "name":"docs", "authStatus":"oAuth", "tools":{"search":{},"fetch":{}}
        }]}});
        let text = format_command_response(PendingCommandResponse::Mcp, &mcp);
        assert!(text.contains("docs：2 个工具"));

        let error = serde_json::json!({"error":{"message":"unsupported"}});
        assert_eq!(
            format_command_response(PendingCommandResponse::Apps, &error),
            "命令执行失败：unsupported"
        );
    }

    #[test]
    fn context_usage_uses_last_turn_instead_of_thread_total() {
        let usage = serde_json::json!({
            "total":{"totalTokens":5_838_540},
            "last":{"totalTokens":58_380,"cachedInputTokens":41_000},
            "modelContextWindow":258_000
        });
        assert_eq!(
            parse_token_usage(&usage),
            Some((58_380, 258_000, Some(41_000)))
        );
        assert_eq!(parse_token_usage(&serde_json::json!({})), None);
    }

    #[test]
    fn model_catalog_publishes_model_and_reasoning_controls() {
        let (tx, rx) = smol::channel::unbounded();
        let response = serde_json::json!({"result":{"data":[{
            "model":"gpt-test", "displayName":"GPT Test", "hidden":false,
            "isDefault":true, "defaultReasoningEffort":"medium",
            "supportsPersonality":true,
            "defaultServiceTier":"fast",
            "serviceTiers":[
                {"id":"flex","name":"标准"},
                {"id":"fast","name":"快速"}
            ],
            "supportedReasoningEfforts":[
                {"reasoningEffort":"low","description":"fast"},
                {"reasoningEffort":"medium","description":"balanced"}
            ]
        }]}});
        let permissions = serde_json::json!({"result":{"data":[
            {"id":":read-only","allowed":true},
            {"id":":workspace","allowed":true}
        ]}});
        let collaboration = serde_json::json!({"result":{"data":[
            {"name":"Default","mode":"default","model":null,"reasoning_effort":null},
            {"name":"Plan","mode":"plan","model":null,"reasoning_effort":null}
        ]}});
        publish_codex_config(
            &response,
            &permissions,
            &collaboration,
            Some("gpt-test"),
            Some("low"),
            Some(":workspace"),
            "default",
            "pragmatic",
            Some("fast"),
            "auto",
            &tx,
        );
        assert!(
            matches!(rx.try_recv(), Ok(AcpEvent::Model(model)) if model.current_name == "GPT Test")
        );
        assert!(matches!(rx.try_recv(), Ok(AcpEvent::ConfigOptions(options))
                if options.iter().any(|item| item.config_id == "reasoning_effort" && item.current_name == "低")
                && options.iter().any(|item| item.config_id == "permissions" && item.current_name == "工作区")
                && options.iter().any(|item| item.config_id == "collaboration_mode" && item.current_name == "默认")
                && options.iter().any(|item| item.config_id == "personality" && item.current_name == "务实")
                && options.iter().any(|item| item.config_id == "service_tier" && item.current_name == "快速")
                && options.iter().any(|item| item.config_id == "reasoning_summary" && item.current_name == "自动")));
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
read permissions
echo '{"id":3,"result":{"data":[{"id":":read-only","allowed":true},{"id":":workspace","allowed":true}]}}'
read collaboration
echo '{"id":4,"result":{"data":[{"name":"Default","mode":"default"},{"name":"Plan","mode":"plan"}]}}'
read config
echo '{"id":5,"result":{"config":{"model_reasoning_summary":"auto"}}}'
read start
echo '{"id":6,"result":{"thread":{"id":"thread-test"},"model":"gpt-test","reasoningEffort":"medium","activePermissionProfile":{"id":":workspace"}}}'
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
                    smol::Timer::after(Duration::from_secs(10)).await;
                    None
                },
            ))
            .expect("driver event")
        };
        assert!(matches!(recv(), AcpEvent::Ready { .. }));
        assert!(matches!(recv(), AcpEvent::Model(_)));
        assert!(matches!(recv(), AcpEvent::ConfigOptions(_)));
        assert!(matches!(recv(), AcpEvent::AvailableCommands(_)));
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
