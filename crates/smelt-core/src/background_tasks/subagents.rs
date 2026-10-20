//! Pi 子智能体在会话宿主里的登记。
//!
//! 子智能体的进程是会话里的 Pi 起的、靠 stdio 跟 Pi 说话，宿主代管不了，只登记。
//! 一次 `subagent` 工具调用是一次派发，开一条派发连接，连接就是这次派发的命：
//! - 扩展在连接上登记每个子智能体（`start`），宿主分 id 和子会话文件；
//! - 运行中报 `progress`，每个子智能体结束报 `end`，整次派发结束报 `finish`；
//! - 没报 `finish` 就断开（Pi 被杀、崩了），还在跑的子智能体记成已中断，没有结果可交；
//! - 界面要停某个子智能体时，宿主在连接上写 `{"op":"stop","id":...}` 转告扩展。
//!
//! 后台派发报 `finish` 时带回交给会话的结果，和后台命令的完成走同一条监听交回。
//! 前台派发的结果就是工具调用的返回值，这里不再交一遍。

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::acp_conn::ConversationEvent;
use crate::acp_session::{SubagentStatus, SubagentView};

use super::{
    Inner, ModelNotice, State, display_tail, drain_wake, now_ms, poke, poke_stream, poll_wait,
    request_owner, wake_pair, write_line,
};

/// 本次宿主生命期里留着多少个结束的子智能体。卡片靠它拿最终状态；更早的从父会话记录恢复。
const KEEP_FINISHED: usize = 64;

pub(super) struct LiveDispatch {
    /// 派发它的那次 `subagent` 工具调用。交回会话的结果、扩展回的 ack 都认它。
    tool_call_id: String,
    owner: String,
    title: String,
    background: bool,
    /// 连接还在，还没报 `finish`。
    open: bool,
    /// 后台派发交回会话的结果。前台派发、没报 `finish` 就断开的，没有东西可交。
    result: Option<String>,
    status: SubagentStatus,
    /// 派发连接看管线程的唤醒端。要转告扩展停下时叫醒它。
    poke: Option<UnixStream>,
    model_delivered: bool,
    delivering_to: Option<u64>,
    quiet: bool,
}

pub(super) struct LiveSubagent {
    view: SubagentView,
    owner: String,
    /// 界面要停它，还没转告扩展。
    stop_pending: bool,
    /// 界面要停过它。扩展随后报的失败（aborted）按停止记。
    stop_requested: bool,
}

/// 扩展开的派发连接。见模块说明。
pub(super) fn serve_dispatch(
    inner: Arc<Inner>,
    mut stream: UnixStream,
    request: &serde_json::Value,
    mut pending: Vec<u8>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Some(tool_call_id) = text_field(request, "toolCallId") else {
        let _ = write_line(
            &mut stream,
            &serde_json::json!({"ok": false, "error": "缺少 toolCallId"}).to_string(),
        );
        return;
    };
    let Ok((mut wake, poke_tx)) = wake_pair() else {
        let _ = write_line(
            &mut stream,
            &serde_json::json!({"ok": false, "error": "无法登记子智能体派发"}).to_string(),
        );
        return;
    };
    let owner = request_owner(request);
    {
        let mut state = inner.mu.lock().unwrap();
        if state
            .dispatches
            .iter()
            .any(|dispatch| dispatch.tool_call_id == tool_call_id)
        {
            drop(state);
            let _ = write_line(
                &mut stream,
                &serde_json::json!({"ok": false, "error": "这次工具调用已经派发过"}).to_string(),
            );
            return;
        }
        state.dispatches.push(LiveDispatch {
            tool_call_id: tool_call_id.clone(),
            owner: owner.clone(),
            title: text_field(request, "title").unwrap_or_else(|| "子智能体".to_string()),
            background: request
                .get("background")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            open: true,
            result: None,
            status: SubagentStatus::Running,
            poke: Some(poke_tx),
            model_delivered: false,
            delivering_to: None,
            quiet: false,
        });
    }
    if write_line(&mut stream, &serde_json::json!({"ok": true}).to_string()).is_err() {
        settle_dispatch(&inner, &tool_call_id, None);
        return;
    }

    let mut buf = [0u8; 4096];
    let finished = 'serve: loop {
        while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let Ok(message) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            match message.get("op").and_then(serde_json::Value::as_str) {
                Some("start") => {
                    let reply = start_subagent(&inner, &tool_call_id, &owner, &message);
                    if write_line(&mut stream, &reply.to_string()).is_err() {
                        break 'serve None;
                    }
                }
                Some("progress") => record_progress(&inner, &tool_call_id, &message),
                Some("end") => end_subagent(&inner, &tool_call_id, &message),
                Some("finish") => {
                    break 'serve Some(text_field(&message, "text").unwrap_or_default());
                }
                _ => {}
            }
        }
        if inner.stop.load(Ordering::SeqCst) {
            break None;
        }
        for id in take_stop_requests(&inner, &tool_call_id) {
            let line = serde_json::json!({"op": "stop", "id": id}).to_string();
            if write_line(&mut stream, &line).is_err() {
                break 'serve None;
            }
        }
        let ready = match poll_wait(
            &[
                (stream.as_raw_fd(), libc::POLLIN),
                (wake.as_raw_fd(), libc::POLLIN),
            ],
            None,
        ) {
            Ok(ready) => ready,
            Err(_) => break None,
        };
        if ready.get(1) == Some(&true) {
            drain_wake(&mut wake);
        }
        if ready.first() == Some(&true) {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break None,
                Ok(read) => pending.extend_from_slice(&buf[..read]),
            }
        }
    };
    settle_dispatch(&inner, &tool_call_id, finished);
}

