//! 侧栏、菜单、手机共用的会话展示名。
//!
//! 会话名来自 provider 标题或首条 prompt 兜底，用户手改名优先。
//! OSC 0/2 不进入这里，也不参与「在不在跑」的判断。
//! 调用方传入的已经是决定好的会话名；这里只判断是否为空，不识别、不改写内容。

const AUTO_TITLE_MAX_CHARS: usize = 36;

/// 从首条用户请求生成各端一致的本地兜底标题。它不是 provider 的显式命名。
/// 显式改名只走 ACP `session_title` 或 hook `SessionTitleChanged`，不走 OSC。
pub fn prompt_title(prompt: &str) -> Option<String> {
    let single_line = without_image_placeholders(prompt)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
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

/// Claude Code 把贴进来的图片在 prompt 里写成 `[Image #1]`，那不是用户说的话。
fn without_image_placeholders(prompt: &str) -> String {
    const OPEN: &str = "[Image #";
    let mut out = String::with_capacity(prompt.len());
    let mut rest = prompt;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after[digits..].starts_with(']') {
            out.push_str(&rest[..start]);
            out.push(' ');
            rest = &after[digits + 1..];
        } else {
            out.push_str(&rest[..start + 1]);
            rest = &rest[start + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// 用户手改名 > 已经决定的会话名 > 启动项显示名 > 兜底（cwd / 旧菜单名）。
///
/// `session_title` 是调用方已经决定的会话名（provider 标题或首条 prompt 兜底），
/// 不是 OSC 原文。这里只判断是否为空，不改写可见内容。
pub fn display_title(
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

/// ACP 会话统一使用协议/本地生成的对话标题；尚无标题时才回退到 Agent + 项目。
/// 用户手改名由上层优先处理，不在这里混入持久化语义。
///
/// `dir` 是工作目录的末段。cwd 是运行时那台机器上的路径，得按它的系统切，由调用方切好。
pub fn acp_display_title(
    conversation_title: Option<&str>,
    agent_label: &str,
    dir: Option<&str>,
) -> String {
    if let Some(title) = nonempty(conversation_title) {
        return title.to_string();
    }
    match nonempty(dir) {
        Some(dir) => format!("{agent_label} 对话 · {dir}"),
        None => format!("{agent_label} 对话"),
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// 提供方给出的会话名。省略和纯空白都不是名字，也不能用来清空已有会话名。
///
/// ACP 流式快照、历史页和 TUI 的 `SessionTitleChanged` 都只在这里返回
/// `Some` 时写入。显式清空是协议事件自己的赋值，不走这里。
pub fn provider_session_title(incoming: Option<&str>) -> Option<&str> {
    nonempty(incoming)
}

/// ACP 会话名的各个来源，分开保存，不把已经决定好的名字倒灌回某一个来源。
///
/// 用户手改名不在这里：它存在工作区目录 / `history_title`，由展示层最先采用
/// （[`display_title`] 的 `custom_title`）。这里的顺序是 provider > 旧版标题 > 本机模型 > 首条 prompt。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionTitleSources {
    /// Agent 通过 ACP `session_info_update` 上报的标题。
    #[serde(default)]
    pub provider: Option<String>,
    /// 分来源保存之前的版本只存了决定好的展示名，分不出是 provider 标题还是 prompt
    /// 兜底。原样留着，不冒充 provider、也不冒充本机模型；有它就不再让本机模型起名。
    #[serde(default)]
    pub legacy: Option<String>,
    /// 本机已安装的小模型为这段对话生成的标题。
    #[serde(default)]
    pub local_model: Option<String>,
    /// 首条用户请求生成的兜底标题。
    #[serde(default)]
    pub prompt: Option<String>,
}

impl SessionTitleSources {
    /// provider > 旧版标题 > 本机模型 > 首条 prompt；空白不算名字。
    pub fn resolved(&self) -> Option<String> {
        nonempty(self.provider.as_deref())
            .or_else(|| nonempty(self.legacy.as_deref()))
            .or_else(|| nonempty(self.local_model.as_deref()))
            .or_else(|| nonempty(self.prompt.as_deref()))
            .map(str::to_string)
    }
}

/// 会话名：非空的提供方标题优先，否则用首条请求兜底。
pub fn session_name(provider_title: Option<&str>, prompt_fallback: Option<&str>) -> Option<String> {
    provider_session_title(provider_title)
        .or_else(|| nonempty(prompt_fallback))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_title_replaces_only_when_nonempty_and_prompt_is_the_fallback() {
        assert_eq!(provider_session_title(None), None);
        assert_eq!(provider_session_title(Some("   ")), None);
        assert_eq!(
            provider_session_title(Some(" UAT 问题修复 ")),
            Some("UAT 问题修复")
        );
        assert_eq!(
            session_name(Some("协议标题"), Some("首条问题")).as_deref(),
            Some("协议标题")
        );
        assert_eq!(
            session_name(Some("   "), Some("首条问题")).as_deref(),
            Some("首条问题")
        );
        assert_eq!(session_name(None, None), None);
    }

    #[test]
    fn title_sources_resolve_provider_then_local_model_then_prompt() {
        let mut sources = SessionTitleSources {
            provider: Some("协议标题".into()),
            legacy: Some("旧版标题".into()),
            local_model: Some("本机标题".into()),
            prompt: Some("首条问题".into()),
        };
        assert_eq!(sources.resolved().as_deref(), Some("协议标题"));
        sources.provider = Some("  ".into());
        assert_eq!(sources.resolved().as_deref(), Some("旧版标题"));
        sources.legacy = None;
        assert_eq!(sources.resolved().as_deref(), Some("本机标题"));
        sources.local_model = None;
        assert_eq!(sources.resolved().as_deref(), Some("首条问题"));
        assert_eq!(SessionTitleSources::default().resolved(), None);
    }

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
    fn prompt_title_drops_claude_image_placeholders() {
        assert_eq!(
            prompt_title("[Image #1] 压缩逻辑 [Image #12]看一下").as_deref(),
            Some("压缩逻辑 看一下")
        );
        assert_eq!(prompt_title("[Image #1]"), None, "只贴了图就没有可用的标题");
        assert_eq!(
            prompt_title("[Image #] [Image #2x] [image #3]").as_deref(),
            Some("[Image #] [Image #2x] [image #3]"),
            "不是占位格式的原样留着"
        );
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
        for kind in crate::agent_kind::ConversationAgentKind::ALL {
            let agent = kind.short_label();
            assert_eq!(
                acp_display_title(Some("修复侧栏标题"), agent, Some("smelt")),
                "修复侧栏标题",
                "{agent} 不应被 Agent 类型特判挡住 ACP 标题"
            );
            assert_eq!(
                acp_display_title(None, agent, Some("smelt")),
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
                    "Implement Feature Request - Verifying frontend-only gates - GitHub Copilot"
                ),
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
