//! 交互式终端的关色开关。
//!
//! Grok / CI / 脚本会给进程打上 `NO_COLOR=1`、`FORCE_COLOR=0`、`CLICOLOR=0`。
//! `portable_pty::CommandBuilder` 和 `std::process::Command` 默认全量继承父环境，
//! 这些变量一旦进 smeltd，就会传给所有内嵌 PTY。agy、Claude Code 等 TUI 只要看见
//! `NO_COLOR` 就会关掉全部 ANSI 色，改 colorScheme 也救不回来。
//!
//! 真源只有一处：托管交互式 PTY 的进程树不得携带「我不是 TTY」的关色开关。
//! 守护入口清掉本进程环境，拉起 smeltd / 开 PTY 时再从 Command 里卸一次。

/// 会让 TUI 关色的环境变量。`NO_COLOR` 规范是「只要存在就关」，与取值无关。
pub const SUPPRESSION_VARS: &[&str] = &["NO_COLOR", "FORCE_COLOR", "CLICOLOR", "CLICOLOR_FORCE"];

/// 从本进程环境卸掉关色开关。
///
/// 必须在启动任何会并发读环境的业务线程之前调用：Edition 2024 把
/// [`std::env::remove_var`] 标成 unsafe（多线程改 env 非同步）。
pub fn clear_process() {
    for key in SUPPRESSION_VARS {
        // SAFETY: 只在 smeltd 入口、尚未起业务线程时调用。
        unsafe {
            std::env::remove_var(key);
        }
    }
}

/// 从即将 spawn 的命令里卸掉关色开关，避免把宿主的 CI/Grok 环境传给子进程。
pub fn clear_command(command: &mut std::process::Command) {
    for key in SUPPRESSION_VARS {
        command.env_remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppression_vars_are_the_ones_tui_libraries_honor() {
        assert!(SUPPRESSION_VARS.contains(&"NO_COLOR"));
        assert!(SUPPRESSION_VARS.contains(&"FORCE_COLOR"));
        assert!(SUPPRESSION_VARS.contains(&"CLICOLOR"));
        assert!(SUPPRESSION_VARS.contains(&"CLICOLOR_FORCE"));
    }

    #[test]
    fn clear_command_drops_host_suppression_even_when_explicitly_set() {
        let mut command = std::process::Command::new("/bin/echo");
        command.env("NO_COLOR", "1");
        command.env("FORCE_COLOR", "0");
        command.env("CLICOLOR", "0");
        command.env("CLICOLOR_FORCE", "0");
        command.env("COLORTERM", "truecolor");
        clear_command(&mut command);

        let envs: Vec<String> = command
            .get_envs()
            .filter_map(|(key, value)| {
                value?;
                key.to_str().map(str::to_string)
            })
            .collect();
        for key in SUPPRESSION_VARS {
            assert!(
                !envs.iter().any(|k| k == key),
                "{key} 不得出现在子进程环境里，实际: {envs:?}"
            );
        }
        assert!(
            envs.iter().any(|k| k == "COLORTERM"),
            "无关变量应保留，实际: {envs:?}"
        );
    }
}
