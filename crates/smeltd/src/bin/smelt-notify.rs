//! Agent hook/扩展调用的小工具：把 Claude/Grok、Copilot、Codex、Antigravity、Cursor、
//! OpenCode、Kiro、Pi 的结构化事件翻译成 smeltd 的版本化事件，经 `agent_event` 上报。
//! 见 docs/notification-architecture.md。
//!
//! stdin: provider hook JSON（`hook_event_name` 或 Grok 的 `hookEventName`，
//!        以及 `tool_name` / `toolName`、`notification_type` / `message` 等）
//! env:   `SMELT_SESSION_ID`（smeltd 会话 id，spawn_session 时注入）、
//!        `SMELT_SOCK`（smeltd.sock 路径，同样是 spawn 时注入）
//! 出参:  连 `$SMELT_SOCK` 发一行 `{"op":"agent_event","id":"..","event":{...}}`
//!
//! **必须 exit 0，并返回各 provider 要求的无副作用响应**：Antigravity 的 Stop 明确
//! 要求 decision；Kiro 会把 command stdout 加进 agent 上下文，因此必须完全静默；
//! 其它 provider 返回 `{}`。非 0 退出码可能阻塞工具执行——这个工具只负责上报状态，
//! 绝不能意外干扰 agent 的正常运行。任何失败（socket 连不上、JSON 解析不出来、字段
//! 缺失、env 没设置）都静默退出。

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use smelt_core::agent_event::{AgentEvent, AgentEventKind};
use smelt_core::daemon_protocol::DaemonOperation;

fn main() {
    // 用户级 hooks 在 Smelt 外也会运行；即使下方因缺少 SMELT_* 环境变量而退出，
    // 仍要给 agent 一个合法、无副作用的 hook 响应。
    let provider = std::env::var("SMELT_HOOK_PROVIDER").unwrap_or_else(|_| "claude".into());
    let event = std::env::var("SMELT_HOOK_EVENT").unwrap_or_default();
    if let Some(response) = hook_response(&provider, &event) {
        println!("{response}");
    }
    let _ = run();
}

fn hook_response(provider: &str, event: &str) -> Option<&'static str> {
    if provider == "antigravity" && event == "Stop" {
        // 官方契约：只有 `continue` 会阻止停止，其它值均放行。显式返回非 continue，
        // 避免 `{}` 被新版 CLI 当成无效 Stop 响应。
        Some(r#"{"decision":"stop"}"#)
    } else if provider == "kiro" {
        // Kiro command hook 会捕获 stdout；空 JSON 对观察型 hook 没有价值，还可能
        // 被 PreToolUse / UserPromptSubmit 当成附加上下文，因此保持完全静默。
        None
    } else {
        Some("{}")
    }
}

fn run() -> Option<()> {
    // 没设这两个 env 说明不在 smelt 会话里跑（用户在别的终端直接用 Claude Code），
    // 静默退出——这不是错误，是正常情况。
    let session_id = std::env::var("SMELT_SESSION_ID").ok()?;
    let sock = std::env::var("SMELT_SOCK").ok()?;

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).ok()?;
    let hook: serde_json::Value = serde_json::from_str(&input).ok()?;

    let provider = std::env::var("SMELT_HOOK_PROVIDER").unwrap_or_else(|_| "claude".into());
    let event = normalize_hook_event_for_provider(&hook, &provider)?;

    let payload = serde_json::json!({
        "op": DaemonOperation::AgentEvent,
        "id": session_id,
        "event": &event,
    });
    // 一条只发一次，并等 daemon 确认 reducer 已提交。provider 的 hooks 是同步
    // 生命周期边沿；本 helper 退出后它才会继续下一步，因此确认把原始事件顺序
    // 延伸到 daemon。超时/断线也绝不重放，避免“结果未知”变成重复事件。
    let _ = send_once_and_confirm(&sock, &payload);
    Some(())
}

fn send_once_and_confirm(sock: &str, payload: &serde_json::Value) -> std::io::Result<bool> {
    let mut stream = UnixStream::connect(sock)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{payload}")?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    Ok(serde_json::from_str::<serde_json::Value>(&response)
        .ok()
        .and_then(|value| value["ok"].as_bool())
        == Some(true))
}

fn normalize_hook_event_name(event: &str) -> &str {
    match event {
        "session_start" => "SessionStart",
        "session_end" => "SessionEnd",
        "user_prompt_submit" => "UserPromptSubmit",
        "pre_tool_use" => "PreToolUse",
        "post_tool_use" => "PostToolUse",
        "post_tool_use_failure" => "PostToolUseFailure",
        "permission_denied" => "PermissionDenied",
        "stop_failure" => "StopFailure",
        "stop_cancelled" => "StopCancelled",
        "teammate_idle" => "TeammateIdle",
        "subagent_start" => "SubagentStart",
        "subagent_stop" | "subagent_end" => "SubagentStop",
        "pre_compact" => "PreCompact",
        "post_compact" => "PostCompact",
        _ => event,
    }
}

