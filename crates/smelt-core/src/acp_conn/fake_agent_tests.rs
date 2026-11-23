//! 用测试程序自己当 ACP agent，端到端走完整条 spawn 路径：真子进程、真 stdio、
//! SDK 真实的消息分发。不依赖本机装了什么解释器，CI 每次都跑。
//!
//! 测试带着 [`FAKE_AGENT_ENV`] 再起一份测试程序自己。`ctor` 在 main 之前、libtest 往
//! stdout 打任何东西之前接管这个进程，按剧本在 stdio 上说 JSON-RPC，说完就退出。
//! 不能放进普通测试函数里接管：libtest 会先打一行 `running 1 test`，弄脏 JSON-RPC 通道。

use std::collections::BTreeMap;
use std::io::BufRead as _;

use serde_json::{Value, json};

use super::*;

const FAKE_AGENT_ENV: &str = "SMELT_TEST_FAKE_ACP_AGENT";
const FAKE_IMAGE_ENV: &str = "SMELT_TEST_FAKE_ACP_IMAGE";

/// `session/load` 时先重放两条历史再回响应。
const SCRIPT_REPLAY: &str = "replay";
/// `session/prompt` 时连吐几段正文增量，紧接着回收尾响应。
const SCRIPT_STREAM: &str = "stream";
/// 实时 `read_file` 读图但不带 `content`（模拟 `agy acp` 行为）。
const SCRIPT_READ_IMAGE_LIVE: &str = "read-image-live";
/// `session/load` 重放已完成 `read_file` 读图但不带 `content`（模拟 `agy acp` 重放）。
const SCRIPT_READ_IMAGE_REPLAY: &str = "read-image-replay";

/// 一轮回复拆成多段：一次性吐完整段就测不出「正文必须排在回合结束之前」。
const STREAM_CHUNKS: [&str; 3] = ["你", "好", "，世界"];

#[ctor::ctor(unsafe)]
fn serve_as_fake_agent() {
    let Some(script) = std::env::var_os(FAKE_AGENT_ENV) else {
        return;
    };
    serve(&script.to_string_lossy());
    std::process::exit(0);
}

fn serve(script: &str) {
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            return;
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // 只回请求；client 发来的通知一律不理。
        let (Some(id), Some(method)) = (
            message.get("id").cloned(),
            message.get("method").and_then(Value::as_str),
        ) else {
            continue;
        };
        let session_id = &message["params"]["sessionId"];
        let result = match method {
            "initialize" => json!({
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": true},
            }),
            "session/new" => json!({"sessionId": "fake-session"}),
            "session/load" if script == SCRIPT_REPLAY => {
                // ACP 规范要求重放先于响应。
                send(
                    &mut out,
                    update(session_id, "user_message_chunk", "早先的问题"),
                );
                send(
                    &mut out,
                    update(session_id, "agent_message_chunk", "早先的回答"),
                );
                json!({})
            }
            "session/load" if script == SCRIPT_READ_IMAGE_REPLAY => {
                let path = std::env::var(FAKE_IMAGE_ENV).unwrap_or_default();
                send(
                    &mut out,
                    read_tool_update(
                        session_id,
                        "tool_call",
                        "call-replay-img",
                        "completed",
                        &path,
                    ),
                );
                json!({})
            }
            "session/prompt" if script == SCRIPT_STREAM => {
                // 增量和收尾响应背靠背写出，两条路的先后竞争压到最紧。
                for chunk in STREAM_CHUNKS {
                    send(&mut out, update(session_id, "agent_message_chunk", chunk));
                }
                json!({"stopReason": "end_turn"})
            }
            "session/prompt" if script == SCRIPT_READ_IMAGE_LIVE => {
                let path = std::env::var(FAKE_IMAGE_ENV).unwrap_or_default();
                send(
                    &mut out,
                    read_tool_update(
                        session_id,
                        "tool_call",
                        "call-live-img",
                        "in_progress",
                        &path,
                    ),
                );
                send(
                    &mut out,
                    read_tool_update(
                        session_id,
                        "tool_call_update",
                        "call-live-img",
                        "completed",
                        &path,
                    ),
                );
                json!({"stopReason": "end_turn"})
            }
            _ => json!({}),
        };
        send(
            &mut out,
            json!({"jsonrpc": "2.0", "id": id, "result": result}),
        );
    }
}

fn send(out: &mut impl std::io::Write, message: Value) {
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

fn update(session_id: &Value, kind: &str, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {"sessionUpdate": kind, "content": {"type": "text", "text": text}},
        },
    })
}

fn read_tool_update(
    session_id: &Value,
    session_update: &str,
    tool_call_id: &str,
    status: &str,
    path: &str,
) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": session_update,
                "toolCallId": tool_call_id,
                "name": "read_file",
                "status": status,
                "kind": "read",
                "title": format!("File {path}"),
                "locations": [{"path": path}],
                "rawInput": {"Path": path},
            },
        },
    })
}

