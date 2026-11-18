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

use smelt_core::agent_kind::{CliResumeSyntax, TerminalAgentKind};

/// 续接一个终端要跑的命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResumeLaunch {
    /// 这次真正跑的：原启动命令接上续接参数。
    pub(crate) run: String,
    /// 记进会话状态的启动命令，仍是原来那条。下次续接再从它去掉选对话那段后接，不会越接越长。
    pub(crate) launch: Option<String>,
    /// 续接的新终端沿用原来的身份，不等下一次 hook 才能再次续接。
    pub(crate) provider: String,
    pub(crate) conversation_id: String,
}

/// 这个终端能不能续接、怎么续接。
///
/// 只有 hook 报过对话 id、这家 CLI 有续接语法时才续。`conversation_id` 为 None
/// （还没报过）直接不续，启动命令是不是这家 CLI 都不看。进程已经退出由调用方
/// 看 `cli_exited` 提前拒绝，这里不重复。
/// 启动命令就是这家 CLI 时带着原来的参数（权限、配置目录这些）接回去，去掉它原来选对话
/// 的那段（见 [`without_resume`]）；没有启动命令（在 shell 里手动起的）用这家的程序名，退出后
/// 照样回到 shell。启动的是别的东西（脚本、别家 CLI），这家是后来在里面起的：不替用户把这个
/// 终端改成别的命令，照旧起原来那条。
pub(crate) fn resume_launch(
    launch: Option<&str>,
    provider: Option<&str>,
    conversation_id: Option<&str>,
) -> Option<ResumeLaunch> {
    let provider = provider?;
    let kind = TerminalAgentKind::from_id(provider)?;
    let syntax = kind.cli_resume_syntax()?;
    let conversation_id = conversation_id?.trim();
    let id = quote_id(conversation_id)?;
    let launch = launch.map(str::trim).filter(|launch| !launch.is_empty());
    let base = match launch {
        Some(launch) => {
            let words = words(launch)?;
            if program(&words).map(|(_, kind)| kind) != Some(kind) {
                return None;
            }
            without_resume(launch, &words, syntax)
        }
        None => kind.cli_program().to_string(),
    };
    Some(ResumeLaunch {
        run: format!("{base} {}", syntax.suffix(&id)),
        launch: launch.map(str::to_string),
        provider: provider.to_string(),
        conversation_id: conversation_id.to_string(),
    })
}

/// 启动命令里的一个词：`raw` 是原文（带引号），`value` 是 shell 去掉引号后的值。
struct Word<'a> {
    raw: &'a str,
    value: String,
}

/// 按 POSIX shell 的规则切词，留着每个词的原文。引号没配对是 `None`。
fn words(line: &str) -> Option<Vec<Word<'_>>> {
    let mut words = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some(&(start, _)) = chars.peek() {
        if chars.next_if(|(_, ch)| ch.is_whitespace()).is_some() {
            continue;
        }
        let mut value = String::new();
        let mut end = line.len();
        while let Some(&(index, ch)) = chars.peek() {
            if ch.is_whitespace() {
                end = index;
                break;
            }
            chars.next();
            match ch {
                '\\' => value.push(chars.next()?.1),
                '\'' => loop {
                    match chars.next()?.1 {
                        '\'' => break,
                        ch => value.push(ch),
                    }
                },
                '"' => loop {
                    match chars.next()?.1 {
                        '"' => break,
                        '\\' => {
                            let (_, escaped) = chars.next()?;
                            if !matches!(escaped, '"' | '\\' | '$' | '`') {
                                value.push('\\');
                            }
                            value.push(escaped);
                        }
                        ch => value.push(ch),
                    }
                },
                ch => value.push(ch),
            }
        }
        words.push(Word {
            raw: &line[start..end],
            value,
        });
    }
    Some(words)
}

