//! 交互式 login shell 环境探测：GUI 是 Finder/Dock 拉起的进程，读不到用户
//! `.zshrc` 里 `export` 的值——不止 PATH（nvm/homebrew 装的 CLI 找不到），
//! 各家 agent 自定义 workspace 目录的环境变量也是同一个坑：
//!
//! - Claude：`CLAUDE_CONFIG_DIR`（非官方文档但实测生效，默认 `~/.claude`）
//! - Codex：`CODEX_HOME`（官方文档，默认 `~/.codex`）
//! - Grok：`GROK_HOME`（写在 Grok CLI 自带的用户手册里，默认 `~/.grok`）
//! - Copilot：`COPILOT_HOME`（官方推荐用法，优先于遗留的 `--config-dir`）或
//!   `XDG_CONFIG_HOME`（改成 `$XDG_CONFIG_HOME/copilot`），默认 `~/.copilot`
//!
//! 一次 `zsh -ilc` 探测把**整个环境**导出来（`env -0`），不为每个变量单开一次
//! 慢 shell（`-ilc` 要跑一遍 `.zshrc`，有感知延迟）。
//!
//! 为什么是整个环境而不是一张具名变量白名单：Node 工具链的定位方式因人而异，
//! 而**只补 PATH 只对"把 bin 目录塞进 PATH"式的管理器（nvm、homebrew）成立**。
//! shim 式的管理器把版本决策放在环境变量里，PATH 里那个可执行文件只是个转发器：
//!
//! - volta：shim 靠 `VOLTA_HOME` 找工具链；pnpm 支持还要 `VOLTA_FEATURE_PNPM=1`
//! - asdf / mise：`ASDF_DIR`、`ASDF_DATA_DIR`、`MISE_*`
//! - fnm：`FNM_DIR`、`FNM_MULTISHELL_PATH`
//! - pnpm 自身：`PNPM_HOME`；corepack：`COREPACK_*`
//!
//! 还有一类与管理器无关但同样致命的：`HTTPS_PROXY` / `NO_PROXY` /
//! `npm_config_registry`。丢了它们，`pnpm add` 不会报错，而是**静默重试到超时**
//! ——用户看到的就是"卡住"。
//!
//! 枚举这些变量名等于把每一个新出现的管理器都变成一次改代码，所以这里不枚举：
//! 整体快照 login shell 的环境。所有管理器的共同契约就是"在 shell rc 里初始化
//! 自己"，快照天然覆盖已有和未来的实现。

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 环境块的 marker 名。整个 `env -0` 输出被包在这一对标记之间。
const ENV_MARKER: &str = "ENV";

#[derive(Default)]
struct LoginEnv {
    /// PATH 单独留一份：它是唯一保证有值的（进程自身 PATH 兜底），
    /// 且 `login_path()` 要返回 `&'static str`。
    path: String,
    vars: HashMap<String, String>,
}

fn login_env() -> &'static LoginEnv {
    static ENV: OnceLock<LoginEnv> = OnceLock::new();
    ENV.get_or_init(probe)
}

/// 权威探测用户的默认登录 Shell：优先从 `$SHELL` 环境变量取，
/// 其次通过 `libc::getpwuid` 从系统目录服务获取，最后回退 `/bin/zsh`。
fn detect_user_shell() -> String {
    if let Some(shell) = std::env::var("SHELL").ok().filter(|s| !s.trim().is_empty())
        && std::path::Path::new(&shell).is_file()
    {
        return shell;
    }
    #[cfg(unix)]
    unsafe {
        let uid = libc::getuid();
        let pw = libc::getpwuid(uid);
        if !pw.is_null()
            && !(*pw).pw_shell.is_null()
            && let Ok(s) = std::ffi::CStr::from_ptr((*pw).pw_shell).to_str()
            && !s.is_empty()
            && std::path::Path::new(s).is_file()
        {
            return s.to_string();
        }
    }
    "/bin/zsh".to_string()
}

