//! smeltd 侧任务权威状态与 op handler。任务数据由 smeltd 唯一持有（Mutex 保护，
//! 落盘 `~/.smelt/tasks.json`），GUI 与 `smelt-task` CLI 都走 socket op 读写——
//! 根治「GUI 与 CLI 并发写文件互相覆盖」的竞态。
//!
//! 每个 handler 都是「lock → 调 smelt_core::task 纯函数改内存 → save」的薄层。
//! 纯业务逻辑都放 smelt_core::task，这里只做参数解析与一行 JSON 响应。

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use smelt_core::task::{
    Task, TaskChannel, TaskColumn, TaskFile, TaskRunStatus, begin_run_in_file,
    due_retry_ids_in_file, due_scheduled_ids_in_file, load_tasks_file, mark_run_failed_in_file,
    mark_session_done_in_file, mark_session_failed_in_file, mark_task_done_in_file, now_secs, pick_claimable,
    runs_for_task_in_file, save_tasks_file,
};

/// smeltd 持有的任务权威状态。启动时从磁盘载入。
pub type TaskState = Arc<Mutex<TaskFile>>;

pub fn new_task_state() -> TaskState {
    Arc::new(Mutex::new(load_tasks_file()))
}

fn respond(conn: &mut UnixStream, value: Value) {
    let _ = writeln!(conn, "{value}");
}

/// `{"ok": true, ...extra}`——json! 宏不支持 spread，手动合并进 Map。
fn ok(conn: &mut UnixStream, extra: Value) {
    let mut obj = serde_json::Map::new();
    obj.insert("ok".into(), Value::Bool(true));
    if let Value::Object(map) = extra {
        for (k, v) in map {
            obj.insert(k, v);
        }
    }
    respond(conn, Value::Object(obj));
}

fn err(conn: &mut UnixStream, msg: impl std::fmt::Display) {
    respond(conn, json!({ "ok": false, "err": msg.to_string() }));
}

fn get_task_id(v: &Value, conn: &mut UnixStream) -> Option<String> {
    let id = v["id"].as_str()?;
    if id.is_empty() {
        err(conn, "id 缺失");
        return None;
    }
    Some(id.to_string())
}

/// `task_add`：插入（或按 id 覆盖）一个任务。沿用 GUI upsert 语义（新任务插头部）。
pub fn handle_task_add(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let Ok(task) = serde_json::from_value::<Task>(v["task"].clone()) else {
        return err(&mut conn, "task 字段缺失或非法");
    };
    if task.project_cwd.trim().is_empty() {
        return err(&mut conn, "project_cwd 不能为空");
    }
    let mut file = task_state.lock().unwrap();
    if let Some(slot) = file.tasks.iter_mut().find(|t| t.id == task.id) {
        *slot = task.clone();
    } else {
        file.tasks.insert(0, task.clone());
    }
    save_tasks_file(&file);
    ok(&mut conn, json!({ "task": task }));
}

/// `task_list`：全量任务文件（GUI 刷缓存用）。
pub fn handle_task_list(mut conn: UnixStream, task_state: &TaskState, _v: &Value) {
    let file = task_state.lock().unwrap();
    ok(&mut conn, json!({ "file": file.clone() }));
}

/// `task_update`：按 id 全量替换。
pub fn handle_task_update(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let Ok(task) = serde_json::from_value::<Task>(v["task"].clone()) else {
        return err(&mut conn, "task 字段缺失或非法");
    };
    let mut file = task_state.lock().unwrap();
    let Some(slot) = file.tasks.iter_mut().find(|t| t.id == task.id) else {
        return err(&mut conn, "任务不存在");
    };
    *slot = task.clone();
    save_tasks_file(&file);
    ok(&mut conn, json!({ "task": task }));
}