/// 启动命令跑的是哪家 CLI，和程序名是第几个词。跳过开头的 `env` 和 `NAME=value`：配置档会
/// 把配置目录写成这样的前缀（`CLAUDE_CONFIG_DIR=… claude …`）。
fn program(words: &[Word<'_>]) -> Option<(usize, TerminalAgentKind)> {
    let (index, word) = words
        .iter()
        .enumerate()
        .find(|(_, word)| word.value != "env" && !is_assignment(&word.value))?;
    Some((index, TerminalAgentKind::from_command_prefix(&word.value)?))
}

/// 启动命令去掉这家 CLI 选对话的那段，剩下的是「怎么起这家 CLI」。启动命令可能本来就是
/// 续接：侧栏「在终端继续」起的是 `claude --resume X`，用户在启动项里也可能写着。原样往后接
/// 就成了 `--resume X --resume Y`。剩下的词按原文拼回去，不重新加引号，`~`、引号都保持原样。
///
/// 认的是 `syntax` 这一种写法，两种书写都认（`--resume X` 和 `--resume=X`）；值可省的
/// （`claude --resume` 打开选单）后面是别的参数时只去掉开关本身。子命令是程序名后第一个
/// `resume` 和它后面的 id。
fn without_resume(launch: &str, words: &[Word<'_>], syntax: CliResumeSyntax) -> String {
    let mut skip = vec![false; words.len()];
    let takes_value = |index: usize| {
        words
            .get(index)
            .is_some_and(|word| !word.value.starts_with('-'))
    };
    match flag_of(syntax) {
        Some(flag) => {
            let mut index = 0;
            while index < words.len() {
                let value = &words[index].value;
                if value == flag {
                    skip[index] = true;
                    if takes_value(index + 1) {
                        skip[index + 1] = true;
                        index += 1;
                    }
                } else if value
                    .strip_prefix(flag)
                    .is_some_and(|rest| rest.starts_with('='))
                {
                    skip[index] = true;
                }
                index += 1;
            }
        }
        None => {
            let start = program(words).map_or(0, |(index, _)| index + 1);
            if let Some(index) = (start..words.len()).find(|&index| words[index].value == "resume")
            {
                skip[index] = true;
                if takes_value(index + 1) {
                    skip[index + 1] = true;
                }
            }
        }
    }
    if !skip.contains(&true) {
        return launch.to_string();
    }
    words
        .iter()
        .zip(skip)
        .filter(|(_, skip)| !skip)
        .map(|(word, _)| word.raw)
        .collect::<Vec<_>>()
        .join(" ")
}

/// 这种续接写法的开关名。子命令没有开关，是 `None`。
fn flag_of(syntax: CliResumeSyntax) -> Option<&'static str> {
    match syntax {
        CliResumeSyntax::Flag | CliResumeSyntax::FlagEquals => Some("--resume"),
        CliResumeSyntax::SessionFlag => Some("--session"),
        CliResumeSyntax::ResumeIdFlag => Some("--resume-id"),
        CliResumeSyntax::ConversationFlag => Some("--conversation"),
        CliResumeSyntax::Subcommand => None,
    }
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

    /// 侧栏「在终端继续」起的终端，启动命令本来就带着续接：守护续接时换成当前这段对话，
    /// 不再叠一个。会话状态里记的仍是原命令。
    #[test]
    fn a_launch_that_already_resumes_is_resumed_once_with_the_current_conversation() {
        let resume = resume_launch(
            Some("claude --dangerously-skip-permissions --resume 'old-id'"),
            Some("claude"),
            Some("new-id"),
        )
        .unwrap();
        assert_eq!(
            resume.run,
            "claude --dangerously-skip-permissions --resume new-id"
        );
        assert_eq!(
            resume.launch.as_deref(),
            Some("claude --dangerously-skip-permissions --resume 'old-id'")
        );
        assert_eq!(
            run(Some("claude --resume=old --model opus"), "claude", "new").as_deref(),
            Some("claude --model opus --resume new")
        );
        assert_eq!(
            run(Some("copilot --resume=old --allow-all"), "copilot", "new").as_deref(),
            Some("copilot --allow-all --resume=new")
        );
        assert_eq!(
            run(Some("codex resume old --search"), "codex", "new").as_deref(),
            Some("codex --search resume new")
        );
    }

    #[test]
    fn a_resume_switch_without_an_id_is_dropped_alone() {
        assert_eq!(
            run(Some("claude --resume --model opus"), "claude", "new").as_deref(),
            Some("claude --model opus --resume new")
        );
    }

    /// 剩下的词按原文拼回去：`~`、引号、带空格的值都不能被改写。
    #[test]
    fn the_rest_of_the_launch_keeps_its_original_quoting() {
        assert_eq!(
            run(
                Some("CLAUDE_CONFIG_DIR=~/.claude-work claude --append-system-prompt \"a b\" --resume x"),
                "claude",
                "y"
            )
            .as_deref(),
            Some("CLAUDE_CONFIG_DIR=~/.claude-work claude --append-system-prompt \"a b\" --resume y")
        );
    }

    /// 只认这家自己的写法：别的开关碰巧以它开头（`--resume-id`）不算。
    #[test]
    fn only_the_clis_own_resume_flag_is_removed() {
        assert_eq!(
            run(Some("claude --resume-id keep"), "claude", "new").as_deref(),
            Some("claude --resume-id keep --resume new")
        );
    }

    #[test]
    fn words_follow_shell_quoting() {
        let values = |line: &str| {
            words(line).map(|words| {
                words
                    .into_iter()
                    .map(|word| (word.raw.to_string(), word.value))
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            values(r#"  a 'b c'  "d \"e\" \n" f\ g "#),
            Some(vec![
                ("a".to_string(), "a".to_string()),
                ("'b c'".to_string(), "b c".to_string()),
                (r#""d \"e\" \n""#.to_string(), r#"d "e" \n"#.to_string()),
                (r"f\ g".to_string(), "f g".to_string()),
            ])
        );
        assert!(values("claude 'unterminated").is_none());
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
