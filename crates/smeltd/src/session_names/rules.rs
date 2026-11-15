//! 显示名的拼法：用户改的名、会话名、启动项、目录按什么次序取。只有运行时算名字用
//! （见上层 [`super`]），客户端只读推来的结果。
//!
//! 调用方传入的会话名已经决定好（提供方标题或首条请求兜底）；这里只判断是否为空，不识别、
//! 不改写内容。

/// 用户手改名 > 已经决定的会话名 > 启动项显示名 > 兜底（cwd / 旧菜单名）。
///
/// `session_title` 是调用方已经决定的会话名（provider 标题或首条 prompt 兜底），
/// 不是 OSC 原文。这里只判断是否为空，不改写可见内容。
pub(super) fn display_title(
    custom_title: Option<&str>,
    session_title: Option<&str>,
    launch_label: Option<&str>,
    fallback: Option<&str>,
) -> String {
    if let Some(custom) = nonempty(custom_title) {
        return custom.to_string();
    }
    if let Some(title) = session_title.filter(|title| !title.trim().is_empty()) {
        return title.to_string();
    }
    if let Some(label) = nonempty(launch_label) {
        return label.to_string();
    }
    nonempty(fallback)
        .map(str::to_string)
        .unwrap_or_else(|| "终端".to_string())
}

/// 终端会话（一段对话里的一组终端）显示名的来源。名字取自一格「代表格」：第一格，
/// 各端看到的是同一个名字，不跟着哪格是活动的变。
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TerminalTitle<'a> {
    /// 代表格里那段 agent 对话的用户命名（历史页 / 侧栏改名写的同一份，运行时随会话
    /// 状态推来）。
    pub(super) conversation_custom: Option<&'a str>,
    /// 用户给整段会话起的名。
    pub(super) session_custom: Option<&'a str>,
    /// 这段会话里有几格终端。
    pub(super) pane_count: usize,
    /// 用户给代表格起的名。
    pub(super) pane_custom: Option<&'a str>,
    /// 代表格已经决定的会话名（provider 标题或首条 prompt 兜底）。
    pub(super) session_title: Option<&'a str>,
    pub(super) launch_label: Option<&'a str>,
    /// 都没有时的兜底（终端标题栏、目录名）。
    pub(super) fallback: Option<&'a str>,
}

impl TerminalTitle<'_> {
    /// 「当前对话的用户命名」排在最前：终端里的 agent 换一段对话，名字就该跟着换，
    /// 而不是继续顶着上一段对话的标题。会话级 / 格子级的名字只服务于没有对话身份的
    /// 终端（裸 shell、hook 不带 id 的 provider）；分屏时格子名不能反过来改掉会话名。
    pub(super) fn display(&self) -> String {
        let pinned = nonempty(self.conversation_custom)
            .or_else(|| nonempty(self.session_custom))
            .or_else(|| {
                (self.pane_count == 1)
                    .then(|| nonempty(self.pane_custom))
                    .flatten()
            });
        display_title(pinned, self.session_title, self.launch_label, self.fallback)
    }
}

/// 一段 ACP 对话的名字：用户改的名 > 会话名 > 自定义智能体自己的名字 > 「智能体 对话 · 目录」。
///
/// `session_name` 是运行时已经决定的会话名（提供方标题或首条请求兜底）。`dir` 是工作目录
/// 的末段，按运行时这台机器的系统切好。
pub(super) fn acp_conversation_name(
    custom_title: Option<&str>,
    session_name: Option<&str>,
    definition_name: Option<&str>,
    agent_label: &str,
    dir: Option<&str>,
) -> String {
    nonempty(custom_title)
        .or_else(|| nonempty(session_name))
        .or_else(|| nonempty(definition_name))
        .map(str::to_string)
        .unwrap_or_else(|| match nonempty(dir) {
            Some(dir) => format!("{agent_label} 对话 · {dir}"),
            None => format!("{agent_label} 对话"),
        })
}