/// 登记一个子智能体，分 id 和子会话文件。回复带着扩展给的 `ref`，扩展据此对上请求。
fn start_subagent(
    inner: &Inner,
    tool_call_id: &str,
    owner: &str,
    message: &serde_json::Value,
) -> serde_json::Value {
    let reference = message
        .get("ref")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let id = next_subagent_id();
    let session_file = match prepare_session_file(&id) {
        Ok(path) => path,
        Err(error) => {
            return serde_json::json!({"op": "started", "ref": reference, "error": error});
        }
    };
    {
        let mut state = inner.mu.lock().unwrap();
        let Some(background) = state
            .dispatches
            .iter()
            .find(|dispatch| dispatch.tool_call_id == tool_call_id)
            .map(|dispatch| dispatch.background)
        else {
            return serde_json::json!({"op": "started", "ref": reference, "error": "派发已经结束"});
        };
        prune_finished(&mut state);
        state.subagents.push(LiveSubagent {
            view: SubagentView {
                id: id.clone(),
                tool_call_id: tool_call_id.to_string(),
                parent_session_id: text_field(message, "parentSessionId").unwrap_or_default(),
                title: text_field(message, "title").unwrap_or_else(|| "子智能体".to_string()),
                agent: text_field(message, "agent").unwrap_or_default(),
                model: text_field(message, "model"),
                background,
                status: SubagentStatus::Running,
                started_at_ms: now_ms(),
                finished_at_ms: None,
                turns: 0,
                tool_calls: 0,
                last_activity: None,
                revision: 0,
            },
            owner: owner.to_string(),
            stop_pending: false,
            stop_requested: false,
        });
        publish_locked(&state, &inner.events);
    }
    serde_json::json!({
        "op": "started",
        "ref": reference,
        "id": id,
        "sessionFile": session_file.display().to_string(),
    })
}

/// 子会话文件跨宿主、跨重启留着，id 不能重复。
fn next_subagent_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("sa-{}", &id[..12])
}