fn launch_fake_agent_with_env(
    script: &str,
    resume_session_id: Option<&str>,
    extra_env: BTreeMap<String, String>,
) -> ConversationHandle {
    let exe = std::env::current_exe().expect("拿不到测试程序路径");
    let mut env = extra_env;
    env.insert(FAKE_AGENT_ENV.to_string(), script.to_string());
    spawn_acp(
        ConversationLaunch {
            launch: ConversationLaunchSpec {
                // 启动命令按空白分词：测试程序路径里不能有空格。
                command: exe.to_string_lossy().into_owned(),
                env,
            },
            ephemeral_env: Default::default(),
            cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            sid: format!("acp-fake-{script}"),
            agent_token: String::new(),
            resume_session_id: resume_session_id.map(SessionId::new),
            fork_session_id: None,
            fork_cut: None,
            peer_handler: None,
            disallowed_tools: Vec::new(),
        },
        None,
    )
}

fn launch_fake_agent(script: &str, resume_session_id: Option<&str>) -> ConversationHandle {
    launch_fake_agent_with_env(script, resume_session_id, BTreeMap::new())
}

fn event_name(event: &ConversationEvent) -> &'static str {
    match event {
        ConversationEvent::HistoryReplayStarted => "Started",
        ConversationEvent::HistoryReplayFinished => "Finished",
        ConversationEvent::UserChunk(_) => "User",
        ConversationEvent::AgentChunk { .. } => "Agent",
        ConversationEvent::Ready { .. } => "Ready",
        ConversationEvent::TurnEnded(_) => "TurnEnded",
        ConversationEvent::TurnFailed(_) => "TurnFailed",
        ConversationEvent::Fatal(_) => "Fatal",
        ConversationEvent::RestoreFailed(_) => "RestoreFailed",
        _ => "other",
    }
}

/// 收事件直到 `done` 命中或超时。`other` 不记，记下的是关心的事件名和正文增量。
fn collect_until(
    handle: &ConversationHandle,
    mut done: impl FnMut(&ConversationEvent) -> bool,
) -> (Vec<&'static str>, String) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut seen = Vec::new();
    let mut agent_text = String::new();
    while std::time::Instant::now() < deadline {
        let next = smol::block_on(async {
            smol::future::race(async { handle.event_rx.recv().await.ok() }, async {
                smol::Timer::after(std::time::Duration::from_millis(200)).await;
                None
            })
            .await
        });
        let Some(event) = next else { continue };
        if let ConversationEvent::AgentChunk { text, .. } = &event {
            agent_text.push_str(text);
        }
        let name = event_name(&event);
        if name != "other" {
            seen.push(name);
        }
        if done(&event) {
            break;
        }
    }
    (seen, agent_text)
}

fn connection_settled(event: &ConversationEvent) -> bool {
    matches!(
        event,
        ConversationEvent::Ready { .. }
            | ConversationEvent::Fatal(_)
            | ConversationEvent::RestoreFailed(_)
    )
}

/// `session/load` 的端到端顺序。SDK 2.1 把响应前的重放放进会话自己的队列，要等
/// 驱动读出来才进事件通道；只看单测里的归约器测不出「收尾信号抢在重放正文前面」。
#[test]
fn replay_is_delivered_before_history_replay_finished() {
    let handle = launch_fake_agent(SCRIPT_REPLAY, Some("session-from-disk"));
    let (seen, _) = collect_until(&handle, connection_settled);
    let _ = handle.cmd_tx.send_blocking(ConversationCommand::Shutdown);

    let position = |name: &str| seen.iter().position(|seen| *seen == name);
    let finished = position("Finished").unwrap_or_else(|| panic!("没有收尾信号：{seen:?}"));
    let agent = position("Agent").unwrap_or_else(|| panic!("重放正文没送达：{seen:?}"));
    let user = position("User").unwrap_or_else(|| panic!("重放的用户消息没送达：{seen:?}"));
    assert!(
        position("Started").is_some_and(|started| started < user),
        "{seen:?}"
    );
    assert!(
        user < finished && agent < finished,
        "重放正文必须先于 HistoryReplayFinished，否则草稿以空正文提交、历史被当成实时消息：{seen:?}"
    );
}

/// prompt 的完成响应由我们自己的回调经 cmd 通道送回循环，流式更新走 SDK 的
/// update 队列——两条不同的路。回合结束若抢在正文前面到 UI，正文就会落在
/// 「已结束」的回合后面，或者干脆在回合结束后才到、被收尾截掉。
#[test]
fn streamed_text_arrives_before_the_turn_ends() {
    let handle = launch_fake_agent(SCRIPT_STREAM, None);
    let (seen, _) = collect_until(&handle, connection_settled);
    assert!(seen.contains(&"Ready"), "握手没成功：{seen:?}");

    handle
        .cmd_tx
        .send_blocking(ConversationCommand::Prompt {
            text: "你好".to_string(),
            images: Vec::new(),
        })
        .expect("发 prompt");
    let (seen, agent_text) = collect_until(&handle, |event| {
        event.ends_turn() || matches!(event, ConversationEvent::Fatal(_))
    });
    let _ = handle.cmd_tx.send_blocking(ConversationCommand::Shutdown);

    assert_eq!(
        seen.last(),
        Some(&"TurnEnded"),
        "这一轮应当成功收场：{seen:?}"
    );
    // 收到回合结束就停：晚到的增量不会被收进来，拼出来的正文就不完整。
    assert_eq!(
        agent_text,
        STREAM_CHUNKS.concat(),
        "正文必须全部排在回合结束之前：{seen:?}"
    );
}

