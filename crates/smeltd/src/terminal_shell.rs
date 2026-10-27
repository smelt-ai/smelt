//! 交互终端起哪个 shell、怎么起，往它的命令行里拼参数时怎么引用。
//!
//! Unix 用 `$SHELL`，起成 login + interactive。Windows 和 Windows Terminal、VS Code 的
//! 默认一致：PowerShell 7（`pwsh`）→ 系统自带的 Windows PowerShell 5.1 → `COMSPEC`（cmd）。
//! `COMSPEC` 是给批处理和 `system()` 用的命令解释器，不是用户的交互 shell；自动化和后台
//! 任务的单行命令仍走 `cmd /d /c`（见 `smelt_model::automation`）。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShellKind {
    Posix,
    // 只有 Windows 的 `detect` 会挑出这两种；别的平台也留着，让拼参数和引用的测试在 Mac 上照跑。
    #[cfg_attr(not(windows), allow(dead_code))]
    PowerShell,
    #[cfg_attr(not(windows), allow(dead_code))]
    Cmd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalShell {
    pub(crate) program: String,
    pub(crate) kind: ShellKind,
}

impl TerminalShell {
    /// 每次起终端时现查：用户装上 pwsh 之后新开的终端就用它，不用重启守护。
    pub(crate) fn detect() -> Self {
        #[cfg(windows)]
        {
            windows_shell()
        }
        #[cfg(not(windows))]
        {
            let program = std::env::var("SHELL")
                .ok()
                .filter(|shell| !shell.trim().is_empty())
                .unwrap_or_else(|| "/bin/zsh".to_string());
            Self {
                program,
                kind: ShellKind::Posix,
            }
        }
    }

    /// `launch`：项目「+」悬浮菜单的 Claude Code / Codex 快捷入口——把要跑的命令直接编进
    /// 启动命令行，而不是等 shell 起来后再补发按键。这样从根上没有「shell 是否已经在读
    /// stdin」的时序问题；命令跑完留在一个正常的交互 shell 里，之后就是一个普通会话。
    pub(crate) fn launch_args(&self, launch: Option<&str>) -> Vec<String> {
        match (self.kind, launch) {
            // interactive + login：用户级 CLI 安装器通常把 PATH 写进 .zshrc，只用 `-lc`
            // 读不到，Dock 启动的 smeltd 又只有系统 PATH。
            (ShellKind::Posix, Some(launch)) => vec![
                "-ilc".to_string(),
                format!("{launch}; exec {} -l", self.program),
            ],
            (ShellKind::Posix, None) => vec!["-l".to_string()],
            // 照常读用户的 profile（相当于 login shell 读 rc），-NoExit 跑完命令留在提示符。
            (ShellKind::PowerShell, Some(launch)) => vec![
                "-NoLogo".to_string(),
                "-NoExit".to_string(),
                "-Command".to_string(),
                launch.to_string(),
            ],
            (ShellKind::PowerShell, None) => vec!["-NoLogo".to_string()],
            // cmd 不读 AutoRun（/d），/k 跑完命令留在提示符。
            (ShellKind::Cmd, Some(launch)) => {
                vec!["/d".to_string(), "/k".to_string(), launch.to_string()]
            }
            (ShellKind::Cmd, None) => vec!["/d".to_string(), "/k".to_string()],
        }
    }

    /// 把一个参数作为单个词拼进 [`Self::launch_args`] 的命令。只含字母数字和 `-_.` 的
    /// 原样返回。cmd 没有能转义 `"` 和 `%` 的引号，碰到它们返回 None，调用方别拼。
    pub(crate) fn quote(&self, value: &str) -> Option<String> {
        if !value.is_empty()
            && value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Some(value.to_string());
        }
        match self.kind {
            ShellKind::Posix => Some(format!("'{}'", value.replace('\'', "'\"'\"'"))),
            // PowerShell 把弯引号也当单引号，单引号字符串里一律写两遍。
            ShellKind::PowerShell => {
                let mut quoted = String::with_capacity(value.len() + 2);
                quoted.push('\'');
                for ch in value.chars() {
                    if matches!(ch, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
                        quoted.push(ch);
                    }
                    quoted.push(ch);
                }
                quoted.push('\'');
                Some(quoted)
            }
            ShellKind::Cmd => (!value.contains(['"', '%'])).then(|| format!("\"{value}\"")),
        }
    }
}

