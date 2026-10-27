//! 终端里那个智能体 CLI 的续接：守护停过（重启、升级、崩溃）之后，桌面用原来的会话 id
//! 重新打开终端时，接回 CLI 停下前正在聊的那段对话，而不是起一段新的。
//!
//! 材料都是守护自己记着的：起终端时的启动命令（`SessionState::launch`），hook 报来的
//! provider 和对话 id（`SessionState::provider` / `conversation_id`，TUI 里 `/resume` 换了
//! 对话也跟着变）。停机时会话目录把能续接的终端留下（见
//! `SessionDirectory::keep_resumable_terminals`），崩溃时目录本来就整份留着；重启后它们是
//! `runtime=false` 的目录条目，`open` 新建 PTY 前从这里取续接命令。
//!
//! 对话 id 在 CLI 退出后仍留着（侧栏身份、复制会话 id、改名都靠它）。能不能续接看
//! `SessionState::cli_exited`：`SessionEnded` 把它置上之后，会话目录拒绝拼 `--resume`，
//! 启动命令即便就是这家 CLI（`launch = "claude"`）也不续。
//! 不发 `SessionEnded` 的 provider 没有「进程已退出」这个信号：用户回到 shell 之后，
//! `cli_exited` 仍是 false，守护重启仍会按最后一次对话 id 续接。当前 hook 集里没有
//! SessionEnd 的是 Antigravity 和 Kiro；Crush 没有生命周期 hook。OpenCode 只在会话被删除时
//! 转发 SessionEnd，退出 TUI 回到 shell 通常不会发。

use smelt_core::agent_kind::TerminalAgentKind;

/// 续接一个终端要跑的命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResumeLaunch {
    /// 这次真正跑的：原启动命令接上续接参数。
    pub(crate) run: String,
    /// 记进会话状态的启动命令，仍是原来那条。下次再续接时在它后面接，不会越接越长。
    pub(crate) launch: Option<String>,
}

/// 这个终端能不能续接、怎么续接。
///
/// 只有 hook 报过对话 id、这家 CLI 有续接语法时才续。`conversation_id` 为 None
/// （还没报过）直接不续，启动命令是不是这家 CLI 都不看。进程已经退出由调用方
/// 看 `cli_exited` 提前拒绝，这里不重复。
/// 启动命令就是这家 CLI 时带着原来的参数（权限、配置目录这些）接回去；没有启动命令
/// （在 shell 里手动起的）用这家的程序名，退出后照样回到 shell。启动的是别的东西
/// （脚本、别家 CLI），这家是后来在里面起的：不替用户把这个终端改成别的命令，照旧起原来那条。
pub(crate) fn resume_launch(
    launch: Option<&str>,
    provider: Option<&str>,
    conversation_id: Option<&str>,
) -> Option<ResumeLaunch> {
    let kind = TerminalAgentKind::from_id(provider?)?;
    let syntax = kind.cli_resume_syntax()?;
    let id = quote_id(conversation_id?.trim())?;
    let launch = launch.map(str::trim).filter(|launch| !launch.is_empty());
    let base = match launch {
        Some(launch) if launch_program(launch) == Some(kind) => launch,
        Some(_) => return None,
        None => kind.cli_program(),
    };
    Some(ResumeLaunch {
        run: format!("{base} {}", syntax.suffix(&id)),
        launch: launch.map(str::to_string),
    })
}

/// 启动命令跑的是哪家 CLI。跳过开头的 `env` 和 `NAME=value`：配置档会把配置目录写成
/// 这样的前缀（`CLAUDE_CONFIG_DIR=… claude …`）。切不开（引号没配对）就当认不出。
fn launch_program(launch: &str) -> Option<TerminalAgentKind> {
    let words = shell_words::split(launch).ok()?;
    let program = words
        .iter()
        .find(|word| word.as_str() != "env" && !is_assignment(word))?;
    TerminalAgentKind::from_command_prefix(program)
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars().next().is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

/// 对话 id 平常是 uuid 一类，原样拼进命令行；别的字符按终端用的 shell 引用。shell 引用
/// 不了（cmd 碰上 `"`、`%`）就不续接，照旧起原来那条。
fn quote_id(id: &str) -> Option<String> {
    if id.is_empty() {
        return None;
    }
    crate::terminal_shell::TerminalShell::detect().quote(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(launch: Option<&str>, provider: &str, id: &str) -> Option<String> {
        resume_launch(launch, Some(provider), Some(id)).map(|resume| resume.run)
    }

    #[test]
    fn the_original_command_keeps_its_flags_and_gains_the_resume_argument() {
        let resume = resume_launch(
            Some("claude --dangerously-skip-permissions"),
            Some("claude"),
            Some("0b5c7f3e-1d2a-4c6b-9e8f-123456789abc"),
        )
        .unwrap();
        assert_eq!(
            resume.run,
            "claude --dangerously-skip-permissions --resume 0b5c7f3e-1d2a-4c6b-9e8f-123456789abc"
        );
        assert_eq!(
            resume.launch.as_deref(),
            Some("claude --dangerously-skip-permissions"),
            "会话状态里记原命令，下次续接才不会叠两个 --resume"
        );
    }

    #[test]
    fn each_cli_uses_its_own_resume_syntax() {
        assert_eq!(
            run(Some("codex"), "codex", "abc").as_deref(),
            Some("codex resume abc")
        );
        assert_eq!(
            run(Some("copilot --allow-all"), "copilot", "abc").as_deref(),
            Some("copilot --allow-all --resume=abc")
        );
    }

    #[test]
    fn a_profile_prefix_still_counts_as_the_same_cli() {
        assert_eq!(
            run(Some("CLAUDE_CONFIG_DIR=~/.claude-work claude"), "claude", "abc").as_deref(),
            Some("CLAUDE_CONFIG_DIR=~/.claude-work claude --resume abc")
        );
        assert_eq!(
            run(Some("env CLAUDE_CONFIG_DIR='/a b' claude"), "claude", "abc").as_deref(),
            Some("env CLAUDE_CONFIG_DIR='/a b' claude --resume abc")
        );
    }

    #[test]
    fn a_cli_started_by_hand_in_the_shell_resumes_with_its_program_name() {
        let resume = resume_launch(None, Some("claude"), Some("abc")).unwrap();
        assert_eq!(resume.run, "claude --resume abc");
        assert_eq!(resume.launch, None);
    }

    #[test]
    fn a_terminal_that_launched_something_else_is_left_alone() {
        assert_eq!(run(Some("npm run dev"), "claude", "abc"), None);
        assert_eq!(run(Some("codex"), "claude", "abc"), None);
    }

    #[test]
    fn nothing_to_resume_without_a_known_cli_or_a_conversation() {
        assert_eq!(resume_launch(Some("claude"), Some("claude"), None), None);
        assert_eq!(resume_launch(Some("claude"), None, Some("abc")), None);
        assert_eq!(run(Some("claude"), "no-such-cli", "abc"), None);
        assert_eq!(run(Some("claude"), "claude", "  "), None);
    }

    #[cfg(unix)]
    #[test]
    fn an_unusual_id_is_quoted() {
        assert_eq!(
            run(Some("claude"), "claude", "a b").as_deref(),
            Some("claude --resume 'a b'")
        );
    }
}