/// 建好子会话目录（0700），返回这个子智能体的子会话文件路径。文件由子进程自己写。
fn prepare_session_file(id: &str) -> Result<std::path::PathBuf, String> {
    let dir = smelt_model::agent_paths::subagent_sessions_dir()
        .ok_or_else(|| "找不到 Smelt 主目录，无法保存子会话".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|error| format!("无法创建子会话目录：{error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    smelt_model::agent_paths::subagent_session_file_in(&dir, id)
        .ok_or_else(|| format!("子智能体 id 不能当文件名：{id}"))
}

fn record_progress(inner: &Inner, tool_call_id: &str, message: &serde_json::Value) {
    let Some(id) = text_field(message, "id") else {
        return;
    };
    let mut state = inner.mu.lock().unwrap();
    let Some(subagent) = find_running(&mut state, tool_call_id, &id) else {
        return;
    };
    let count = |key: &str| {
        message
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .map(|value| u32::try_from(value).unwrap_or(u32::MAX))
    };
    let view = &mut subagent.view;
    if let Some(turns) = count("turns") {
        view.turns = turns;
    }
    if let Some(tool_calls) = count("toolCalls") {
        view.tool_calls = tool_calls;
    }
    if let Some(activity) = text_field(message, "lastActivity") {
        view.last_activity = Some(activity);
    }
    view.revision = view.revision.saturating_add(1);
    publish_locked(&state, &inner.events);
}

/// 一个子智能体结束。界面要停过它的，以停止为准：扩展叫停后报的多半是失败（aborted）。
fn end_subagent(inner: &Inner, tool_call_id: &str, message: &serde_json::Value) {
    let Some(id) = text_field(message, "id") else {
        return;
    };
    let reported = SubagentStatus::parse(
        message
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    );
    let mut state = inner.mu.lock().unwrap();
    let Some(subagent) = find_running(&mut state, tool_call_id, &id) else {
        return;
    };
    subagent.view.status = if subagent.stop_requested {
        SubagentStatus::Stopped
    } else if reported.is_running() {
        SubagentStatus::Failed
    } else {
        reported
    };
    subagent.view.finished_at_ms = Some(now_ms());
    subagent.stop_pending = false;
    publish_locked(&state, &inner.events);
}

/// 派发结束。没报 `finish` 就断开的（`result` 为 `None`），还在跑的子智能体记成已中断，
/// 没有结果可交。后台派发带回的结果等监听交回会话；前台派发的结果已经是工具返回值。
fn settle_dispatch(inner: &Inner, tool_call_id: &str, result: Option<String>) {
    {
        let mut state = inner.mu.lock().unwrap();
        let now = now_ms();
        for subagent in &mut state.subagents {
            if subagent.view.tool_call_id == tool_call_id && subagent.view.status.is_running() {
                subagent.view.status = if subagent.stop_requested {
                    SubagentStatus::Stopped
                } else {
                    SubagentStatus::Interrupted
                };
                subagent.view.finished_at_ms = Some(now);
                subagent.stop_pending = false;
            }
        }
        let status = dispatch_status(&state, tool_call_id);
        let Some(index) = state
            .dispatches
            .iter()
            .position(|dispatch| dispatch.tool_call_id == tool_call_id)
        else {
            return;
        };
        let dispatch = &mut state.dispatches[index];
        dispatch.open = false;
        dispatch.poke = None;
        dispatch.status = status;
        let deliver = dispatch.background && result.is_some();
        if deliver {
            dispatch.result = result;
        } else {
            state.dispatches.remove(index);
        }
        if !inner.stop.load(Ordering::SeqCst) {
            publish_locked(&state, &inner.events);
        }
        inner.cv.notify_all();
    }
    if !inner.stop.load(Ordering::SeqCst) {
        poke(inner);
    }
}

/// 一次派发交回会话时说的状态：全完成是完成，全停掉是停止，其余有失败或中断的是失败。
/// 停掉一部分、其余完成的，交回的仍是有用的结果，算完成。
fn dispatch_status(state: &State, tool_call_id: &str) -> SubagentStatus {
    let statuses: Vec<SubagentStatus> = state
        .subagents
        .iter()
        .filter(|subagent| subagent.view.tool_call_id == tool_call_id)
        .map(|subagent| subagent.view.status)
        .collect();
    if statuses.is_empty() {
        return SubagentStatus::Failed;
    }
    if statuses
        .iter()
        .all(|status| *status == SubagentStatus::Stopped)
    {
        return SubagentStatus::Stopped;
    }
    if statuses
        .iter()
        .any(|status| matches!(status, SubagentStatus::Failed | SubagentStatus::Interrupted))
    {
        return SubagentStatus::Failed;
    }
    SubagentStatus::Completed
}

fn find_running<'a>(
    state: &'a mut State,
    tool_call_id: &str,
    id: &str,
) -> Option<&'a mut LiveSubagent> {
    state.subagents.iter_mut().find(|subagent| {
        subagent.view.id == id
            && subagent.view.tool_call_id == tool_call_id
            && subagent.view.status.is_running()
    })
}

/// 界面要停一个会话自己的子智能体：记下来，叫醒它的派发连接去转告扩展。
pub(super) fn request_stop(inner: &Inner, id: &str) -> Result<(), String> {
    let mut guard = inner.mu.lock().unwrap();
    let state = &mut *guard;
    let subagent = state
        .subagents
        .iter_mut()
        .find(|subagent| subagent.view.id == id && subagent.owner.is_empty())
        .ok_or_else(|| format!("没有子智能体 {id}"))?;
    if !subagent.view.status.is_running() {
        return Ok(());
    }
    subagent.stop_requested = true;
    subagent.stop_pending = true;
    let tool_call_id = subagent.view.tool_call_id.clone();
    if let Some(poke) = state
        .dispatches
        .iter()
        .find(|dispatch| dispatch.tool_call_id == tool_call_id)
        .and_then(|dispatch| dispatch.poke.as_ref())
    {
        poke_stream(poke);
    }
    Ok(())
}