fn normalize_hook_event_for_provider(
    hook: &serde_json::Value,
    provider: &str,
) -> Option<AgentEvent> {
    // Grok 原生 payload 用 hookEventName + snake_case；其它兼容来源常用
    // hook_event_name。安装器注入的环境变量只作为缺少原生字段时的兜底。
    let event_from_env = std::env::var("SMELT_HOOK_EVENT").ok();
    let event = hook["hook_event_name"]
        .as_str()
        .or_else(|| hook["hookEventName"].as_str())
        .or(event_from_env.as_deref())
        .map(normalize_hook_event_name)?;
    let kind_and_message = match event {
        // 会话刚起、hooks 链路第一次有机会发声——不上报的话，Idle 态和「hooks
        // 根本没装/装的是旧配置/socket 连不上」在 UI 上长得一模一样；有这一条，
        // DaemonStates 里出现记录本身就是「链路确认通了」的信号。
        "SessionStart" | "sessionStart" => Some((AgentEventKind::SessionStarted, None)),
        "SessionTitleChanged" | "sessionTitleChanged" => {
            Some((AgentEventKind::SessionTitleChanged, None))
        }
        "UserPromptSubmit" | "userPromptSubmitted" | "userPromptSubmit" | "beforeSubmitPrompt" => {
            Some((AgentEventKind::PromptSubmitted, None))
        }
        // Antigravity 没有 UserPromptSubmit；每轮第一次/后续模型调用都由
        // PreInvocation 打开或维持 Running 状态。它不参与权限裁决。
        "PreInvocation" if provider == "antigravity" => {
            Some((AgentEventKind::PromptSubmitted, describe_invocation(hook)))
        }
        "PostInvocation" if provider == "antigravity" => {
            Some((AgentEventKind::ToolFinished, describe_invocation(hook)))
        }
        "PreToolUse" | "preToolUse" => {
            if let Some(tool) = hook_tool_name(hook) {
                let normalized = tool.to_ascii_lowercase();
                if matches!(
                    normalized.as_str(),
                    "askuser" | "ask_user" | "askuserquestion" | "request_user_input"
                ) {
                    let question = if normalized == "request_user_input" {
                        Some(describe_codex_user_input(hook))
                    } else {
                        describe_tool_call(hook)
                    };
                    return Some(build_agent_event(
                        hook,
                        provider,
                        AgentEventKind::InputRequested,
                        question,
                    ));
                }
            }
            Some((AgentEventKind::ToolStarted, describe_tool_call(hook)))
        }
        "PostToolUse" if provider == "antigravity" => {
            let error = hook["error"]
                .as_str()
                .map(str::trim)
                .filter(|error| !error.is_empty());
            let progress = describe_antigravity_progress(hook);
            if let Some(error) = error {
                let tool = hook_tool_name(hook).unwrap_or("工具");
                Some((
                    AgentEventKind::ToolFailed,
                    Some(format!("⚠ {tool} 执行失败：{}", truncate_chars(error, 60))),
                ))
            } else {
                Some((AgentEventKind::ToolFinished, progress))
            }
        }
        "PostToolUse" | "postToolUse" => Some((AgentEventKind::ToolFinished, None)),
        // 工具跑挂了：跟 PostToolUse 一样回到 thinking（agent 马上会看着错误决定
        // 下一步），但 question 带上失败标记——不然「刚才那个工具是不是炸了」在
        // UI 上完全看不出来，跟正常跑完长得一样。
        "PostToolUseFailure" | "postToolUseFailure" => {
            let tool = hook_tool_name(hook).unwrap_or("工具");
            let err = first_present_str(hook, &["error", "error_message", "tool_error", "reason"]);
            let q = match err {
                Some(e) => format!("⚠ {tool} 执行失败：{}", truncate_chars(e, 60)),
                None => format!("⚠ {tool} 执行失败"),
            };
            Some((AgentEventKind::ToolFailed, Some(q)))
        }
        // 独立的权限请求事件（比 Notification 更明确），见官方 hooks 文档。
        "PermissionRequest" | "permissionRequest" if provider != "copilot" => {
            let tool = hook_tool_name(hook).unwrap_or("");
            let q = format!("请求执行 {tool}");
            Some((AgentEventKind::ApprovalRequested, Some(q)))
        }
        // Copilot 在自身权限引擎作出 allow/deny/ask 决定前就发送此事件；只有后续
        // Notification(permission_prompt) 才证明用户真的看到了审批框。
        "PermissionRequest" | "permissionRequest" => None,
        "Notification" | "notification" => {
            // Notification 不都是"等审批"——notification_type 区分子类型，
            // 认不出来的子类型不改 phase（比如 auth_success 这种跟"要不要
            // 批准/输入"无关的通知）。
            let message = hook["message"].as_str().map(String::from);
            match first_present_str(hook, &["notification_type", "notificationType"]) {
                Some("permission_prompt") => Some((AgentEventKind::ApprovalRequested, message)),
                // Claude 的 idle_prompt 是「本轮已完成，等待下一条 prompt」，Grok
                // 也会用同一语义上报“Turn complete”。它不是一个待回答的问题；
                // 真正的交互式提问走 elicitation_dialog 或 AskUser 工具。
                Some(kind) if is_idle_notification(kind) => {
                    Some((AgentEventKind::TurnSucceeded, message))
                }
                Some("elicitation_dialog" | "elicitation_url_dialog") => {
                    Some((AgentEventKind::InputRequested, message))
                }
                // Copilot 后台 agent/shell 结束：不能当成主回合 Stop，否则主会话
                // 还在跑时会被提前画成空闲。
                Some("agent_idle" | "agent_completed") => {
                    Some((AgentEventKind::SubagentStopped, message))
                }
                _ => None,
            }
        }
        // agent 起了个子任务（比如 Task 工具）：这段时间之前完全是黑箱，只显示
        // 笼统的 executing_tool；agent_type 是官方 hooks 字段（"Explore" /
        // "security-reviewer" 这类），带上就知道具体在跑哪个子任务。
        "SubagentStart" | "subagentStart" => {
            let name = first_present_str(hook, &["agent_type", "subagent_type"]);
            let q = match name {
                Some(n) => format!("子任务：{n}"),
                None => "运行子任务".to_string(),
            };
            Some((AgentEventKind::SubagentStarted, Some(q)))
        }
        // 子任务做完，主 agent 回去汇总/继续思考。
        "SubagentStop" | "subagentStop" => Some((AgentEventKind::SubagentStopped, None)),
        "Stop" | "stop" if provider == "antigravity" => normalize_antigravity_stop(hook),
        "Stop" | "stop" if provider == "cursor" => normalize_cursor_stop(hook),
        "Stop" | "stop" | "agentStop" | "StopCancelled" | "SessionStop" | "sessionStop" => {
            Some((AgentEventKind::TurnSucceeded, None))
        }
        // 队友空闲 ≠ 主回合结束。当成子任务停下，主会话继续思考。
        "TeammateIdle" | "teammateIdle" => Some((AgentEventKind::SubagentStopped, None)),
        // 回合因 API 错误中断，跟正常「说完了等你」（Stop）语义不同，落到
        // failed；question 标出「出错」，detail_line 上能看出差别。
        "StopFailure" => {
            let reason = first_present_str(hook, &["error", "error_type", "reason"]);
            let q = match reason {
                Some(r) => format!("⚠ 因错误中断：{r}"),
                None => "⚠ 因错误中断".to_string(),
            };
            Some((AgentEventKind::TurnFailed, Some(q)))
        }
        "ErrorOccurred" | "errorOccurred" => {
            let reason = first_present_str(hook, &["message", "error", "reason"]);
            let message = Some(match reason {
                Some(reason) => format!("⚠ 因错误中断：{}", truncate_chars(reason, 60)),
                None => "⚠ 因错误中断".to_string(),
            });
            if hook["recoverable"].as_bool() == Some(true) {
                Some((AgentEventKind::ToolFailed, message))
            } else {
                Some((AgentEventKind::TurnFailed, message))
            }
        }
        "SessionEnd" | "sessionEnd" => Some((AgentEventKind::SessionEnded, None)),
        _ => None,
    }?;
    Some(build_agent_event(
        hook,
        provider,
        kind_and_message.0,
        kind_and_message.1,
    ))
}