fn build_probe_command(shell: &str) -> Command {
    // 用绝对路径调 env：rc 里把 PATH 覆盖坏的机器上，裸 `env` 会连带失败，
    // 而这个探测正是用来救 PATH 的，不能反过来依赖它。
    let script =
        format!(r#"printf "__V_{ENV_MARKER}_B__"; /usr/bin/env -0; printf "__V_{ENV_MARKER}_E__""#);

    let mut cmd = Command::new(shell);
    if shell.ends_with("/fish") {
        cmd.args(["-l", "-c", &script]);
    } else if shell.ends_with("/nu") {
        // nushell 没有内置 `env` 命令（环境在 `$env` 记录里），`^` 前缀强制走
        // 外部命令；marker 用 `print -n` 避免尾随换行。
        let nu_script = format!(
            r#"print -n "__V_{ENV_MARKER}_B__"; ^/usr/bin/env -0; print -n "__V_{ENV_MARKER}_E__""#
        );
        cmd.args(["--login", "-c", &nu_script]);
    } else {
        cmd.args(["-ilc", &script]);
    }

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    cmd
}

fn run_probe_with_timeout(shell: &str) -> Option<String> {
    let child = build_probe_command(shell).spawn().ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let res = child.wait_with_output();
        let _ = tx.send(res);
    });

    match rx.recv_timeout(PROBE_TIMEOUT) {
        Ok(Ok(output)) => {
            let out = String::from_utf8_lossy(&output.stdout).into_owned();
            if output.status.success() || out.contains(&format!("__V_{ENV_MARKER}_B__")) {
                Some(out)
            } else {
                None
            }
        }
        Ok(Err(_)) => None,
        Err(_) => {
            // 超时熔断保护：kill 挂起的子进程，防止阻塞 GUI
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
            None
        }
    }
}

fn probe() -> LoginEnv {
    let shell = detect_user_shell();
    let raw = run_probe_with_timeout(&shell);

    let mut vars = raw
        .as_deref()
        .and_then(|raw| extract_marked(raw, ENV_MARKER))
        .map(|block| parse_env_block(&block))
        .unwrap_or_default();

    // PATH 兜底：探测失败（shell 起不来/超时）也必须有一条能用的搜索路径，
    // 否则连 `dsh`、`npx` 都解析不出来。
    let path = vars
        .get("PATH")
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    vars.insert("PATH".to_string(), path.clone());

    LoginEnv { path, vars }
}

/// 把 `env -0` 的输出解析成 KV。
///
/// NUL 分隔是唯一能安全承载"值里带换行"的形式（`npm_config_*`、证书类变量真的
/// 会有多行值）。但个别 shell 会在管道里把 NUL 吞掉，那时退回按行解析——多行值
/// 的续行还原不了，只能丢弃，可单行值（PATH、代理、各管理器的 HOME）仍能拿到，
/// 比整块放弃强。
fn parse_env_block(block: &str) -> HashMap<String, String> {
    if block.contains('\0') {
        block.split('\0').filter_map(split_env_entry).collect()
    } else {
        block.lines().filter_map(split_env_entry).collect()
    }
}

/// 拆 `KEY=VALUE`。只认合法的 shell 变量名，借此丢掉按行解析时混进来的多行值
/// 续行（那些行不是 `合法名=` 开头）。
fn split_env_entry(entry: &str) -> Option<(String, String)> {
    let (key, value) = entry.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || key.starts_with(|c: char| c.is_ascii_digit())
        || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

/// 从探测输出里抠出 `__V_<name>_B__…__V_<name>_E__` 之间的内容，丢掉交互式
/// shell 启动时 shell-integration 打的 OSC 转义噪音（形如
/// `\e]1337;…;shell=zsh`，会粘在第一个变量前面，见 acp_conn.rs 历史踩坑记录）。
/// 找不到标记（shell 没跑起来 / 输出被截断）返回 None。
fn extract_marked(raw: &str, name: &str) -> Option<String> {
    let begin = format!("__V_{name}_B__");
    let end = format!("__V_{name}_E__");
    let start = raw.find(&begin)? + begin.len();
    let rest = &raw[start..];
    let stop = rest.find(&end)?;
    Some(rest[..stop].trim().to_string())
}

/// login shell 里的 PATH（进程存活期只探一次）。终端会话不需要这个——那边
/// shell 由 smeltd 起，自带 login 环境；ACP 子进程是 GUI 直接 spawn 的，得
/// 自己补上 nvm/homebrew 这些用户级 PATH，不然 `npx`/`bunx` 直接 ENOENT。
pub fn login_path() -> &'static str {
    &login_env().path
}