/// `task_remove`：删任务 + 其执行记录 + 首包 prompt 文件。
pub fn handle_task_remove(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let Some(id) = get_task_id(v, &mut conn) else { return };
    let mut file = task_state.lock().unwrap();
    file.tasks.retain(|t| t.id != id);
    file.runs.retain(|run| run.task_id != id);
    save_tasks_file(&file);
    if let Some(dir) = smelt_core::task::tasks_dir() {
        let _ = std::fs::remove_file(dir.join("prompts").join(format!("{id}.txt")));
    }
    ok(&mut conn, json!({}));
}

/// `task_done`：agent 自循环「我做完了」→ 单次任务进待审查，重复任务安排下一次执行。
pub fn handle_task_done(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let Some(id) = get_task_id(v, &mut conn) else { return };
    let mut file = task_state.lock().unwrap();
    let now = now_secs();
    if !mark_task_done_in_file(&mut file, &id, now) {
        return err(&mut conn, "任务不存在");
    }
    save_tasks_file(&file);
    ok(&mut conn, json!({}));
}

/// `task_claim`：原子 claim 同 cwd 下一条可自动执行任务，并立即 begin_run。
/// 响应带 task + run；无可用任务时 `task: null`。
pub fn handle_task_claim(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let cwd = v["cwd"].as_str().unwrap_or_default();
    let launch = v["launch"].as_str().unwrap_or("claude");
    let mut file = task_state.lock().unwrap();
    let now = now_secs();
    let Some(idx) = pick_claimable(&file.tasks, cwd, now) else {
        return ok(&mut conn, json!({ "task": Value::Null }));
    };
    let task_id = file.tasks[idx].id.clone();
    // The run snapshots the active GUI launch command; Task remains agent-neutral.
    let Some(run) = begin_run_in_file(&mut file, &task_id, launch, TaskChannel::Pty, now) else {
        return ok(&mut conn, json!({ "task": Value::Null }));
    };
    let task = file.tasks[idx].clone();
    save_tasks_file(&file);
    ok(&mut conn, json!({ "task": task, "run": run }));
}

/// `task_begin_run`：手动「运行」，不 gate 同 cwd 并发。返回新 Run。
pub fn handle_task_begin_run(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let Some(task_id) = v["task_id"].as_str() else { return err(&mut conn, "task_id 缺失") };
    let launch = v["launch"].as_str().unwrap_or("claude");
    let channel = parse_channel(v).unwrap_or(TaskChannel::Pty);
    let mut file = task_state.lock().unwrap();
    let now = now_secs();
    let Some(run) = begin_run_in_file(&mut file, task_id, launch, channel, now) else {
        return err(&mut conn, "任务不存在");
    };
    save_tasks_file(&file);
    ok(&mut conn, json!({ "run": run }));
}

/// `task_attach_session`：把 Run 与稳定 session id 绑定（替代 mark_run_started）。
pub fn handle_task_attach_session(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let Some(task_id) = v["task_id"].as_str() else { return err(&mut conn, "task_id 缺失") };
    let Some(run_id) = v["run_id"].as_str() else { return err(&mut conn, "run_id 缺失") };
    let Some(session_id) = v["session_id"].as_str() else { return err(&mut conn, "session_id 缺失") };
    let mut file = task_state.lock().unwrap();
    let now = now_secs();
    let Some(run) = file
        .runs
        .iter_mut()
        .find(|r| r.id == run_id && r.task_id == task_id)
    else {
        return err(&mut conn, "run 不存在");
    };
    run.status = TaskRunStatus::Running;
    run.session_id = Some(session_id.to_string());
    run.started_at = Some(now);
    let Some(task) = file.tasks.iter_mut().find(|t| t.id == task_id) else {
        return err(&mut conn, "任务不存在");
    };
    task.session_id = Some(session_id.to_string());
    task.current_run_id = Some(run_id.to_string());
    task.column = TaskColumn::Running;
    task.updated_at = now;
    save_tasks_file(&file);
    ok(&mut conn, json!({}));
}