fn build_agent_event(
    hook: &serde_json::Value,
    provider: &str,
    kind: AgentEventKind,
    message: Option<String>,
) -> AgentEvent {
    let mut event = AgentEvent::new(provider, kind);
    match kind {
        AgentEventKind::PromptSubmitted => {
            event.conversation_title = hook["prompt"]
                .as_str()
                .and_then(smelt_core::session_title::prompt_title);
        }
        AgentEventKind::SessionTitleChanged => {
            event.conversation_title =
                first_present_str(hook, &["title", "conversation_title", "session_title"])
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .map(String::from);
        }
        _ => {}
    }
    event.message = message;
    event.conversation_id = hook_conversation_id(hook);
    event.tool_name = hook_tool_name(hook).map(String::from);
    event.tool_use_id = first_present_str(hook, &["tool_use_id", "toolUseId"])
        .map(String::from)
        .or_else(|| hook["stepIdx"].as_u64().map(|step| step.to_string()));
    event.agent_id = first_present_str(
        hook,
        &[
            "agent_id",
            "agentId",
            "conversation_id",
            "conversationId",
            "session_id",
            "sessionID",
        ],
    )
    .map(String::from);
    event
}

/// provider 自己的对话 id。每条 hook 都带，绑定因此是自愈的：用户在 TUI 里
/// 退出、`/resume` 到别的对话后，下一条事件就会把绑定纠正过来。
///
/// 各家字段名不同，这里只做键名归一，不解析 id 的内部结构（对 smelt 而言它是
/// 不透明字符串，只用于和 provider 自己的历史存档对账）。
fn hook_conversation_id(hook: &serde_json::Value) -> Option<String> {
    first_present_str(
        hook,
        &[
            "session_id",
            "sessionId",
            "sessionID",
            "conversation_id",
            "conversationId",
        ],
    )
    .map(str::trim)
    .filter(|id| !id.is_empty())
    .map(String::from)
}

/// PreToolUse 的「当前工具」摘要：尽量带点路径/命令细节。
fn describe_tool_call(hook: &serde_json::Value) -> Option<String> {
    let tool = hook_tool_name(hook)?;
    let parsed_input;
    let input = if !hook["tool_input"].is_null() {
        &hook["tool_input"]
    } else if !hook
        .pointer("/toolCall/args")
        .is_none_or(|value| value.is_null())
    {
        hook.pointer("/toolCall/args")?
    } else if let Some(raw) = hook["toolArgs"].as_str() {
        parsed_input = serde_json::from_str(raw).unwrap_or(serde_json::Value::Null);
        &parsed_input
    } else {
        &hook["toolArgs"]
    };
    Some(
        if let Some(cmd) = input["command"]
            .as_str()
            .or_else(|| input["CommandLine"].as_str())
        {
            format!("Bash: {}", truncate_chars(cmd, 48))
        } else if let Some(p) = input["file_path"]
            .as_str()
            .or_else(|| input["path"].as_str())
            .or_else(|| input["FilePath"].as_str())
            .or_else(|| input["TargetFile"].as_str())
            .or_else(|| input["DirectoryPath"].as_str())
        {
            let name = p.rsplit('/').next().unwrap_or(p);
            format!("{tool}: {name}")
        } else {
            tool.to_string()
        },
    )
}

/// Codex TUI 的选择器/确认框走 `request_user_input` 本地工具。通知优先展示第一道
/// 问题本身；旧客户端若只带 header，或载荷不完整，也要给出可理解的固定提示。
fn describe_codex_user_input(hook: &serde_json::Value) -> String {
    hook.pointer("/tool_input/questions/0/question")
        .and_then(|value| value.as_str())
        .or_else(|| {
            hook.pointer("/tool_input/questions/0/header")
                .and_then(|value| value.as_str())
        })
        .filter(|text| !text.trim().is_empty())
        .map(|text| truncate_chars(text.trim(), 80))
        .unwrap_or_else(|| "Codex 等待你的输入".to_string())
}

fn hook_tool_name(hook: &serde_json::Value) -> Option<&str> {
    first_present_str(hook, &["tool_name", "toolName"]).or_else(|| {
        hook.pointer("/toolCall/name")
            .and_then(|value| value.as_str())
    })
}

fn describe_invocation(hook: &serde_json::Value) -> Option<String> {
    hook["invocationNum"]
        .as_u64()
        .map(|number| format!("第 {} 次模型调用", number + 1))
}

fn describe_antigravity_progress(hook: &serde_json::Value) -> Option<String> {
    let detail = describe_tool_call(hook)?;
    Some(match hook["stepIdx"].as_u64() {
        Some(step) => format!("第 {} 步 · {detail}", step + 1),
        None => detail,
    })
}

fn normalize_antigravity_stop(
    hook: &serde_json::Value,
) -> Option<(AgentEventKind, Option<String>)> {
    if hook["fullyIdle"].as_bool() == Some(false) {
        return Some((
            AgentEventKind::ToolFinished,
            Some("后台任务仍在运行".to_string()),
        ));
    }
    let error = hook["error"]
        .as_str()
        .map(str::trim)
        .filter(|error| !error.is_empty());
    let reason = hook["terminationReason"]
        .as_str()
        .map(str::trim)
        .filter(|reason| !reason.is_empty());
    let reason_is_failure = reason.is_some_and(|reason| {
        let reason = reason.to_ascii_lowercase();
        reason.contains("error")
            || reason.contains("fail")
            || reason.contains("max_steps")
            || reason.contains("cancel")
            || reason.contains("abort")
    });
    if error.is_some() || reason_is_failure {
        let detail = error.or(reason).unwrap_or("未知错误");
        Some((
            AgentEventKind::TurnFailed,
            Some(format!(
                "⚠ Antigravity 已停止：{}",
                truncate_chars(detail, 60)
            )),
        ))
    } else {
        Some((AgentEventKind::TurnSucceeded, None))
    }
}