/// login shell 的完整环境快照。
///
/// 给"要跑用户 Node 工具链"的子进程用。**只设 PATH 是不够的**：volta 一类的
/// shim 靠 `VOLTA_HOME` 找工具链，代理设置决定 registry 能不能连上——两者缺失
/// 的表现都不是干净的报错，而是挂起或静默重试，见模块头注释。
pub fn login_environment() -> &'static HashMap<String, String> {
    &login_env().vars
}

/// 把 login shell 环境整体铺到子进程上。
///
/// 保留（而不是 `env_clear`）父进程环境：GUI 自身注入的那些变量（临时目录、
/// 沙盒标识）子进程照样需要。同名的以 login shell 为准——用户在 rc 里写的才是
/// 他真正在用的那套工具链。
pub fn apply_login_environment(command: &mut Command) -> &mut Command {
    for (key, value) in login_environment() {
        command.env(key, value);
    }
    command.env("PATH", login_path())
}

/// Resolve an executable against the login shell PATH rather than Finder's
/// stripped process PATH. GUI actions that directly spawn a helper (rather
/// than going through ACP's launcher) use this for Node-backed DSH tools.
pub fn executable_in_login_path(name: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() || name.contains(std::path::MAIN_SEPARATOR) {
        let path = std::path::PathBuf::from(name);
        return is_executable_file(&path).then_some(path);
    }
    std::env::split_paths(login_path())
        .map(|directory| directory.join(name))
        .find(|path| is_executable_file(path))
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// 四个 workspace 覆盖变量的读取顺序：**先查当前进程自己的环境**，查不到再
/// 退回登录 shell 探测出来的缓存值。
///
/// 为什么不直接只用探测缓存：探测结果在整个进程生命周期只算一次
/// （`OnceLock`），但从终端 `cargo run`/`cargo test` 启动时，进程其实已经
/// 原样继承了当前 shell 的完整环境（包含用户在 `.zshrc` 里 export 的这些
/// 变量）——这种情况下直接查进程自身环境更快、更准，不用等一次 `zsh -ilc`；
/// 只有 Finder/Dock 拉起的 GUI 才会出现"进程环境里没有，得靠探测补"的落差
/// （跟 PATH 是同一个原理，但 PATH 那份要跟安装目录兜底合并成一条完整搜索
/// 路径，逻辑不同，没法共用这个直查优先的简单版本）。
/// 副作用：测试可以直接 `std::env::set_var`/`remove_var` 这几个变量来控制
/// 行为，不受 `OnceLock` 探测缓存污染——这也是选直查优先的实际原因，本仓库
/// 就真的踩过"开发机全局设了 CLAUDE_CONFIG_DIR，historia 测试假 HOME 沙盒
/// 被越过"这个坑。
fn resolve_override(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            login_env()
                .vars
                .get(var)
                .filter(|value| !value.is_empty())
                .cloned()
        })
}

/// Claude Code 自定义 workspace 目录（`CLAUDE_CONFIG_DIR`），没设就是 `None`
/// （调用方回退 `~/.claude`）。同一台机器可能想在不同 workspace 之间切换
/// （比如同时维护 `~/.claude` 和 `~/.claude-quant`）——这里只解决"GUI 进程
/// 能读到当前生效的那个值"，多 workspace 之间怎么选/怎么记还需要更上层的
/// 设置支持（见 acp_conn.rs 里 ConversationLaunch 未来怎么带每次启动各自的覆盖值）。
pub fn claude_config_dir() -> Option<String> {
    resolve_override("CLAUDE_CONFIG_DIR")
}

/// Codex CLI 自定义目录（`CODEX_HOME`），没设就是 `None`（回退 `~/.codex`）。
pub fn codex_home() -> Option<String> {
    resolve_override("CODEX_HOME")
}

/// Grok CLI 自定义目录（`GROK_HOME`），没设就是 `None`（回退 `~/.grok`）。
pub fn grok_home() -> Option<String> {
    resolve_override("GROK_HOME")
}