#[cfg(windows)]
fn windows_shell() -> TerminalShell {
    use std::path::{Path, PathBuf};

    // 应用商店装的 pwsh 是 WindowsApps 下的执行别名（重解析点），跟随它取元数据会失败，
    // 只看链接本身在不在。
    fn exists(path: &Path) -> bool {
        path.symlink_metadata().is_ok()
    }

    let on_path = || {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join("pwsh.exe"))
            .find(|path| exists(path))
    };
    let installed_pwsh = || {
        let root = std::env::var_os("ProgramFiles")?;
        Some(PathBuf::from(root).join(r"PowerShell\7\pwsh.exe")).filter(|path| exists(path))
    };
    let system_root =
        std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let windows_powershell = || {
        Some(Path::new(&system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
            .filter(|path| exists(path))
    };
    if let Some(path) = on_path()
        .or_else(installed_pwsh)
        .or_else(windows_powershell)
    {
        return TerminalShell {
            program: path.to_string_lossy().into_owned(),
            kind: ShellKind::PowerShell,
        };
    }
    let program = std::env::var("COMSPEC")
        .ok()
        .filter(|shell| exists(Path::new(shell)))
        .unwrap_or_else(|| {
            Path::new(&system_root)
                .join(r"System32\cmd.exe")
                .to_string_lossy()
                .into_owned()
        });
    TerminalShell {
        program,
        kind: ShellKind::Cmd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(program: &str, kind: ShellKind) -> TerminalShell {
        TerminalShell {
            program: program.to_string(),
            kind,
        }
    }

    #[test]
    fn agent_launch_stays_in_an_interactive_shell_afterwards() {
        let launch = Some("claude --dangerously-skip-permissions");
        assert_eq!(
            shell("/bin/zsh", ShellKind::Posix).launch_args(launch),
            ["-ilc", "claude --dangerously-skip-permissions; exec /bin/zsh -l"]
        );
        assert_eq!(shell("/bin/zsh", ShellKind::Posix).launch_args(None), ["-l"]);
        assert_eq!(
            shell("pwsh.exe", ShellKind::PowerShell).launch_args(launch),
            [
                "-NoLogo",
                "-NoExit",
                "-Command",
                "claude --dangerously-skip-permissions"
            ]
        );
        assert_eq!(
            shell("pwsh.exe", ShellKind::PowerShell).launch_args(None),
            ["-NoLogo"]
        );
        assert_eq!(
            shell("cmd.exe", ShellKind::Cmd).launch_args(launch),
            ["/d", "/k", "claude --dangerously-skip-permissions"]
        );
        assert_eq!(shell("cmd.exe", ShellKind::Cmd).launch_args(None), ["/d", "/k"]);
    }

    #[test]
    fn quoting_follows_the_shell() {
        let posix = shell("/bin/zsh", ShellKind::Posix);
        let powershell = shell("pwsh.exe", ShellKind::PowerShell);
        let cmd = shell("cmd.exe", ShellKind::Cmd);
        for shell in [&posix, &powershell, &cmd] {
            assert_eq!(shell.quote("abc-1_2.3").as_deref(), Some("abc-1_2.3"));
        }
        assert_eq!(posix.quote("it's").as_deref(), Some(r#"'it'"'"'s'"#));
        assert_eq!(
            powershell.quote(r"C:\Users\o'neil\a b.ts").as_deref(),
            Some(r"'C:\Users\o''neil\a b.ts'")
        );
        assert_eq!(powershell.quote("a\u{2019}b").as_deref(), Some("'a\u{2019}\u{2019}b'"));
        assert_eq!(
            cmd.quote(r"C:\Users\w\a b.ts").as_deref(),
            Some(r#""C:\Users\w\a b.ts""#)
        );
        assert_eq!(cmd.quote("50%"), None);
        assert_eq!(cmd.quote(r#"a"b"#), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_prefers_powershell() {
        assert_eq!(TerminalShell::detect().kind, ShellKind::PowerShell);
    }
}