fn normalize_cursor_stop(hook: &serde_json::Value) -> Option<(AgentEventKind, Option<String>)> {
    if hook["status"].as_str() != Some("error") {
        return Some((AgentEventKind::TurnSucceeded, None));
    }

    let detail = first_present_str(hook, &["error", "error_message", "message", "reason"])
        .map(|detail| format!("⚠ Cursor 回合失败：{}", truncate_chars(detail, 60)))
        .unwrap_or_else(|| "⚠ Cursor 回合失败".to_string());
    Some((AgentEventKind::TurnFailed, Some(detail)))
}

/// 按**字符**截断，不能按字节切：`&s[..n]` 是字节切片，第 n 字节一旦落在中文/
/// emoji 的多字节编码中间就会 panic（byte index is not a char boundary）。
fn truncate_chars(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &s[..end]),
        None => s.to_string(),
    }
}

/// 依次尝试几个可能的字段名，返回第一个存在的字符串——官方文档没有给出
/// `PostToolUseFailure`/`StopFailure` 的精确错误字段名，宽容读取：拿不到具体
/// 错误文案就退化成通用提示，但「失败」这个事实本身来自 hook_event_name 自己，
/// 不依赖猜中字段名，不会因为猜错而整条不上报。
fn first_present_str<'a>(hook: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| hook[*k].as_str())
}

