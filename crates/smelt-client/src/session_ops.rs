//! 客户端向运行时发的会话、自动化、菜单操作（守护协议的短请求）。调用方指定是哪个运行时。

use std::time::Duration;

/// 短请求的读写时限。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

use serde_json::{Value, json};
use smelt_model::agent_kind::HistorySourceKind;
use smelt_model::session_control::*;
use smelt_model::workspace_menu::WorkspaceMenuSnapshot;
use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::daemon::DaemonOperation;

/// 历史自定义标题的唯一写入口。smeltd 写 overlay 并同步远程目录。
pub fn rename_history_title(
    runtime: &RuntimeClient,
    kind: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
    title: Option<&str>,
    cwd: Option<&str>,
) -> Result<(), String> {
    let mut request = json!({
        "op": DaemonOperation::HistoryRename,
        "agent": kind.id(),
        "resume_id": resume_id,
    });
    if let Some(profile_id) = profile_id {
        request["profile_id"] = json!(profile_id);
    }
    match title {
        Some(title) => request["title"] = json!(title),
        None => request["title"] = Value::Null,
    }
    if let Some(cwd) = cwd {
        request["cwd"] = json!(cwd);
    }
    runtime.request_ok(&request, REQUEST_TIMEOUT)?;
    Ok(())
}

/// 桌面发布侧栏菜单。smeltd 是唯一持有者；远程客户端只消费 subscribe，不读工作区快照。
pub fn publish_workspace_menu(
    runtime: &RuntimeClient,
    menu: &WorkspaceMenuSnapshot,
) -> Result<(), String> {
    runtime.request_ok(
        &serde_json::json!({
            "op": DaemonOperation::WorkspaceMenu,
            "menu": menu,
        }),
        REQUEST_TIMEOUT,
    )?;
    Ok(())
}

pub fn submit_automation_command(
    runtime: &RuntimeClient,
    command: &smelt_model::automation::AutomationCommand,
) -> Result<
    (
        smelt_model::automation::AutomationCommandResult,
        smelt_model::automation::AutomationFile,
    ),
    String,
> {
    let response = runtime.request_ok(
        &serde_json::json!({
            "op": DaemonOperation::AutomationCommand,
            "command": command,
        }),
        REQUEST_TIMEOUT,
    )?;
    let result = response
        .get("result")
        .cloned()
        .ok_or_else(|| "smeltd returned no automation command result".to_string())
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
    let snapshot = response
        .get("automations")
        .cloned()
        .ok_or_else(|| "smeltd returned no automation snapshot".to_string())
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
    Ok((result, snapshot))
}

/// 列出侧栏「对话」栏里的产品对话。需要 smeltd。
pub fn list_agent_conversations(
    runtime: &RuntimeClient,
) -> Result<Vec<AgentConversationSummary>, String> {
    let response = runtime.request_ok(
        &serde_json::json!({
            "op": DaemonOperation::RemoteSessions,
        }),
        REQUEST_TIMEOUT,
    )?;
    let snapshot: RemoteSessionSnapshot = response
        .get("remote_sessions")
        .cloned()
        .ok_or_else(|| "smeltd 没有返回对话目录".to_string())
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
    let mut conversations = snapshot
        .sessions
        .into_iter()
        .filter(|session| session.kind == RemoteSessionKind::Acp && session.is_visible())
        .filter(|session| !is_background_acp_session_id(&session.id))
        .filter(|session| {
            is_agent_conversation(
                None,
                agent_definition_id_from_option_id(
                    session.agent_option_id.as_deref().unwrap_or(""),
                ),
                Some(&session.cwd),
            )
        })
        .map(|session| AgentConversationSummary {
            agent_definition_id: agent_definition_id_from_option_id(
                session.agent_option_id.as_deref().unwrap_or(""),
            )
            .map(str::to_string),
            session_id: session.id,
            title: session.title,
            cwd: session.cwd,
        })
        .collect::<Vec<_>>();
    conversations.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    Ok(conversations)
}

/// 重命名一场产品对话。目录、已保存的工作区标题和正在跑的 agent 会话名一起改。
pub fn rename_agent_conversation(
    runtime: &RuntimeClient,
    session_id: &str,
    title: &str,
) -> Result<AgentConversationSummary, String> {
    let session_id = session_id.trim();
    let title = title.trim();
    if session_id.is_empty() {
        return Err("对话 id 不能为空".into());
    }
    if title.is_empty() {
        return Err("对话标题不能为空".into());
    }
    let response = runtime.request_ok(
        &serde_json::json!({
            "op": DaemonOperation::ConversationRename,
            "id": session_id,
            "title": title,
        }),
        REQUEST_TIMEOUT,
    )?;
    serde_json::from_value(
        response
            .get("conversation")
            .cloned()
            .ok_or_else(|| "smeltd 没有返回改名结果".to_string())?,
    )
    .map_err(|error| error.to_string())
}

/// 提交一次交互式会话输入。路由由 smeltd 保存的 ConversationBinding 决定；
/// 调用方不能在失败后自行改走 ACP，否则远端请求结果不确定时会重复执行。
///
/// `config_values` 是用户刚点、agent 还没回包确认的会话配置（尤其是模型）。
/// 必须和正文走同一条 `acp_submit_input`：切模型走 acp_open action 流、发消息
/// 走这条短 RPC，两条通道没有顺序，空闲时一切就发会让 prompt 先开回合，
/// 模型切换被排到下一轮，看起来像没生效。
pub fn submit_conversation_input(
    runtime: &RuntimeClient,
    id: &str,
    input: &smelt_model::conversation::ConversationInput,
    config_values: &[(String, String)],
) -> Result<(), smelt_model::conversation::ConversationSubmitError> {
    // 超时后结果不明，调用方会恢复草稿并提示可能重复，但不会自动重发。
    let mut request = serde_json::json!({
        "op": DaemonOperation::AcpSubmitInput,
        "id": id,
        "input": input,
    });
    if !config_values.is_empty() {
        request["config_values"] = serde_json::to_value(config_values)
            .expect("config_values 是 String 对，序列化不会失败");
    }
    let response = runtime
        .request(&request, Duration::from_secs(35))
        .map_err(|error| {
            smelt_model::conversation::ConversationSubmitError::unknown(error.to_string())
        })?;
    parse_conversation_submit_response(response)
}