/// `task_session_done`：会话完成边沿 → 绑定任务进待审查。返回 done_cwd。
pub fn handle_task_session_done(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let Some(session_id) = v["session_id"].as_str() else { return err(&mut conn, "session_id 缺失") };
    let mut file = task_state.lock().unwrap();
    let done_cwd = mark_session_done_in_file(&mut file, session_id, now_secs());
    if done_cwd.is_some() {
        save_tasks_file(&file);
    }
    ok(&mut conn, json!({ "done_cwd": done_cwd }));
}

/// `task_session_failed`：会话失败边沿 → 绑定任务按重试策略处理。返回 cwd。
pub fn handle_task_session_failed(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let Some(session_id) = v["session_id"].as_str() else { return err(&mut conn, "session_id 缺失") };
    let error = v["error"].as_str().unwrap_or("agent 回合失败");
    let mut file = task_state.lock().unwrap();
    let cwd = mark_session_failed_in_file(&mut file, session_id, error, now_secs());
    if cwd.is_some() {
        save_tasks_file(&file);
    }
    ok(&mut conn, json!({ "cwd": cwd }));
}

/// `task_run_failed`：执行现场启动失败。返回 cwd。
pub fn handle_task_run_failed(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let Some(task_id) = v["task_id"].as_str() else { return err(&mut conn, "task_id 缺失") };
    let Some(run_id) = v["run_id"].as_str() else { return err(&mut conn, "run_id 缺失") };
    let error = v["error"].as_str().unwrap_or("启动失败");
    let mut file = task_state.lock().unwrap();
    let cwd = mark_run_failed_in_file(&mut file, task_id, run_id, error, now_secs());
    if cwd.is_some() {
        save_tasks_file(&file);
    }
    ok(&mut conn, json!({ "cwd": cwd }));
}

/// `task_due`：已到期的指定时间任务 + 重试冷却到点的任务 id 列表。
pub fn handle_task_due(mut conn: UnixStream, task_state: &TaskState, _v: &Value) {
    let file = task_state.lock().unwrap();
    let now = now_secs();
    let mut ids = due_scheduled_ids_in_file(&file, now);
    ids.extend(due_retry_ids_in_file(&file, now));
    ids.sort();
    ids.dedup();
    ok(&mut conn, json!({ "ids": ids }));
}

/// `task_runs_for`：任务执行历史。
pub fn handle_task_runs_for(mut conn: UnixStream, task_state: &TaskState, v: &Value) {
    let Some(task_id) = v["task_id"].as_str() else { return err(&mut conn, "task_id 缺失") };
    let file = task_state.lock().unwrap();
    let runs = runs_for_task_in_file(&file, task_id);
    ok(&mut conn, json!({ "runs": runs }));
}