fn take_stop_requests(inner: &Inner, tool_call_id: &str) -> Vec<String> {
    let mut state = inner.mu.lock().unwrap();
    state
        .subagents
        .iter_mut()
        .filter(|subagent| subagent.view.tool_call_id == tool_call_id && subagent.stop_pending)
        .map(|subagent| {
            subagent.stop_pending = false;
            subagent.view.id.clone()
        })
        .collect()
}

/// 宿主停机：叫醒每条派发连接，让它们看到停机标志后退出。
pub(super) fn wake_dispatches(state: &State) {
    for dispatch in &state.dispatches {
        if let Some(poke) = dispatch.poke.as_ref() {
            poke_stream(poke);
        }
    }
}

/// 结束的留最近 `KEEP_FINISHED` 个，在跑的都留着。
fn prune_finished(state: &mut State) {
    let finished = state
        .subagents
        .iter()
        .filter(|subagent| !subagent.view.status.is_running())
        .count();
    let mut excess = finished.saturating_sub(KEEP_FINISHED - 1);
    state.subagents.retain(|subagent| {
        if excess > 0 && !subagent.view.status.is_running() {
            excess -= 1;
            return false;
        }
        true
    });
}

pub(super) fn publish_locked(state: &State, events: &smol::channel::Sender<ConversationEvent>) {
    let views = state
        .subagents
        .iter()
        .filter(|subagent| subagent.owner.is_empty())
        .map(|subagent| subagent.view.clone())
        .collect();
    let _ = events.try_send(ConversationEvent::Subagents(views));
}

// 交回会话：和后台命令的完成共用监听、ack 和重发，见上层的 `claim_model` 等。

fn notice_of(dispatch: &LiveDispatch, state: &State) -> ModelNotice {
    let output = dispatch.result.clone().unwrap_or_default();
    let runs: Vec<serde_json::Value> = state
        .subagents
        .iter()
        .filter(|subagent| subagent.view.tool_call_id == dispatch.tool_call_id)
        .map(|subagent| {
            serde_json::json!({
                "id": subagent.view.id,
                "title": subagent.view.title,
                "status": subagent.view.status.as_str(),
            })
        })
        .collect();
    ModelNotice {
        id: dispatch.tool_call_id.clone(),
        kind: "subagent",
        title: dispatch.title.clone(),
        status: dispatch.status.as_str().to_string(),
        exit_code: None,
        output_tail: display_tail(&output),
        output,
        runs: Some(serde_json::Value::Array(runs)),
        peer: None,
        // 全被停掉的不叫醒模型；停止时被清掉、放回来重发的也不叫醒，用户刚按过停止。
        wake: dispatch.status != SubagentStatus::Stopped && !dispatch.quiet,
    }
}

fn deliverable(dispatch: &LiveDispatch) -> bool {
    !dispatch.open && dispatch.result.is_some() && !dispatch.model_delivered
}

pub(super) fn claim(state: &mut State, owner: &str, listener_id: u64) -> Option<ModelNotice> {
    let index = state.dispatches.iter().position(|dispatch| {
        dispatch.owner == owner
            && deliverable(dispatch)
            && dispatch.delivering_to != Some(listener_id)
    })?;
    state.dispatches[index].delivering_to = Some(listener_id);
    Some(notice_of(&state.dispatches[index], state))
}

/// 扩展确认 Pi 已经把这次派发的结果交给了模型。交到了就不用再留着。
pub(super) fn acknowledge(state: &mut State, owner: &str, tool_call_id: &str) {
    state.dispatches.retain(|dispatch| {
        !(dispatch.tool_call_id == tool_call_id && dispatch.owner == owner && !dispatch.open)
    });
}

pub(super) fn still_claimed(state: &State, tool_call_id: &str, listener_id: u64) -> bool {
    state.dispatches.iter().any(|dispatch| {
        dispatch.tool_call_id == tool_call_id
            && deliverable(dispatch)
            && dispatch.delivering_to == Some(listener_id)
    })
}

pub(super) fn unclaim(state: &mut State, tool_call_id: &str, listener_id: u64) {
    if let Some(dispatch) = state.dispatches.iter_mut().find(|dispatch| {
        dispatch.tool_call_id == tool_call_id && dispatch.delivering_to == Some(listener_id)
    }) {
        dispatch.delivering_to = None;
    }
}

pub(super) fn queue_cleared(state: &mut State) {
    for dispatch in &mut state.dispatches {
        if dispatch.owner.is_empty() && deliverable(dispatch) && dispatch.delivering_to.is_some() {
            dispatch.delivering_to = None;
            dispatch.quiet = true;
        }
    }
}

fn text_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}