/// 分屏里一格自己的名字（侧栏的分屏子行、手机上的一格）：这一格当前那段对话的用户命名 >
/// 用户给这一格起的名 > 会话名 > 启动项 > 兜底。整段会话的名字不进来：改了会话名，
/// 每一格不该都跟着变成同一个名字。
pub(super) fn pane_name(
    conversation_custom: Option<&str>,
    pane_custom: Option<&str>,
    session_title: Option<&str>,
    launch_label: Option<&str>,
    fallback: Option<&str>,
) -> String {
    display_title(
        nonempty(conversation_custom).or_else(|| nonempty(pane_custom)),
        session_title,
        launch_label,
        fallback,
    )
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal<'a>(
        conversation_custom: Option<&'a str>,
        session_custom: Option<&'a str>,
        pane_count: usize,
        pane_custom: Option<&'a str>,
    ) -> String {
        TerminalTitle {
            conversation_custom,
            session_custom,
            pane_count,
            pane_custom,
            session_title: Some("自动标题"),
            ..Default::default()
        }
        .display()
    }

    #[test]
    fn single_pane_title_keeps_the_surviving_pane_name() {
        assert_eq!(
            terminal(None, None, 1, Some("跑测试的终端")),
            "跑测试的终端"
        );
        assert_eq!(
            terminal(None, Some("整个会话"), 1, Some("跑测试的终端")),
            "整个会话"
        );
        assert_eq!(
            terminal(None, None, 2, Some("左侧 pane")),
            "自动标题",
            "分屏时 pane 名不能反向改掉会话名"
        );
    }

    /// 终端里跑的是可识别的 agent 对话时，名字属于那段对话：它必须盖过会话/pane
    /// 上遗留的名字，否则用户换一段对话，还顶着上一段的名字。
    #[test]
    fn conversation_name_outranks_stale_session_and_pane_names() {
        assert_eq!(
            terminal(
                Some("记忆方案研究"),
                Some("上一段对话的旧名字"),
                1,
                Some("pane 旧名字"),
            ),
            "记忆方案研究"
        );
        // 换到一段还没被命名的对话：回落到自动标题，而不是继续用空白的名字。
        assert_eq!(terminal(None, Some("  "), 1, None), "自动标题");
    }

    #[test]
    fn terminal_without_any_name_uses_launch_label_then_fallback() {
        let mut title = TerminalTitle {
            pane_count: 1,
            launch_label: Some("Claude Code"),
            fallback: Some("smelt"),
            ..Default::default()
        };
        assert_eq!(title.display(), "Claude Code");
        title.launch_label = None;
        assert_eq!(title.display(), "smelt");
    }

    #[test]
    fn decided_session_name_is_not_rewritten_even_when_it_looks_like_a_spinner() {
        assert_eq!(
            display_title(None, Some("⠋ smelt"), Some("Codex"), Some("smelt")),
            "⠋ smelt"
        );
    }

    #[test]
    fn every_acp_provider_prefers_conversation_title_then_falls_back() {
        for kind in smelt_core::agent_kind::ConversationAgentKind::ALL {
            let agent = kind.short_label();
            assert_eq!(
                acp_conversation_name(None, Some("修复侧栏标题"), None, agent, Some("smelt")),
                "修复侧栏标题",
                "{agent} 不应被 Agent 类型特判挡住 ACP 标题"
            );
            assert_eq!(
                acp_conversation_name(None, None, None, agent, Some("smelt")),
                format!("{agent} 对话 · smelt")
            );
        }
    }

    #[test]
    fn copilot_task_title_does_not_need_spinner() {
        assert_eq!(
            display_title(
                None,
                Some("Implement Feature Request - Verifying frontend-only gates - GitHub Copilot"),
                Some("GitHub Copilot"),
                Some("ve"),
            ),
            "Implement Feature Request - Verifying frontend-only gates - GitHub Copilot"
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
                Some("developer@workstation:~/project"),
                Some("GitHub Copilot"),
                Some("smelt"),
            ),
            "developer@workstation:~/project"
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
    fn acp_conversation_name_prefers_user_then_session_then_definition() {
        let name = |custom, session, definition| {
            acp_conversation_name(custom, session, definition, "Claude", Some("smelt"))
        };
        assert_eq!(name(Some("改过"), Some("会话"), Some("Til")), "改过");
        assert_eq!(name(Some("  "), Some("会话"), Some("Til")), "会话");
        assert_eq!(name(None, None, Some("Til")), "Til");
        assert_eq!(name(None, Some(" "), None), "Claude 对话 · smelt");
    }

    #[test]
    fn pane_name_ignores_the_session_name() {
        assert_eq!(
            pane_name(None, Some("格子"), Some("会话名"), None, Some("smelt")),
            "格子"
        );
        assert_eq!(
            pane_name(Some("对话"), Some("格子"), None, None, None),
            "对话"
        );
        assert_eq!(
            pane_name(None, None, None, Some("Claude"), Some("smelt")),
            "Claude"
        );
        assert_eq!(pane_name(None, None, None, None, Some("smelt")), "smelt");
    }

    #[test]
    fn custom_title_wins() {
        assert_eq!(
            display_title(
                Some("我改的名"),
                Some("Implement Feature Request - GitHub Copilot"),
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