fn is_idle_notification(kind: &str) -> bool {
    matches!(
        kind.trim().to_ascii_lowercase().replace(' ', "_").as_str(),
        "idle_prompt" | "idleprompt" | "idle" | "turn_complete" | "turncomplete"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// hook 事件 → (kind, message)：断言用的形状适配，包的是
    /// prod 的 `normalize_hook_event_for_provider`。
    fn map_hook_event(hook: &serde_json::Value) -> Option<(AgentEventKind, Option<String>)> {
        map_hook_event_for_provider(hook, "claude")
    }

    fn map_hook_event_for_provider(
        hook: &serde_json::Value,
        provider: &str,
    ) -> Option<(AgentEventKind, Option<String>)> {
        let event = normalize_hook_event_for_provider(hook, provider)?;
        Some((event.kind, event.message))
    }

    /// Copilot 的真实 hook 载荷：`sessionStart` 与 `userPromptSubmitted` 都带
    /// `sessionId`。对话身份必须能从任意一条事件里拿到——绑定靠每条事件自愈，
    /// 不依赖某一次会话开始的握手，用户在 TUI 里 `/resume` 到别的对话也能纠正。
    #[test]
    fn copilot_hooks_carry_the_conversation_id_on_every_event() {
        let started = json!({
            "hook_event_name": "SessionStart",
            "sessionId": "ffd68b82-46e6-4701-acfc-99dece7b9116",
            "cwd": "/work/smelt",
            "source": "new",
            "initialPrompt": "say ok",
        });
        assert_eq!(
            normalize_hook_event_for_provider(&started, "copilot")
                .unwrap()
                .conversation_id
                .as_deref(),
            Some("ffd68b82-46e6-4701-acfc-99dece7b9116")
        );

        let prompt = json!({
            "hook_event_name": "UserPromptSubmit",
            "sessionId": "0a23b00a-e06a-49b9-a5f4-f5e942687a00",
            "prompt": "继续",
        });
        assert_eq!(
            normalize_hook_event_for_provider(&prompt, "copilot")
                .unwrap()
                .conversation_id
                .as_deref(),
            Some("0a23b00a-e06a-49b9-a5f4-f5e942687a00")
        );
    }

    /// Claude 系用 snake_case；认不出 id 的 provider 保持 None，退化成纯终端
    /// 标题语义，绝不能瞎猜一个 id 出来（那会把改名写到别的对话上）。
    #[test]
    fn conversation_id_normalizes_key_names_and_stays_absent_when_unknown() {
        let claude = json!({ "hook_event_name": "PostToolUse", "session_id": "claude-1" });
        assert_eq!(
            normalize_hook_event_for_provider(&claude, "claude")
                .unwrap()
                .conversation_id
                .as_deref(),
            Some("claude-1")
        );

        let blank = json!({ "hook_event_name": "PostToolUse", "session_id": "   " });
        assert_eq!(
            normalize_hook_event_for_provider(&blank, "claude")
                .unwrap()
                .conversation_id,
            None
        );

        let missing = json!({ "hook_event_name": "PostToolUse" });
        assert_eq!(
            normalize_hook_event_for_provider(&missing, "codex")
                .unwrap()
                .conversation_id,
            None
        );
    }

    #[test]
    fn user_prompt_submit_maps_to_thinking() {
        let hook = json!({ "hook_event_name": "UserPromptSubmit" });
        assert_eq!(
            map_hook_event(&hook),
            Some((AgentEventKind::PromptSubmitted, None))
        );
    }

    #[test]
    fn codex_user_prompt_submit_carries_a_conversation_title() {
        let hook = json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "  修复 Codex 标题\n并保留用户 rename  "
        });
        let event = normalize_hook_event_for_provider(&hook, "codex").unwrap();
        assert_eq!(
            event.conversation_title.as_deref(),
            Some("修复 Codex 标题 并保留用户 rename")
        );
        assert_eq!(event.message, None, "prompt 标题不应冒充 pending question");
    }

    #[test]
    fn pi_extension_events_use_the_existing_structured_lifecycle() {
        let prompt = normalize_hook_event_for_provider(
            &json!({
                "hook_event_name": "UserPromptSubmit",
                "prompt": "修复 Pi 侧栏状态"
            }),
            "pi",
        )
        .unwrap();
        assert_eq!(prompt.provider, "pi");
        assert_eq!(prompt.kind, AgentEventKind::PromptSubmitted);
        assert_eq!(
            prompt.conversation_title.as_deref(),
            Some("修复 Pi 侧栏状态")
        );
        let tool = normalize_hook_event_for_provider(
            &json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "bash",
                "tool_use_id": "pi-tool-1",
                "tool_input": { "command": "cargo test" }
            }),
            "pi",
        )
        .unwrap();
        assert_eq!(tool.kind, AgentEventKind::ToolStarted);
        assert_eq!(tool.tool_use_id.as_deref(), Some("pi-tool-1"));
        assert_eq!(tool.message.as_deref(), Some("Bash: cargo test"));
        let settled =
            normalize_hook_event_for_provider(&json!({ "hook_event_name": "Stop" }), "pi").unwrap();
        assert_eq!(settled.kind, AgentEventKind::TurnSucceeded);
        let failed = normalize_hook_event_for_provider(
            &json!({
                "hook_event_name": "StopFailure",
                "error_type": "length"
            }),
            "pi",
        )
        .unwrap();
        assert_eq!(failed.kind, AgentEventKind::TurnFailed);
    }

    #[test]
    fn explicit_session_title_becomes_a_phase_neutral_title_event() {
        let event = normalize_hook_event_for_provider(
            &json!({
                "hook_event_name": "SessionTitleChanged",
                "title": "  修复 OpenCode 分屏标题  "
            }),
            "opencode",
        )
        .expect("OpenCode 标题事件必须可归一化");

        assert_eq!(event.kind, AgentEventKind::SessionTitleChanged);
        assert_eq!(
            event.conversation_title.as_deref(),
            Some("修复 OpenCode 分屏标题")
        );
    }

    #[test]
    fn pre_tool_use_carries_tool_name_as_question() {
        let hook = json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash" });
        assert_eq!(
            map_hook_event(&hook),
            Some((AgentEventKind::ToolStarted, Some("Bash".to_string())))
        );
    }

    /// 命令摘要按**字符**截断，不能按字节切：`&cmd[..48]` 遇到第 48 字节落在多字节
    /// 字符中间会直接 panic（"byte index is not a char boundary"），把整个 hook 打挂
    /// ——中文命令一跑就中招，实际见过。
    #[test]
    fn long_cjk_command_does_not_panic_on_char_boundary() {
        // 第 48 字节落在「图」的三字节编码中间
        let cmd = "echo \"=== 卷内容（应见中文软链 + 卷图标）===\"; ls -la /tmp/x; echo done";
        assert!(
            !cmd.is_char_boundary(48),
            "用例前提：第 48 字节须落在字符中间"
        );
        assert!(cmd.chars().count() > 48, "用例前提：字符数须超过截断阈值");

        let hook = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": cmd },
        });
        let (kind, q) = map_hook_event(&hook).unwrap();
        assert_eq!(kind, AgentEventKind::ToolStarted);
        let q = q.expect("应带命令摘要");
        assert!(q.starts_with("Bash: echo"), "摘要应保留命令开头：{q}");
        assert!(q.ends_with('…'), "超长应截断并加省略号：{q}");
        assert_eq!(
            q.chars().count(),
            "Bash: ".chars().count() + 48 + 1,
            "截断按字符算：前缀 + 48 字符 + 省略号"
        );
    }

    /// 阈值按字符算而非字节：中文命令字节数很容易翻三倍越过阈值，但字符数并不多。
    /// 旧的 `cmd.len() > 48`（字节）会把这种并不长的命令误判成超长、砍掉尾巴。
    #[test]
    fn cjk_command_within_char_limit_is_not_truncated() {
        let cmd = "echo \"=== 卷内容（应见中文软链 + 卷图标）===\"; ls -la /tmp/x";
        assert!(cmd.len() > 48, "用例前提：字节数须超阈值");
        assert!(cmd.chars().count() <= 48, "用例前提：字符数须未超阈值");

        let hook = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": cmd },
        });
        let (_, q) = map_hook_event(&hook).unwrap();
        assert_eq!(
            q.expect("应带命令摘要"),
            format!("Bash: {cmd}"),
            "字符数没超阈值就不该截断"
        );
    }

    #[test]
    fn permission_request_builds_a_question_from_tool_name() {
        let hook = json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash" });
        let (kind, q) = map_hook_event(&hook).unwrap();
        assert_eq!(kind, AgentEventKind::ApprovalRequested);
        assert_eq!(q.as_deref(), Some("请求执行 Bash"));
    }

    #[test]
    fn copilot_permission_request_is_not_yet_user_visible() {
        let hook = json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash" });
        assert_eq!(map_hook_event_for_provider(&hook, "copilot"), None);
    }

    #[test]
    fn codex_permission_request_is_user_visible() {
        let hook = json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash" });
        assert_eq!(
            map_hook_event_for_provider(&hook, "codex"),
            Some((
                AgentEventKind::ApprovalRequested,
                Some("请求执行 Bash".to_string())
            ))
        );
    }

    #[test]
    fn ask_user_tool_waits_for_input() {
        let hook = json!({ "hook_event_name": "PreToolUse", "tool_name": "ask_user" });
        assert_eq!(
            map_hook_event_for_provider(&hook, "copilot"),
            Some((AgentEventKind::InputRequested, Some("ask_user".to_string())))
        );
    }

    #[test]
    fn codex_request_user_input_waits_with_question_summary() {
        let hook = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "request_user_input",
            "tool_input": {
                "questions": [{
                    "header": "选择水果",
                    "question": "你想选哪一种水果？"
                }]
            }
        });
        assert_eq!(
            map_hook_event_for_provider(&hook, "codex"),
            Some((
                AgentEventKind::InputRequested,
                Some("你想选哪一种水果？".to_string())
            ))
        );
        let event = normalize_hook_event_for_provider(&hook, "codex").unwrap();
        assert_eq!(event.version, 1);
        assert_eq!(event.kind, AgentEventKind::InputRequested);
        assert_eq!(event.tool_name.as_deref(), Some("request_user_input"));
    }

    #[test]
    fn codex_request_user_input_summary_falls_back_cleanly() {
        let header_only = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "request_user_input",
            "tool_input": { "questions": [{ "header": "确认操作" }] }
        });
        assert_eq!(
            map_hook_event_for_provider(&header_only, "codex"),
            Some((AgentEventKind::InputRequested, Some("确认操作".to_string())))
        );

        let missing = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "request_user_input",
            "tool_input": {}
        });
        assert_eq!(
            map_hook_event_for_provider(&missing, "codex"),
            Some((
                AgentEventKind::InputRequested,
                Some("Codex 等待你的输入".to_string())
            ))
        );
    }

    #[test]
    fn codex_request_user_input_post_tool_use_resumes_thinking() {
        let hook = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "request_user_input"
        });
        assert_eq!(
            map_hook_event_for_provider(&hook, "codex"),
            Some((AgentEventKind::ToolFinished, None))
        );
    }

    #[test]
    fn copilot_native_ask_user_payload_waits_for_input() {
        let hook = json!({
            "hook_event_name": "preToolUse",
            "toolName": "ask_user",
            "toolArgs": "{\"question\":\"选择通知场景\"}"
        });
        assert_eq!(
            map_hook_event_for_provider(&hook, "copilot"),
            Some((AgentEventKind::InputRequested, Some("ask_user".to_string())))
        );
    }

    #[test]
    fn copilot_native_tool_payload_builds_summary() {
        let hook = json!({
            "toolName": "bash",
            "toolArgs": "{\"command\":\"git status\"}"
        });
        assert_eq!(
            describe_tool_call(&hook).as_deref(),
            Some("Bash: git status")
        );
    }

    #[test]
    fn copilot_native_notification_type_waits_for_input() {
        let hook = json!({
            "hook_event_name": "notification",
            "notificationType": "elicitation_dialog",
            "message": "请选择"
        });
        assert_eq!(
            map_hook_event_for_provider(&hook, "copilot"),
            Some((AgentEventKind::InputRequested, Some("请选择".to_string())))
        );
    }

    #[test]
    fn claude_elicitation_url_dialog_maps_to_waiting_for_user() {
        let hook = json!({
            "hook_event_name": "Notification",
            "notification_type": "elicitation_url_dialog",
            "message": "请在浏览器中完成授权"
        });
        assert_eq!(
            map_hook_event(&hook),
            Some((
                AgentEventKind::InputRequested,
                Some("请在浏览器中完成授权".to_string())
            ))
        );
    }

    #[test]
    fn notification_permission_prompt_maps_to_awaiting_approval() {
        let hook = json!({
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": "需要批准执行 rm 命令",
        });
        assert_eq!(
            map_hook_event(&hook),
            Some((
                AgentEventKind::ApprovalRequested,
                Some("需要批准执行 rm 命令".to_string())
            ))
        );
    }

    #[test]
    fn notification_idle_prompt_maps_to_turn_succeeded() {
        let hook = json!({
            "hook_event_name": "Notification",
            "notification_type": "idle_prompt",
            "message": "Turn complete",
        });
        assert_eq!(
            map_hook_event(&hook),
            Some((
                AgentEventKind::TurnSucceeded,
                Some("Turn complete".to_string())
            ))
        );
        assert_eq!(
            map_hook_event_for_provider(&hook, "grok"),
            Some((
                AgentEventKind::TurnSucceeded,
                Some("Turn complete".to_string())
            ))
        );
    }

    #[test]
    fn grok_native_hook_payload_uses_its_camel_case_snake_case_event_name() {
        let prompt = normalize_hook_event_for_provider(
            &json!({
                "hookEventName": "user_prompt_submit",
                "sessionId": "grok-session",
            }),
            "grok",
        )
        .unwrap();
        assert_eq!(prompt.kind, AgentEventKind::PromptSubmitted);

        let tool = normalize_hook_event_for_provider(
            &json!({
                "hookEventName": "pre_tool_use",
                "toolName": "run_terminal_command",
                "toolInput": { "command": "cargo test" },
            }),
            "grok",
        )
        .unwrap();
        assert_eq!(tool.kind, AgentEventKind::ToolStarted);
        assert_eq!(tool.tool_name.as_deref(), Some("run_terminal_command"));

        let stop =
            normalize_hook_event_for_provider(&json!({ "hookEventName": "stop" }), "grok").unwrap();
        assert_eq!(stop.kind, AgentEventKind::TurnSucceeded);
    }

    #[test]
    fn grok_stop_cancelled_ends_the_turn_but_teammate_idle_does_not() {
        let cancelled = normalize_hook_event_for_provider(
            &json!({ "hookEventName": "stop_cancelled" }),
            "grok",
        )
        .unwrap();
        assert_eq!(cancelled.kind, AgentEventKind::TurnSucceeded);

        let teammate = normalize_hook_event_for_provider(
            &json!({ "hook_event_name": "TeammateIdle" }),
            "claude",
        )
        .unwrap();
        assert_eq!(
            teammate.kind,
            AgentEventKind::SubagentStopped,
            "TeammateIdle 是队友停下，不能把主回合标成结束"
        );
    }

    #[test]
    fn grok_idle_notification_aliases_end_the_turn() {
        for hook in [
            json!({
                "hookEventName": "notification",
                "notificationType": "turn_complete",
                "message": "Turn complete",
            }),
            json!({
                "hookEventName": "notification",
                "notification_type": "idle",
            }),
        ] {
            let event = normalize_hook_event_for_provider(&hook, "grok").unwrap();
            assert_eq!(event.kind, AgentEventKind::TurnSucceeded, "{hook}");
        }
    }

    #[test]
    fn kiro_camel_case_prompt_submit_opens_a_turn() {
        let event = normalize_hook_event_for_provider(
            &json!({ "hook_event_name": "userPromptSubmit" }),
            "kiro",
        )
        .unwrap();
        assert_eq!(event.kind, AgentEventKind::PromptSubmitted);
    }

    #[test]
    fn codex_session_stop_ends_the_turn() {
        let event = normalize_hook_event_for_provider(
            &json!({ "hook_event_name": "SessionStop" }),
            "codex",
        )
        .unwrap();
        assert_eq!(event.kind, AgentEventKind::TurnSucceeded);
    }

    #[test]
    fn copilot_background_agent_idle_does_not_end_the_main_turn() {
        let event = normalize_hook_event_for_provider(
            &json!({
                "hook_event_name": "notification",
                "notification_type": "agent_idle",
            }),
            "copilot",
        )
        .unwrap();
        assert_eq!(event.kind, AgentEventKind::SubagentStopped);
    }

    #[test]
    fn copilot_elicitation_dialog_maps_to_waiting_for_user() {
        let hook = json!({
            "hook_event_name": "Notification",
            "notification_type": "elicitation_dialog",
            "message": "请选择部署环境",
        });
        assert_eq!(
            map_hook_event_for_provider(&hook, "copilot"),
            Some((
                AgentEventKind::InputRequested,
                Some("请选择部署环境".to_string())
            ))
        );
    }

    /// 认不出的 notification_type（比如 auth_success）不该反过来乱猜 phase。
    #[test]
    fn notification_unknown_subtype_is_ignored() {
        let hook = json!({
            "hook_event_name": "Notification",
            "notification_type": "auth_success",
            "message": "已登录",
        });
        assert_eq!(map_hook_event(&hook), None);
    }

    #[test]
    fn stop_maps_to_succeeded() {
        let hook = json!({ "hook_event_name": "Stop" });
        assert_eq!(
            map_hook_event(&hook),
            Some((AgentEventKind::TurnSucceeded, None))
        );
    }

    #[test]
    fn antigravity_stop_response_never_forces_another_loop() {
        assert_eq!(
            hook_response("antigravity", "Stop"),
            Some(r#"{"decision":"stop"}"#)
        );
        assert_eq!(hook_response("antigravity", "PostToolUse"), Some("{}"));
        assert_eq!(hook_response("claude", "Stop"), Some("{}"));
    }

    #[test]
    fn kiro_hook_response_is_silent() {
        assert_eq!(hook_response("kiro", "PreToolUse"), None);
        assert_eq!(hook_response("kiro", "Stop"), None);
    }

    #[test]
    fn cursor_prompt_and_stop_aliases_map_to_turn_edges() {
        assert_eq!(
            map_hook_event_for_provider(
                &json!({ "hook_event_name": "beforeSubmitPrompt" }),
                "cursor"
            ),
            Some((AgentEventKind::PromptSubmitted, None))
        );
        assert_eq!(
            map_hook_event_for_provider(&json!({ "hook_event_name": "stop" }), "cursor"),
            Some((AgentEventKind::TurnSucceeded, None))
        );
    }

    #[test]
    fn cursor_stop_status_only_marks_error_as_failed() {
        let hook = json!({
            "hook_event_name": "stop",
            "status": "error"
        });
        let event = normalize_hook_event_for_provider(&hook, "cursor").unwrap();
        assert_eq!(event.kind, AgentEventKind::TurnFailed);
        for status in ["completed", "aborted"] {
            let event = normalize_hook_event_for_provider(
                &json!({ "hook_event_name": "stop", "status": status }),
                "cursor",
            )
            .unwrap();
            assert_eq!(event.kind, AgentEventKind::TurnSucceeded, "status={status}");
        }

        let pascal_error = normalize_hook_event_for_provider(
            &json!({
                "hook_event_name": "Stop",
                "status": "error",
                "error": "model timeout"
            }),
            "cursor",
        )
        .unwrap();
        assert_eq!(pascal_error.kind, AgentEventKind::TurnFailed);
    }

    #[test]
    fn kiro_v3_runtime_events_use_pascal_case() {
        let started = normalize_hook_event_for_provider(
            &json!({ "hook_event_name": "SessionStart" }),
            "kiro",
        )
        .unwrap();
        assert_eq!(started.kind, AgentEventKind::SessionStarted);
        assert_eq!(
            map_hook_event_for_provider(&json!({ "hook_event_name": "UserPromptSubmit" }), "kiro"),
            Some((AgentEventKind::PromptSubmitted, None))
        );
        assert_eq!(
            map_hook_event_for_provider(&json!({ "hook_event_name": "agentSpawn" }), "kiro"),
            None
        );
        // 配置文件 trigger 是 PascalCase；CLI stdin 仍可能发 camelCase。
        assert_eq!(
            map_hook_event_for_provider(&json!({ "hook_event_name": "userPromptSubmit" }), "kiro"),
            Some((AgentEventKind::PromptSubmitted, None))
        );
    }

    #[test]
    fn cursor_ids_and_subagent_type_are_preserved() {
        let hook = json!({
            "hook_event_name": "subagentStart",
            "conversation_id": "cursor-conversation",
            "subagent_type": "explore"
        });
        let event = normalize_hook_event_for_provider(&hook, "cursor").unwrap();
        assert_eq!(event.kind, AgentEventKind::SubagentStarted);
        assert_eq!(event.agent_id.as_deref(), Some("cursor-conversation"));
        assert_eq!(event.message.as_deref(), Some("子任务：explore"));
    }

    #[test]
    fn antigravity_invocation_opens_a_new_running_turn() {
        let hook = json!({
            "hook_event_name": "PreInvocation",
            "invocationNum": 0,
            "conversationId": "conversation-1"
        });
        let event = normalize_hook_event_for_provider(&hook, "antigravity").unwrap();
        assert_eq!(event.kind, AgentEventKind::PromptSubmitted);
        assert_eq!(event.message.as_deref(), Some("第 1 次模型调用"));
        assert_eq!(event.agent_id.as_deref(), Some("conversation-1"));
    }

    #[test]
    fn antigravity_post_tool_reports_nested_tool_and_step() {
        let hook = json!({
            "hook_event_name": "PostToolUse",
            "toolCall": {
                "name": "run_command",
                "args": { "CommandLine": "cargo test" }
            },
            "stepIdx": 4,
            "error": "",
            "conversationId": "conversation-2"
        });
        let event = normalize_hook_event_for_provider(&hook, "antigravity").unwrap();
        assert_eq!(event.kind, AgentEventKind::ToolFinished);
        assert_eq!(event.tool_name.as_deref(), Some("run_command"));
        assert_eq!(event.tool_use_id.as_deref(), Some("4"));
        assert_eq!(event.agent_id.as_deref(), Some("conversation-2"));
        assert_eq!(event.message.as_deref(), Some("第 5 步 · Bash: cargo test"));
    }

    #[test]
    fn antigravity_post_tool_error_remains_non_terminal_progress() {
        let hook = json!({
            "hook_event_name": "PostToolUse",
            "toolCall": { "name": "run_command", "args": {} },
            "stepIdx": 2,
            "error": "exit status 1"
        });
        assert_eq!(
            map_hook_event_for_provider(&hook, "antigravity"),
            Some((
                AgentEventKind::ToolFailed,
                Some("⚠ run_command 执行失败：exit status 1".to_string())
            ))
        );
    }

    #[test]
    fn antigravity_stop_distinguishes_idle_background_and_failure() {
        assert_eq!(
            map_hook_event_for_provider(
                &json!({
                    "hook_event_name": "Stop",
                    "fullyIdle": true,
                    "terminationReason": "model_stop",
                    "error": ""
                }),
                "antigravity"
            ),
            Some((AgentEventKind::TurnSucceeded, None))
        );
        assert_eq!(
            map_hook_event_for_provider(
                &json!({
                    "hook_event_name": "Stop",
                    "fullyIdle": false,
                    "terminationReason": "model_stop"
                }),
                "antigravity"
            ),
            Some((
                AgentEventKind::ToolFinished,
                Some("后台任务仍在运行".to_string())
            ))
        );
        assert_eq!(
            map_hook_event_for_provider(
                &json!({
                    "hook_event_name": "Stop",
                    "fullyIdle": true,
                    "terminationReason": "error",
                    "error": "rate limited"
                }),
                "antigravity"
            ),
            Some((
                AgentEventKind::TurnFailed,
                Some("⚠ Antigravity 已停止：rate limited".to_string())
            ))
        );
        assert_eq!(
            map_hook_event_for_provider(
                &json!({
                    "hook_event_name": "stop",
                    "fullyIdle": false,
                    "terminationReason": "model_stop"
                }),
                "antigravity"
            ),
            Some((
                AgentEventKind::ToolFinished,
                Some("后台任务仍在运行".to_string())
            )),
            "Antigravity 小写 stop 也必须看 fullyIdle"
        );
    }

    #[test]
    fn copilot_config_event_aliases_map_to_the_same_phases() {
        assert_eq!(
            map_hook_event_for_provider(
                &json!({ "hook_event_name": "userPromptSubmitted" }),
                "copilot"
            ),
            Some((AgentEventKind::PromptSubmitted, None))
        );
        assert_eq!(
            map_hook_event_for_provider(&json!({ "hook_event_name": "agentStop" }), "copilot"),
            Some((AgentEventKind::TurnSucceeded, None))
        );
        assert_eq!(
            map_hook_event_for_provider(&json!({ "hook_event_name": "sessionEnd" }), "copilot"),
            Some((AgentEventKind::SessionEnded, None))
        );
    }

    #[test]
    fn session_end_maps_to_dead() {
        let hook = json!({ "hook_event_name": "SessionEnd" });
        assert_eq!(
            map_hook_event(&hook),
            Some((AgentEventKind::SessionEnded, None))
        );
    }

    #[test]
    fn unknown_event_name_is_ignored() {
        let hook = json!({ "hook_event_name": "SomethingWeirdFromAFutureVersion" });
        assert_eq!(map_hook_event(&hook), None);
    }

    #[test]
    fn missing_hook_event_name_is_ignored() {
        assert_eq!(map_hook_event(&json!({})), None);
    }

    #[test]
    fn session_start_is_phase_neutral_metadata() {
        let event = normalize_hook_event_for_provider(
            &json!({ "hook_event_name": "SessionStart" }),
            "claude",
        )
        .unwrap();
        assert_eq!(event.kind, AgentEventKind::SessionStarted);
        assert_eq!(
            event.kind.occupancy(),
            smelt_core::agent_event::Occupancy::Metadata
        );
    }

    #[test]
    fn subagent_start_carries_agent_type() {
        let hook = json!({ "hook_event_name": "SubagentStart", "agent_type": "Explore" });
        assert_eq!(
            map_hook_event(&hook),
            Some((
                AgentEventKind::SubagentStarted,
                Some("子任务：Explore".to_string())
            ))
        );
    }

    #[test]
    fn copilot_camel_case_subagent_start_is_supported() {
        let hook = json!({ "hook_event_name": "subagentStart", "agent_type": "Explore" });
        assert_eq!(
            map_hook_event_for_provider(&hook, "copilot"),
            Some((
                AgentEventKind::SubagentStarted,
                Some("子任务：Explore".to_string())
            ))
        );
    }

    #[test]
    fn copilot_error_event_is_distinct_from_success() {
        let hook = json!({ "hook_event_name": "ErrorOccurred", "message": "rate limited" });
        assert_eq!(
            map_hook_event_for_provider(&hook, "copilot"),
            Some((
                AgentEventKind::TurnFailed,
                Some("⚠ 因错误中断：rate limited".to_string())
            ))
        );
    }

    #[test]
    fn copilot_recoverable_error_keeps_the_turn_working() {
        let hook = json!({
            "hook_event_name": "errorOccurred",
            "message": "temporary network error",
            "recoverable": true
        });
        let event = normalize_hook_event_for_provider(&hook, "copilot").unwrap();
        assert_eq!(event.kind, AgentEventKind::ToolFailed);
        assert!(
            event
                .message
                .as_deref()
                .unwrap()
                .contains("temporary network error")
        );
    }

    #[test]
    fn subagent_start_without_agent_type_falls_back() {
        let hook = json!({ "hook_event_name": "SubagentStart" });
        assert_eq!(
            map_hook_event(&hook),
            Some((
                AgentEventKind::SubagentStarted,
                Some("运行子任务".to_string())
            ))
        );
    }

    #[test]
    fn subagent_stop_maps_to_thinking() {
        let hook = json!({ "hook_event_name": "SubagentStop" });
        assert_eq!(
            map_hook_event(&hook),
            Some((AgentEventKind::SubagentStopped, None))
        );
    }

    #[test]
    fn post_tool_use_failure_carries_error_when_present() {
        let hook = json!({
            "hook_event_name": "PostToolUseFailure",
            "tool_name": "Bash",
            "error": "command not found",
        });
        let (kind, q) = map_hook_event(&hook).unwrap();
        assert_eq!(kind, AgentEventKind::ToolFailed);
        assert_eq!(q.as_deref(), Some("⚠ Bash 执行失败：command not found"));
    }

    /// 官方字段名没有精确文档，宽容读取：这里故意不给 `error`，只给
    /// `tool_error`，验证 first_present_str 会往下试。
    #[test]
    fn post_tool_use_failure_falls_back_to_alternate_field_name() {
        let hook = json!({
            "hook_event_name": "PostToolUseFailure",
            "tool_name": "Write",
            "tool_error": "permission denied",
        });
        let (_, q) = map_hook_event(&hook).unwrap();
        assert_eq!(q.as_deref(), Some("⚠ Write 执行失败：permission denied"));
    }

    /// 一个错误字段都拿不到也不能整条不上报——「失败」这个事实来自
    /// hook_event_name 本身，不依赖猜中字段名。
    #[test]
    fn post_tool_use_failure_without_any_known_field_still_reports() {
        let hook = json!({ "hook_event_name": "PostToolUseFailure", "tool_name": "Bash" });
        let (kind, q) = map_hook_event(&hook).unwrap();
        assert_eq!(kind, AgentEventKind::ToolFailed);
        assert_eq!(q.as_deref(), Some("⚠ Bash 执行失败"));
    }

    #[test]
    fn stop_failure_differs_from_plain_stop() {
        let hook = json!({ "hook_event_name": "StopFailure", "error_type": "rate_limit" });
        let (kind, q) = map_hook_event(&hook).unwrap();
        assert_eq!(kind, AgentEventKind::TurnFailed);
        assert_eq!(q.as_deref(), Some("⚠ 因错误中断：rate_limit"));
        // 失败与正常 Stop 必须是不同 phase，不能只靠文案区分。
        assert_ne!(
            map_hook_event(&hook),
            map_hook_event(&json!({ "hook_event_name": "Stop" }))
        );
    }

    #[test]
    fn stop_failure_without_reason_still_flags_error() {
        let hook = json!({ "hook_event_name": "StopFailure" });
        let (_, q) = map_hook_event(&hook).unwrap();
        assert_eq!(q.as_deref(), Some("⚠ 因错误中断"));
    }
}
