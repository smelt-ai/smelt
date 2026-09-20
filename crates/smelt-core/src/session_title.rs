//! 侧栏、菜单、手机共用的会话展示名。
//!
//! 标题和「在不在跑」不是一回事：终端标题只用于展示，不能参与状态推断。
//! Copilot 的任务名没有状态装饰，仍然是合法标题。

const AUTO_TITLE_MAX_CHARS: usize = 36;

/// 从首条用户请求生成各端一致的本地兜底标题。它不是 provider 的显式命名：
/// agent 之后若通过 ACP/OSC 给出更准确的标题，调用方仍应让 provider 标题优先。
pub fn prompt_title(prompt: &str) -> Option<String> {
    let single_line = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.is_empty() {
        return None;
    }
    let mut chars = single_line.chars();
    let title: String = chars.by_ref().take(AUTO_TITLE_MAX_CHARS).collect();
    Some(if chars.next().is_some() {
        format!("{title}...")
    } else {
        title
    })
}

/// 用户手改名 > 终端原始标题 > 启动项显示名 > 兜底（cwd / 旧菜单名）。
///
/// OSC 标题的 payload 是不透明字符串：这里只判断是否为空，不猜 spinner、
/// shell prompt、产品名或 provider 自定义格式的语义，也不改写可见内容。
pub fn display_title(
    custom_title: Option<&str>,
    terminal_title: Option<&str>,
    launch_label: Option<&str>,
    fallback: Option<&str>,
) -> String {
    if let Some(custom) = nonempty(custom_title) {
        return custom.to_string();
    }
    if let Some(title) = terminal_title.filter(|title| !title.trim().is_empty()) {
        return title.to_string();
    }
    if let Some(label) = nonempty(launch_label) {
        return label.to_string();
    }
    nonempty(fallback)
        .map(str::to_string)
        .unwrap_or_else(|| "终端".to_string())
}

/// ACP 会话统一使用协议/本地生成的对话标题；尚无标题时才回退到 Agent + 项目。
/// 用户手改名由上层优先处理，不在这里混入持久化语义。
pub fn acp_display_title(
    conversation_title: Option<&str>,
    agent_label: &str,
    cwd: Option<&str>,
) -> String {
    if let Some(title) = nonempty(conversation_title) {
        return title.to_string();
    }
    let fallback = cwd
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .and_then(|cwd| cwd.trim_end_matches('/').rsplit('/').next())
        .filter(|dir| !dir.is_empty());
    match fallback {
        Some(dir) => format!("{agent_label} 对话 · {dir}"),
        None => format!("{agent_label} 对话"),
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_title_is_single_line_and_character_bounded() {
        assert_eq!(
            prompt_title("  修复 Codex 标题\n并保留用户 rename  ").as_deref(),
            Some("修复 Codex 标题 并保留用户 rename")
        );
        assert_eq!(
            prompt_title(&"标".repeat(40)),
            Some(format!("{}...", "标".repeat(36)))
        );
        assert_eq!(prompt_title(" \n\t "), None);
    }

    #[test]
    fn terminal_title_is_opaque_even_when_it_looks_like_a_project_spinner() {
        assert_eq!(
            display_title(None, Some("⠋ smelt"), Some("Codex"), Some("smelt")),
            "⠋ smelt"
        );
    }

    #[test]
    fn every_acp_provider_prefers_conversation_title_then_falls_back() {
        for kind in crate::agent_kind::ConversationAgentKind::ALL {
            let agent = kind.short_label();
            assert_eq!(
                acp_display_title(Some("修复侧栏标题"), agent, Some("/work/smelt")),
                "修复侧栏标题",
                "{agent} 不应被 Agent 类型特判挡住 ACP 标题"
            );
            assert_eq!(
                acp_display_title(None, agent, Some("/work/smelt")),
                format!("{agent} 对话 · smelt")
            );
        }
    }

    #[test]
    fn copilot_task_title_does_not_need_spinner() {
        assert_eq!(
            display_title(
                None,
                Some(
                    "Implement Nio Feature Request - Verifying frontend-only gates - GitHub Copilot"
                ),
                Some("GitHub Copilot"),
                Some("ve"),
            ),
            "Implement Nio Feature Request - Verifying frontend-only gates - GitHub Copilot"
        );
    }

    #[test]
    fn grok_spinner_title_is_preserved_verbatim() {
        assert_eq!(
            display_title(
                None,
                Some("⠼ - Preparing read_file... - Reviewing gates - grok"),
                Some("Grok"),
                Some("smelt"),
            ),
            "⠼ - Preparing read_file... - Reviewing gates - grok"
        );
    }

    #[test]
    fn claude_asterisk_title_is_preserved_verbatim() {
        assert_eq!(
            display_title(None, Some("✳ 审查权限门闩"), Some("Claude Code"), None),
            "✳ 审查权限门闩"
        );
    }

    #[test]
    fn shell_prompt_and_product_name_are_not_semantically_filtered() {
        assert_eq!(
            display_title(
                None,
                Some("c.chen@MBP:~/nio/smelt"),
                Some("GitHub Copilot"),
                Some("smelt"),
            ),
            "c.chen@MBP:~/nio/smelt"
        );
        assert_eq!(
            display_title(
                None,
                Some("GitHub Copilot"),
                Some("GitHub Copilot"),
                Some("ve")
            ),
            "GitHub Copilot"
        );
        assert_eq!(
            display_title(None, Some("zsh %"), None, Some("smelt")),
            "zsh %"
        );
    }

    #[test]
    fn custom_title_wins() {
        assert_eq!(
            display_title(
                Some("我改的名"),
                Some("Implement Nio Feature Request - GitHub Copilot"),
                Some("GitHub Copilot"),
                None,
            ),
            "我改的名"
        );
    }

    #[test]
    fn task_title_with_email_and_colon_is_not_a_shell_prompt() {
        assert_eq!(
            display_title(
                None,
                Some("Notify dev@example.com: deployment finished"),
                Some("GitHub Copilot"),
                None,
            ),
            "Notify dev@example.com: deployment finished"
        );
    }

    #[test]
    fn opencode_product_title_is_preserved() {
        assert_eq!(
            display_title(None, Some("OpenCode"), None, Some("smelt")),
            "OpenCode"
        );
    }
}