/// DeepSeek Harness home. Native dsh resolves `$DSH_HOME` first and otherwise
/// uses `~/.dsh`; Smelt must use the same root so profile and session discovery
/// cannot drift from the CLI.
pub fn dsh_home() -> Option<String> {
    resolve_override("DSH_HOME")
}

/// Copilot CLI 自定义目录：`COPILOT_HOME` 优先（官方推荐、整段替换默认路径），
/// 没设再看 `XDG_CONFIG_HOME`（此时基准目录是 `$XDG_CONFIG_HOME/copilot`，
/// 调用方需要自己拼这一段，不是直接可用的完整路径，所以这里分两个访问器）。
pub fn copilot_home() -> Option<String> {
    resolve_override("COPILOT_HOME")
}

pub fn xdg_config_home() -> Option<String> {
    resolve_override("XDG_CONFIG_HOME")
}

#[cfg(test)]
mod tests {
    use super::{extract_marked, parse_env_block};

    /// 交互式 zsh 会在输出前粘一段 shell-integration 的 OSC 噪音（实测形如
    /// `\e]1337;…;shell=zsh`）。抠取逻辑必须只取标记之间的内容——那段噪音里
    /// 就带 `=`，不隔离掉会被当成环境变量解析进来。
    #[test]
    fn strips_shell_integration_noise_before_env_block() {
        let raw = "\u{1b}]1337;RemoteHost=me@host\u{1b}\\\u{1b}]1337;ShellIntegrationVersion=14;shell=zsh\
                   __V_ENV_B__PATH=/Users/me/.grok/bin:/usr/bin:/bin\0VOLTA_HOME=/Users/me/.volta\0__V_ENV_E__";
        let block = extract_marked(raw, "ENV").expect("应能抠出环境块");
        let vars = parse_env_block(&block);
        assert_eq!(
            vars.get("PATH").map(String::as_str),
            Some("/Users/me/.grok/bin:/usr/bin:/bin"),
            "第一段目录不能被 OSC 噪音污染"
        );
        assert_eq!(
            vars.get("VOLTA_HOME").map(String::as_str),
            Some("/Users/me/.volta")
        );
        assert!(
            !vars.contains_key("RemoteHost") && !vars.contains_key("ShellIntegrationVersion"),
            "OSC 噪音里的 `k=v` 不能被当成环境变量：{vars:?}"
        );
    }

    #[test]
    fn returns_none_without_markers() {
        assert!(extract_marked("PATH=/usr/bin:/bin", "ENV").is_none());
        assert!(extract_marked("", "ENV").is_none());
    }

    #[test]
    fn returns_none_when_end_marker_missing() {
        assert!(extract_marked("noise__V_ENV_B__PATH=/usr/bin", "ENV").is_none());
    }

