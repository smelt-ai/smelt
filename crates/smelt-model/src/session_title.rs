//! 会话名：提供方给的标题，没有就用首条请求兜底。运行时托管 ACP 会话、读历史时都用它。
//!
//! OSC 0/2 不进入这里，也不参与「在不在跑」的判断。侧栏、菜单、手机上显示的名字由
//! smeltd 在这之上按工作区目录算好推出去（见 smeltd `session_names`）。

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
}