fn parse_channel(v: &Value) -> Option<TaskChannel> {
    let channel = v["channel"].as_str()?;
    if channel == "acp" {
        Some(TaskChannel::Acp {
            agent: v["agent"].as_str().unwrap_or("claude").to_string(),
            profile_id: v["profile_id"].as_str().map(|s| s.to_string()),
        })
    } else {
        Some(TaskChannel::Pty)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Multica 上下文辅助：移动端/PC 端追问统一从 smeltd 层发评论到 Multica server
// ─────────────────────────────────────────────────────────────────────────────

/// Multica 会话上下文，从 Task 中提取的 Multica 相关字段。
#[derive(Clone, Debug)]
pub struct MulticaContext {
    pub issue_id: String,
    pub workspace_id: String,
    pub server_task_id: Option<String>,
    pub parent_comment_id: Option<String>,
    pub trigger_comment_content: Option<String>,
}

/// 通过 session_id 在 TaskFile 中查找对应的 Multica 上下文。
/// 如果会话不是 Multica 任务或找不到，返回 None。
pub fn find_multica_context_by_session(
    file: &TaskFile,
    session_id: &str,
) -> Option<MulticaContext> {
    let task = file
        .tasks
        .iter()
        .find(|task| task.session_id.as_deref() == Some(session_id))?;
    // 只有 Multica 任务（有 issue_id）才返回上下文
    let issue_id = task.multica_issue_id.as_ref()?.clone();
    if issue_id.trim().is_empty() {
        return None;
    }
    let workspace_id = task.multica_workspace_id.clone().unwrap_or_default();
    if workspace_id.trim().is_empty() {
        return None;
    }
    Some(MulticaContext {
        issue_id,
        workspace_id,
        server_task_id: task.multica_task_id.clone(),
        parent_comment_id: task.multica_parent_comment_id.clone(),
        trigger_comment_content: task.multica_trigger_comment_content.clone(),
    })
}

/// Multica 凭据（server_url + token），从 ~/.smelt/multica.json 读取。
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct MulticaCredentials {
    #[serde(default)]
    pub server_url: String,
    #[serde(default)]
    pub token: String,
}

/// 从 ~/.smelt/multica.json 读取 Multica 凭据。
pub fn load_multica_credentials() -> MulticaCredentials {
    let path = dirs::home_dir().map(|h| h.join(".smelt").join("multica.json"));
    smelt_core::json_store::load_json(path)
}

/// 发送用户追问到 Multica server。
///
/// 这是移动端/PC 端统一的追问路径：用户输入 → 发评论到 Multica → Multica 分发给
/// runtime → runtime 执行。成功返回 Ok(())，失败返回错误信息。
pub async fn send_multica_prompt(
    context: &MulticaContext,
    credentials: &MulticaCredentials,
    text: &str,
) -> Result<(), String> {
    if credentials.server_url.trim().is_empty() || credentials.token.trim().is_empty() {
        return Err("Multica 未连接（凭据为空）".to_string());
    }

    let client = smelt_multica::MulticaClient::new(&credentials.server_url, &credentials.token)
        .map_err(|e| format!("创建 Multica 客户端失败: {e}"))?;

    // 旧会话可能只保存了 issue 绑定而没有 parent_id。先从 issue 评论列表恢复
    // 线程根，再创建评论；否则这条输入会错误地变成新的主评论。
    let has_trigger_comment = context
        .trigger_comment_content
        .as_deref()
        .is_some_and(|content| !content.trim().is_empty());

    let resolved_parent_id = match &context.parent_comment_id {
        Some(parent_id) if !parent_id.trim().is_empty() => Some(parent_id.clone()),
        _ if has_trigger_comment => client
            .resolve_issue_reply_parent(
                &context.issue_id,
                &context.workspace_id,
                context.server_task_id.as_deref(),
                context.trigger_comment_content.as_deref(),
            )
            .await
            .map_err(|e| format!("解析 Multica 评论线程失败: {e}"))?,
        _ => {
            // direct Issue 没有原评论线程，首次本地输入按顶层评论发送
            None
        }
    };

    if has_trigger_comment && resolved_parent_id.is_none() {
        return Err("未找到 Multica 原评论线程，已阻止创建新的主评论".to_string());
    }

    client
        .add_issue_comment(
            &context.issue_id,
            &context.workspace_id,
            text,
            resolved_parent_id.as_deref(),
        )
        .await
        .map_err(|e| format!("Multica 评论发送失败: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    /// 用一对 socket 模拟一次 op 请求/响应：把 server 端当 handler 的连接，
    /// 调 handler 后从 client 端读回响应。
    fn roundtrip(task_state: &TaskState, request: Value) -> Value {
        let (server, client) = UnixStream::pair().unwrap();
        let op = request["op"].as_str().unwrap().to_string();
        match op.as_str() {
            "task_add" => handle_task_add(server, task_state, &request),
            "task_list" => handle_task_list(server, task_state, &request),
            "task_claim" => handle_task_claim(server, task_state, &request),
            "task_done" => handle_task_done(server, task_state, &request),
            _ => unreachable!(),
        }
        let mut resp = String::new();
        BufReader::new(client).read_line(&mut resp).unwrap();
        serde_json::from_str(&resp).unwrap()
    }

    #[test]
    fn add_list_claim_done_roundtrip() {
        // 临时 HOME，避免污染真实 ~/.smelt/tasks.json。
        let tmp = std::env::temp_dir().join(format!("smeltd-task-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let old_home = std::env::var_os("HOME");
        // set_var 在 smeltd 的 edition 下是 unsafe。
        unsafe { std::env::set_var("HOME", &tmp) };
        let task_state = new_task_state();

        let cwd = "/tmp/proj".to_string();
        let mut t = Task::new(cwd.clone(), "任务一".into(), "首包指令".into());
        t.auto_run = true;
        let id = t.id.clone();
        let add = roundtrip(&task_state, json!({ "op": "task_add", "task": t }));
        assert_eq!(add["ok"], true);

        let list = roundtrip(&task_state, json!({ "op": "task_list" }));
        assert_eq!(list["file"]["tasks"][0]["id"], id);

        // claim：任务在 /tmp/proj，claim 同 cwd → 取到并 begin_run
        let claim = roundtrip(&task_state, json!({
            "op": "task_claim",
            "cwd": "/tmp/proj",
            "launch": "codex --quiet",
        }));
        assert_eq!(claim["task"]["id"], id);
        assert!(claim["run"]["id"].is_string());
        assert_eq!(claim["run"]["launch"], "codex --quiet");
        let _run_id = claim["run"]["id"].as_str().unwrap().to_string();

        // done：agent 声明完成 → 进 Review
        let done = roundtrip(&task_state, json!({ "op": "task_done", "id": id }));
        assert_eq!(done["ok"], true);
        let list2 = roundtrip(&task_state, json!({ "op": "task_list" }));
        assert_eq!(list2["file"]["tasks"][0]["column"], "review");

        // 再 claim 同 cwd：无待办可领（已 Review）→ task null
        let claim2 = roundtrip(&task_state, json!({ "op": "task_claim", "cwd": "/tmp/proj" }));
        assert!(claim2["task"].is_null());

        // 重复任务完成后不进入 Review，而是回待办并安排下一次执行。
        let mut recurring = Task::new(cwd, "每小时任务".into(), "首包指令".into());
        recurring.kind = smelt_core::task::TaskKind::Scheduled;
        recurring.schedule_frequency = smelt_core::task::TaskScheduleFrequency::Hourly;
        recurring.run_at = Some(1);
        let recurring_id = recurring.id.clone();
        let add_recurring = roundtrip(&task_state, json!({ "op": "task_add", "task": recurring }));
        assert_eq!(add_recurring["ok"], true);
        let claim_recurring = roundtrip(
            &task_state,
            json!({ "op": "task_claim", "cwd": "/tmp/proj", "launch": "codex" }),
        );
        assert_eq!(claim_recurring["task"]["id"], recurring_id);
        let done_recurring = roundtrip(
            &task_state,
            json!({ "op": "task_done", "id": recurring_id }),
        );
        assert_eq!(done_recurring["ok"], true);
        let list3 = roundtrip(&task_state, json!({ "op": "task_list" }));
        let recurring = list3["file"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["id"] == recurring_id)
            .unwrap();
        assert_eq!(recurring["column"], "backlog");
        assert_eq!(recurring["schedule_frequency"], "hourly");
        assert!(
            recurring["run_at"].as_u64().unwrap() > smelt_core::task::now_secs(),
            "completed recurring task must move to a future occurrence"
        );

        // 清理临时目录
        std::fs::remove_dir_all(&tmp).ok();
        if let Some(h) = old_home {
            unsafe { std::env::set_var("HOME", h) };
        }
    }
}