    /// 值里带换行的变量真实存在（证书、`npm_config_*` 的多行配置）。NUL 分隔
    /// 必须让它们原样通过，而不是把续行拆成新变量。
    #[test]
    fn nul_separated_entries_preserve_newlines_inside_values() {
        let vars = parse_env_block("PATH=/bin\0CERT=line1\nline2\0PNPM_HOME=/home/me/.pnpm\0");
        assert_eq!(vars.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(vars.get("CERT").map(String::as_str), Some("line1\nline2"));
        assert_eq!(
            vars.get("PNPM_HOME").map(String::as_str),
            Some("/home/me/.pnpm")
        );
        assert!(!vars.contains_key("line2"), "续行不能变成独立变量");
    }

    /// 个别 shell 会在管道里吞掉 NUL。那时按行解析兜底：多行值还原不了（只能
    /// 丢），但单行值——PATH、代理、各版本管理器的 HOME——仍要拿到，否则整块
    /// 环境作废，又退回"只有 PATH"的老问题。
    #[test]
    fn falls_back_to_line_parsing_when_nul_is_swallowed() {
        let vars =
            parse_env_block("PATH=/bin\nVOLTA_HOME=/home/me/.volta\nHTTPS_PROXY=http://p:8080");
        assert_eq!(vars.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(
            vars.get("VOLTA_HOME").map(String::as_str),
            Some("/home/me/.volta")
        );
        assert_eq!(
            vars.get("HTTPS_PROXY").map(String::as_str),
            Some("http://p:8080")
        );
    }

    /// 非法变量名（多行值的续行、路径片段）不能混进来。
    #[test]
    fn rejects_entries_whose_key_is_not_a_shell_identifier() {
        let vars = parse_env_block("PATH=/bin\n  /some/continuation=x\n9BAD=y\nOK_1=z\nnoequals");
        assert_eq!(vars.get("OK_1").map(String::as_str), Some("z"));
        assert!(!vars.contains_key("9BAD"), "不能以数字开头");
        assert!(!vars.contains_key("/some/continuation"));
        assert!(!vars.contains_key("noequals"));
    }

    /// 这条锁死本次修复的核心：**只补 PATH 对 shim 式管理器不成立**。volta 的
    /// shim 要 `VOLTA_HOME` 才能定位工具链，pnpm 还要 `VOLTA_FEATURE_PNPM`；
    /// 代理变量决定 registry 连不连得上。快照必须把它们一起带出来，否则
    /// `dsh plugin add` 的表现不是报错而是挂起。
    #[test]
    fn snapshot_carries_toolchain_and_proxy_vars_not_just_path() {
        let block = "PATH=/home/me/.volta/bin:/usr/bin\0VOLTA_HOME=/home/me/.volta\0\
                     VOLTA_FEATURE_PNPM=1\0HTTPS_PROXY=http://proxy:8080\0NO_PROXY=localhost\0\
                     FNM_DIR=/home/me/.fnm\0ASDF_DATA_DIR=/home/me/.asdf\0";
        let vars = parse_env_block(block);
        for key in [
            "VOLTA_HOME",
            "VOLTA_FEATURE_PNPM",
            "HTTPS_PROXY",
            "NO_PROXY",
            "FNM_DIR",
            "ASDF_DATA_DIR",
        ] {
            assert!(
                vars.contains_key(key),
                "{key} 必须随快照带出，只传 PATH 会让该管理器/网络配置失效"
            );
        }
    }

    #[test]
    fn detect_user_shell_returns_existing_path() {
        let shell = super::detect_user_shell();
        assert!(!shell.is_empty(), "shell path cannot be empty");
        assert!(
            std::path::Path::new(&shell).exists(),
            "detected shell must exist on system: {shell}"
        );
    }

    #[test]
    fn build_probe_command_produces_non_empty_command() {
        let cmd = super::build_probe_command("/bin/zsh");
        assert_eq!(cmd.get_program(), "/bin/zsh");
        let fish_cmd = super::build_probe_command("/usr/local/bin/fish");
        assert_eq!(fish_cmd.get_program(), "/usr/local/bin/fish");
        let args: Vec<&str> = fish_cmd
            .get_args()
            .map(|a| a.to_str().expect("fish args are utf-8"))
            .collect();
        assert_eq!(&args[..2], ["-l", "-c"], "fish 需要 login + command");
    }

    #[test]
    fn nushell_probe_uses_external_env_command() {
        let cmd = super::build_probe_command("/opt/homebrew/bin/nu");
        assert_eq!(cmd.get_program(), "/opt/homebrew/bin/nu");
        let args: Vec<&str> = cmd
            .get_args()
            .map(|a| a.to_str().expect("nu args are utf-8"))
            .collect();
        assert_eq!(args[0], "--login");
        let script = args[2];
        // nushell 没有内置 `env` 命令，必须用 `^` 强制外部命令，否则拿不到
        // 环境快照。
        assert!(
            script.contains("^/usr/bin/env -0"),
            "nu 需要 `^` 前缀调外部 env：{script}"
        );
        assert!(script.contains("__V_ENV_B__"), "marker 缺失：{script}");
    }

    /// 探测脚本必须用绝对路径调 `env`：这个探测正是用来救 PATH 的，不能反过来
    /// 依赖一条可能已经被 rc 搞坏的 PATH。
    #[test]
    fn probe_script_invokes_env_by_absolute_path() {
        for shell in ["/bin/zsh", "/usr/local/bin/fish", "/opt/homebrew/bin/nu"] {
            let cmd = super::build_probe_command(shell);
            let script = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                script.contains("/usr/bin/env -0"),
                "{shell} 的探测脚本要用绝对路径 env：{script}"
            );
        }
    }
}
