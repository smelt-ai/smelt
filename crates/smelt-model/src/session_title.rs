//! 侧栏、菜单、手机共用的会话展示名。
//!
//! 会话名的来源，从高到低：用户手改名、provider 标题（hook / ACP / OSC 7501 `title`）、
//! agent CLI 用 OSC 0/2 写的窗口标题、首条 prompt 兜底。谁能覆盖谁由守护归约时定。
//! OSC 0/2 只在 agent CLI 前台运行时收，先经 [`terminal_program_title`] 清洗；它不参与
//! 「在不在跑」的判断。
//! 展示时调用方传入的已经是决定好的会话名；[`display_title`] 只判断是否为空，不识别、不改写内容。

const AUTO_TITLE_MAX_CHARS: usize = 36;
/// 窗口标题清洗后最多留几个字。
const TERMINAL_TITLE_MAX_CHARS: usize = 80;

/// agent CLI 用 OSC 0/2 写的窗口标题，清洗成会话名。不是名字的返回 `None`。
///
/// 内容来自 PTY 里任意程序，只拿来给人看。参照 cmux 的做法：
/// - 控制字符、改变文字方向和不可见的格式字符去掉，压成一行，限长；
/// - 单独成词的转圈字符（Claude 的星号、盲文点阵、半圆）去掉，动画的每一帧就是同一个名字，
///   不会反复广播；去掉后首尾剩下的分隔符也去掉；
/// - 清洗后是空的，或只是 `generic` 里的某一个（程序名、项目目录名，不分大小写），不是名字。
pub fn terminal_program_title(raw: &str, generic: &[&str]) -> Option<String> {
    let visible: String = raw
        .chars()
        .filter(|ch| !is_invisible_format(*ch))
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let words: Vec<&str> = visible
        .split_whitespace()
        .filter(|word| !is_spinner_frame(word))
        .collect();
    let joined = words.join(" ");
    let title =
        joined.trim_matches(|ch: char| ch.is_whitespace() || TITLE_EDGE_SEPARATORS.contains(&ch));
    if title.is_empty()
        || generic
            .iter()
            .any(|name| !name.trim().is_empty() && name.trim().eq_ignore_ascii_case(title))
    {
        return None;
    }
    let mut chars = title.chars();
    let bounded: String = chars.by_ref().take(TERMINAL_TITLE_MAX_CHARS).collect();
    Some(if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    })
}

/// 去掉转圈字符后留在首尾的分隔符，如 Grok 的 `⠼ - … - grok` 开头那个 `-`。
const TITLE_EDGE_SEPARATORS: &[char] = &['-', '–', '—', '|', '·', ':', '•'];

/// 改变文字方向、零宽之类看不见但会改变显示的字符。
fn is_invisible_format(ch: char) -> bool {
    matches!(
        ch,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// 一个单独成词的转圈字符。两个以上连在一起的不算，那是文字或字符画。
fn is_spinner_frame(word: &str) -> bool {
    let mut chars = word.chars();
    let (Some(ch), None) = (chars.next(), chars.next()) else {
        return false;
    };
    // 盲文点阵（Codex、Grok、Gemini 等）。
    ('\u{2800}'..='\u{28FF}').contains(&ch)
        // Claude Code 的星号帧；半圆、四分圆。
        || "✢✳✶✻✽✸✹✺*·◐◓◑◒◴◷◶◵".contains(ch)
}

/// 从首条用户请求生成各端一致的本地兜底标题。它不是 provider 的显式命名，
/// 排在所有其它来源之后。
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
/// `session_title` 是调用方已经决定的会话名（见模块文档的来源顺序），窗口标题
/// 在守护里已经清洗过。这里只判断是否为空，不改写可见内容。
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
    fn terminal_program_title_strips_spinner_frames_so_animation_is_one_name() {
        // Claude Code 忙的时候在会话名前轮换星号帧。
        for frame in ["✳", "✢", "✶", "✻", "✽", "·"] {
            assert_eq!(
                terminal_program_title(&format!("{frame} 审查权限门闩"), &[]).as_deref(),
                Some("审查权限门闩")
            );
        }
        assert_eq!(
            terminal_program_title("⠼ - Preparing read_file... - Reviewing gates - grok", &[])
                .as_deref(),
            Some("Preparing read_file... - Reviewing gates - grok")
        );
        assert_eq!(
            terminal_program_title("◐ 重构存储层 ◓", &[]).as_deref(),
            Some("重构存储层")
        );
        // 两个以上连在一起的是文字，不是转圈。
        assert_eq!(
            terminal_program_title("⠓⠠ 盲文", &[]).as_deref(),
            Some("⠓⠠ 盲文")
        );
    }

    #[test]
    fn terminal_program_title_rejects_program_and_project_names() {
        let generic = ["Claude Code", "claude", "smelt"];
        assert_eq!(terminal_program_title("✳ Claude Code", &generic), None);
        assert_eq!(terminal_program_title("claude code", &generic), None);
        assert_eq!(terminal_program_title("⠐ smelt", &generic), None);
        assert_eq!(terminal_program_title("  ✳  ", &generic), None);
        assert_eq!(terminal_program_title("", &generic), None);
        assert_eq!(
            terminal_program_title("smelt 标题重做", &generic).as_deref(),
            Some("smelt 标题重做")
        );
    }

    #[test]
    fn terminal_program_title_drops_controls_and_direction_overrides_and_is_bounded() {
        assert_eq!(
            terminal_program_title("修\u{202E}复\t标\n题\u{200B}", &[]).as_deref(),
            Some("修复 标 题")
        );
        let long = terminal_program_title(&"标".repeat(200), &[]).unwrap();
        assert_eq!(long.chars().count(), TERMINAL_TITLE_MAX_CHARS + 1);
        assert!(long.ends_with('…'));
    }

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