fn collect_events_until(
    handle: &ConversationHandle,
    mut done: impl FnMut(&ConversationEvent) -> bool,
) -> Vec<ConversationEvent> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut events = Vec::new();
    while std::time::Instant::now() < deadline {
        let next = smol::block_on(async {
            smol::future::race(async { handle.event_rx.recv().await.ok() }, async {
                smol::Timer::after(std::time::Duration::from_millis(200)).await;
                None
            })
            .await
        });
        let Some(event) = next else { continue };
        let finished = done(&event);
        events.push(event);
        if finished {
            break;
        }
    }
    events
}

const TINY_PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, b'I', b'D', b'A', b'T', 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xAE,
    0x42, 0x60, 0x82,
];

#[test]
fn live_image_read_without_content_is_hydrated_from_disk() {
    use base64::Engine as _;

    let dir = tempfile::tempdir().expect("创建临时目录");
    let img_path = dir.path().join("sim_play_check3.png");
    std::fs::write(&img_path, TINY_PNG).expect("写入测试图片");

    let handle = launch_fake_agent_with_env(
        SCRIPT_READ_IMAGE_LIVE,
        None,
        BTreeMap::from([(
            FAKE_IMAGE_ENV.to_string(),
            img_path.to_string_lossy().into_owned(),
        )]),
    );
    let mut state = crate::acp_session::AcpSessionState::default();
    for event in collect_events_until(&handle, connection_settled) {
        let _ = crate::acp_session::apply_event(&mut state, event);
    }
    handle
        .cmd_tx
        .send_blocking(ConversationCommand::Prompt {
            text: "看图".to_string(),
            images: Vec::new(),
        })
        .expect("发 prompt");
    for event in collect_events_until(&handle, |event| {
        event.ends_turn() || matches!(event, ConversationEvent::Fatal(_))
    }) {
        let _ = crate::acp_session::apply_event(&mut state, event);
    }
    let _ = handle.cmd_tx.send_blocking(ConversationCommand::Shutdown);

    let expected_b64 = base64::engine::general_purpose::STANDARD.encode(TINY_PNG);
    let has_image = state.entries().iter().any(|entry| {
        matches!(
            entry,
            crate::acp_chat::AcpEntry::ToolCall {
                kind: crate::acp_chat::ToolKind::Read,
                status: crate::acp_chat::ToolCallStatus::Completed,
                output,
                ..
            } if output.iter().any(|part| matches!(
                part,
                crate::acp_chat::ToolOutputPart::Image(img)
                    if img.mime == "image/png" && img.data_b64 == expected_b64
            ))
        )
    });
    assert!(
        has_image,
        "无 content 的实时图片 read 应按磁盘路径回填 ToolOutputPart::Image：{:?}",
        state.entries()
    );
}

#[test]
fn replayed_image_read_without_content_is_hydrated_from_disk() {
    use base64::Engine as _;

    let dir = tempfile::tempdir().expect("创建临时目录");
    let img_path = dir.path().join("sim_play_live.png");
    std::fs::write(&img_path, TINY_PNG).expect("写入测试图片");

    let handle = launch_fake_agent_with_env(
        SCRIPT_READ_IMAGE_REPLAY,
        Some("session-from-disk"),
        BTreeMap::from([(
            FAKE_IMAGE_ENV.to_string(),
            img_path.to_string_lossy().into_owned(),
        )]),
    );
    let mut state = crate::acp_session::AcpSessionState::default();
    for event in collect_events_until(&handle, connection_settled) {
        let _ = crate::acp_session::apply_event(&mut state, event);
    }
    let _ = handle.cmd_tx.send_blocking(ConversationCommand::Shutdown);

    let expected_b64 = base64::engine::general_purpose::STANDARD.encode(TINY_PNG);
    let has_image = state.entries().iter().any(|entry| {
        matches!(
            entry,
            crate::acp_chat::AcpEntry::ToolCall {
                kind: crate::acp_chat::ToolKind::Read,
                status: crate::acp_chat::ToolCallStatus::Completed,
                output,
                ..
            } if output.iter().any(|part| matches!(
                part,
                crate::acp_chat::ToolOutputPart::Image(img)
                    if img.mime == "image/png" && img.data_b64 == expected_b64
            ))
        )
    });
    assert!(
        has_image,
        "无 content 的历史重放图片 read 应按磁盘路径回填 ToolOutputPart::Image：{:?}",
        state.entries()
    );
}
