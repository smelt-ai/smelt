//! 登录环境探测：
//!
//! - macOS / Linux：GUI 是 Finder/Dock 拉起的进程，读不到用户 `.zshrc` 里 `export` 的值。
//!   一次 `zsh -ilc` 探测把**整个环境**导出来（`env -0`），不止补 PATH（nvm/homebrew），
//!   还带上 `VOLTA_HOME`、`FNM_*`、`HTTPS_PROXY` 等变量和各家 agent 的覆盖目录
//!   （`CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `GROK_HOME` / `COPILOT_HOME` /
//!   `XDG_CONFIG_HOME`）。
//! - Windows：GUI / `smeltd` 从桌面拉起时只继承父进程（如 `explorer.exe`）的环境块快照，
//!   修改系统/用户环境变量后不会自动刷新。这里每次现读注册表
//!   `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` 与
//!   `HKCU\Environment`，展开 `REG_EXPAND_SZ` 里的 `%VAR%` 并合并 `PATH`。

use std::borrow::Cow;
use std::collections::HashMap;
use std::process::Command;
#[cfg(any(not(windows), test))]
use std::process::Stdio;
#[cfg(not(windows))]
use std::sync::OnceLock;
#[cfg(not(windows))]
use std::time::Duration;

#[cfg(not(windows))]
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 环境块的 marker 名。整个 `env -0` 输出被包在这一对标记之间。
#[cfg(any(not(windows), test))]
const ENV_MARKER: &str = "ENV";

#[derive(Clone, Default)]
struct LoginEnv {
    /// PATH 单独留一份：它是唯一保证有值的（进程自身 PATH 兜底），
    /// 且 `login_path()` 要返回切片/引用。
    path: String,
    vars: HashMap<String, String>,
}

#[cfg(not(windows))]
fn cached_login_env() -> &'static LoginEnv {
    static ENV: OnceLock<LoginEnv> = OnceLock::new();
    ENV.get_or_init(probe)
}

fn current_login_env() -> Cow<'static, LoginEnv> {
    #[cfg(windows)]
    {
        Cow::Owned(windows_env::probe_windows_env())
    }
    #[cfg(not(windows))]
    {
        Cow::Borrowed(cached_login_env())
    }
}

/// 权威探测用户的默认登录 Shell：优先从 `$SHELL` 环境变量取，
/// 其次通过 `libc::getpwuid` 从系统目录服务获取，最后回退 `/bin/zsh`。
/// Windows 没有这套目录服务，用 `COMSPEC`（通常是 `cmd.exe`）。
#[cfg(any(not(windows), test))]
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
    #[cfg(all(windows, test))]
    if let Some(shell) = windows_command_shell() {
        return shell;
    }
    "/bin/zsh".to_string()
}

/// `cmd.exe` 跑不了 `zsh -ilc` 那条探测脚本。只有名字像 Unix shell 时才去探。
#[cfg(not(windows))]
fn unix_login_shell(shell: &str) -> bool {
    let name = std::path::Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(shell);
    let name = name
        .rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("exe"))
        .map(|(stem, _)| stem)
        .unwrap_or(name);
    matches!(
        name.to_ascii_lowercase().as_str(),
        "zsh" | "bash" | "sh" | "fish" | "nu" | "ksh" | "dash" | "ash" | "csh" | "tcsh"
    )
}

#[cfg(all(windows, test))]
fn windows_command_shell() -> Option<String> {
    if let Some(shell) = std::env::var("COMSPEC")
        .ok()
        .filter(|shell| std::path::Path::new(shell).is_file())
    {
        return Some(shell);
    }
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let cmd = std::path::Path::new(&root).join(r"System32\cmd.exe");
    cmd.is_file().then(|| cmd.to_string_lossy().into_owned())
}

#[cfg(any(not(windows), test))]
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

#[cfg(not(windows))]
fn run_probe_with_timeout(shell: &str) -> Option<String> {
    let child = build_probe_command(shell).spawn().ok()?;
    let pid = child.id();
    #[cfg(not(unix))]
    let _ = pid;
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

#[cfg(not(windows))]
fn probe() -> LoginEnv {
    let shell = detect_user_shell();
    // Windows 上的 `cmd.exe` 不是 login shell。硬跑 `-ilc` 既拿不到环境，
    // 超时后也杀不掉子进程。探测失败时下面会退回进程自己的 PATH。
    let raw = if cfg!(unix) || unix_login_shell(&shell) {
        run_probe_with_timeout(&shell)
    } else {
        None
    };

    let mut vars = raw
        .as_deref()
        .and_then(|raw| extract_marked(raw, ENV_MARKER))
        .map(|block| parse_env_block(&block))
        .unwrap_or_default();

    // PATH 兜底：探测失败（shell 起不来/超时）也必须有一条能用的搜索路径，
    // 否则连 `npx` 这类工具都解析不出来。
    let path = vars
        .get("PATH")
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let bin = smelt_paths::smelt_home().map(|home| home.join("bin"));
    let path = with_smelt_bin(&path, bin.as_deref());
    vars.insert("PATH".to_string(), path.clone());

    LoginEnv { path, vars }
}

/// 把 `~/.smelt/bin` 放到 PATH 最前面：智能体和它起的 shell 里直接敲 `smelt` 就能用内置的
/// `smelt` 技能，不靠用户自己把它加进 PATH。用的是跟这个运行时配套的那份：macOS 上是桌面
/// 放的、指向 App 的软链接，Linux 上是安装包装进来的 CLI。
fn with_smelt_bin(path: &str, bin: Option<&std::path::Path>) -> String {
    let Some(bin) = bin else {
        return path.to_string();
    };
    let rest = std::env::split_paths(path).filter(|entry| entry != bin);
    std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(rest))
        .map(|joined| joined.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// 把 `env -0` 的输出解析成 KV。
///
/// NUL 分隔是唯一能安全承载"值里带换行"的形式（`npm_config_*`、证书类变量真的
/// 会有多行值）。但个别 shell 会在管道里把 NUL 吞掉，那时退回按行解析——多行值
/// 的续行还原不了，只能丢弃，可单行值（PATH、代理、各管理器的 HOME）仍能拿到，
/// 比整块放弃强。
#[cfg(any(not(windows), test))]
fn parse_env_block(block: &str) -> HashMap<String, String> {
    if block.contains('\0') {
        block.split('\0').filter_map(split_env_entry).collect()
    } else {
        block.lines().filter_map(split_env_entry).collect()
    }
}

/// 拆 `KEY=VALUE`。只认合法的 shell 变量名，借此丢掉按行解析时混进来的多行值
/// 续行（那些行不是 `合法名=` 开头）。
#[cfg(any(not(windows), test))]
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
#[cfg(any(not(windows), test))]
fn extract_marked(raw: &str, name: &str) -> Option<String> {
    let begin = format!("__V_{name}_B__");
    let end = format!("__V_{name}_E__");
    let start = raw.find(&begin)? + begin.len();
    let rest = &raw[start..];
    let stop = rest.find(&end)?;
    Some(rest[..stop].trim().to_string())
}

#[cfg(any(windows, test))]
mod windows_env {
    use std::collections::{HashMap, HashSet};

    #[derive(Clone, Debug)]
    pub(super) struct WindowsRegEntry {
        pub(super) name: String,
        pub(super) value: String,
        pub(super) expand: bool,
    }

    pub(super) fn get_ci<'a>(map: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
        map.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn lookup_ci<'a>(vars: &'a [(String, String)], name: &str) -> Option<&'a str> {
        vars.iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn is_machine_session_var(name: &str) -> bool {
        name.eq_ignore_ascii_case("USERNAME")
            || name.eq_ignore_ascii_case("TEMP")
            || name.eq_ignore_ascii_case("TMP")
    }

    /// 展开 Windows `REG_EXPAND_SZ` 里的 `%VAR%` 占位符；找不到、空 `%%` 或未闭合的 `%` 原样保留。
    pub(super) fn expand_windows_env_vars(
        input: &str,
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> String {
        let mut out = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(start) = rest.find('%') {
            out.push_str(&rest[..start]);
            let after_open = &rest[start + 1..];
            let Some(end) = after_open.find('%') else {
                out.push('%');
                rest = after_open;
                break;
            };
            let var = &after_open[..end];
            if var.is_empty() {
                out.push('%');
                rest = after_open;
            } else if let Some(value) = lookup(var) {
                out.push_str(&value);
                rest = &after_open[end + 1..];
            } else {
                out.push('%');
                out.push_str(var);
                out.push('%');
                rest = &after_open[end + 1..];
            }
        }
        out.push_str(rest);
        out
    }

    fn windows_path_dedup_key(segment: &str) -> String {
        let trimmed = segment.trim();
        let unquoted = trimmed
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(trimmed);
        let mut normalized = unquoted.replace('/', "\\");
        while normalized.ends_with('\\') {
            let bytes = normalized.as_bytes();
            let is_drive_root =
                bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
            if normalized.len() == 1 || is_drive_root {
                break;
            }
            normalized.pop();
        }
        normalized.to_ascii_lowercase()
    }

    /// 合并 Windows `PATH`：`Machine` → `User` → 当前进程残留，按大小写和尾斜杠去重。
    pub(super) fn merge_windows_path(
        machine: Option<&str>,
        user: Option<&str>,
        process: Option<&str>,
    ) -> String {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for source in [machine, user, process].into_iter().flatten() {
            for part in source.split(';') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let key = windows_path_dedup_key(part);
                if key.is_empty() || !seen.insert(key) {
                    continue;
                }
                out.push(part);
            }
        }
        out.join(";")
    }

    /// 从注册表系统/用户条目与当前进程环境组装 Windows 登录环境。
    pub(super) fn build_windows_login_env(
        machine: &[WindowsRegEntry],
        user: &[WindowsRegEntry],
        process_vars: &[(String, String)],
    ) -> super::LoginEnv {
        struct Pending {
            name: String,
            value: String,
            expand: bool,
        }

        let mut merged: HashMap<String, Pending> = HashMap::new();
        for entry in machine {
            let name = entry.name.trim();
            if name.is_empty()
                || name.eq_ignore_ascii_case("PATH")
                || is_machine_session_var(name)
            {
                continue;
            }
            merged.insert(
                name.to_ascii_lowercase(),
                Pending {
                    name: name.to_string(),
                    value: entry.value.clone(),
                    expand: entry.expand,
                },
            );
        }
        for entry in user {
            let name = entry.name.trim();
            if name.is_empty() || name.eq_ignore_ascii_case("PATH") {
                continue;
            }
            merged.insert(
                name.to_ascii_lowercase(),
                Pending {
                    name: name.to_string(),
                    value: entry.value.clone(),
                    expand: entry.expand,
                },
            );
        }

        // 两轮展开 `REG_EXPAND_SZ`：与注册表枚举顺序无关，且允许最多两级 `%VAR%` 引用。
        let mut values: HashMap<String, String> = merged
            .iter()
            .map(|(lower, entry)| (lower.clone(), entry.value.clone()))
            .collect();
        for _ in 0..2 {
            let mut next = values.clone();
            for (lower, entry) in &merged {
                if !entry.expand {
                    continue;
                }
                let current = values.get(lower).map(String::as_str).unwrap_or("");
                let expanded = expand_windows_env_vars(current, &|var| {
                    let var_lower = var.to_ascii_lowercase();
                    if var_lower == *lower {
                        return lookup_ci(process_vars, var).map(str::to_string);
                    }
                    values
                        .get(&var_lower)
                        .map(String::as_str)
                        .or_else(|| lookup_ci(process_vars, var))
                        .map(str::to_string)
                });
                next.insert(lower.clone(), expanded);
            }
            values = next;
        }

        let mut vars = HashMap::with_capacity(merged.len() + 1);
        for (lower, entry) in merged {
            if let Some(value) = values.remove(&lower) {
                vars.insert(entry.name, value);
            }
        }

        let expand_path = |entry: &WindowsRegEntry| -> String {
            if !entry.expand {
                return entry.value.clone();
            }
            expand_windows_env_vars(&entry.value, &|var| {
                if var.eq_ignore_ascii_case("PATH") {
                    return None;
                }
                get_ci(&vars, var)
                    .or_else(|| lookup_ci(process_vars, var))
                    .map(str::to_string)
            })
        };
        let machine_path = machine
            .iter()
            .rfind(|entry| entry.name.trim().eq_ignore_ascii_case("PATH"))
            .map(expand_path);
        let user_path = user
            .iter()
            .rfind(|entry| entry.name.trim().eq_ignore_ascii_case("PATH"))
            .map(expand_path);
        let path = merge_windows_path(
            machine_path.as_deref(),
            user_path.as_deref(),
            lookup_ci(process_vars, "PATH"),
        );
        vars.insert("PATH".to_string(), path.clone());
        super::LoginEnv { path, vars }
    }

    #[cfg(windows)]
    fn read_registry_key(root: &windows_registry::Key, subkey: &str) -> Vec<WindowsRegEntry> {
        let Ok(key) = root.open(subkey) else {
            return Vec::new();
        };
        let Ok(values) = key.values() else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        for (name, value) in values {
            let expand = matches!(value.ty(), windows_registry::Type::ExpandString);
            if let Ok(text) = String::try_from(value) {
                entries.push(WindowsRegEntry {
                    name,
                    value: text,
                    expand,
                });
            }
        }
        entries
    }

    #[cfg(windows)]
    pub(super) fn probe_windows_env() -> super::LoginEnv {
        let machine = read_registry_key(
            windows_registry::LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        );
        let user = read_registry_key(windows_registry::CURRENT_USER, "Environment");
        let process_vars: Vec<(String, String)> = std::env::vars().collect();
        let mut env = build_windows_login_env(&machine, &user, &process_vars);
        let bin = smelt_paths::smelt_home().map(|home| home.join("bin"));
        env.path = super::with_smelt_bin(&env.path, bin.as_deref());
        env.vars.insert("PATH".to_string(), env.path.clone());
        env
    }
}

/// 用户登录环境的 PATH：Unix 缓存一次 login shell；Windows 每次现读注册表合并最新结果。
pub fn login_path() -> Cow<'static, str> {
    #[cfg(windows)]
    {
        Cow::Owned(windows_env::probe_windows_env().path)
    }
    #[cfg(not(windows))]
    {
        Cow::Borrowed(&cached_login_env().path)
    }
}

/// 用户登录环境的完整变量快照（Windows 为最新注册表系统 + 用户变量）。
///
/// 给"要跑用户 Node 工具链"的子进程用。**只设 PATH 是不够的**：volta 一类的
/// shim 靠 `VOLTA_HOME` 找工具链，代理设置决定 registry 能不能连上——两者缺失
/// 的表现都不是干净的报错，而是挂起或静默重试，见模块头注释。
pub fn login_environment() -> Cow<'static, HashMap<String, String>> {
    #[cfg(windows)]
    {
        Cow::Owned(windows_env::probe_windows_env().vars)
    }
    #[cfg(not(windows))]
    {
        Cow::Borrowed(&cached_login_env().vars)
    }
}

/// 把 login shell 环境整体铺到子进程上。
///
/// 保留（而不是 `env_clear`）父进程环境：GUI 自身注入的那些变量（临时目录、
/// 沙盒标识）子进程照样需要。同名的以 login shell / 注册表为准——用户在 rc 或
/// 系统设置里写的才是他真正在用的那套工具链。npm 官方源连不上、用户又没配源时
/// 补上国内镜像，见 `mirrors`。
///
/// 顺序是「login 环境 → 调用方覆盖 `overrides`（启动配置、ACP 规格里的 env，含 PATH）→
/// 用户只在系统设置里配了代理时补 `HTTP(S)_PROXY` / `NO_PROXY`」，见 `system_proxy`。
/// 调用方自己的 env 一律经 `overrides` 传进来，不要在返回后再铺：补系统代理时要看到
/// 子进程最终的环境，启动配置里写了 `ALL_PROXY` 就不能再补 `HTTPS_PROXY` 把它盖掉。
pub fn apply_login_environment<'a>(
    command: &mut Command,
    overrides: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> &mut Command {
    apply_environment(command, overrides, crate::system_proxy::read_system_proxy)
}

fn apply_environment<'a>(
    command: &mut Command,
    overrides: impl IntoIterator<Item = (&'a str, &'a str)>,
    read_system_proxy: impl FnOnce() -> Option<crate::system_proxy::SystemProxy>,
) -> &mut Command {
    let login = current_login_env();
    for (key, value) in &login.vars {
        command.env(key, value);
    }
    if let Some(registry) = crate::mirrors::npm_registry_fallback() {
        command.env("NPM_CONFIG_REGISTRY", registry);
    }
    command.env("PATH", &login.path);
    for (key, value) in overrides {
        command.env(key, value);
    }
    crate::system_proxy::apply_system_proxy_fallback(command, read_system_proxy);
    command
}

/// 四个 workspace 覆盖变量的读取顺序：**先查当前进程自己的环境**，查不到再
/// 退回登录环境探测出来的缓存值/注册表值。
fn resolve_override(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let env = current_login_env();
            #[cfg(windows)]
            let value = windows_env::get_ci(&env.vars, var).map(str::to_string);
            #[cfg(not(windows))]
            let value = env.vars.get(var).cloned();
            value.filter(|value| !value.is_empty())
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
    use super::{extract_marked, parse_env_block, with_smelt_bin};

    #[test]
    fn smelt_bin_goes_first_once() {
        let bin = std::path::PathBuf::from("/home/me/.smelt/bin");
        let join = |dirs: &[&str]| {
            std::env::join_paths(dirs)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(
            with_smelt_bin(&join(&["/usr/bin", "/bin"]), Some(&bin)),
            join(&["/home/me/.smelt/bin", "/usr/bin", "/bin"])
        );
        // 用户自己加过的，挪到最前面，不留两份。
        assert_eq!(
            with_smelt_bin(&join(&["/usr/bin", "/home/me/.smelt/bin"]), Some(&bin)),
            join(&["/home/me/.smelt/bin", "/usr/bin"])
        );
        assert_eq!(with_smelt_bin("/usr/bin", None), "/usr/bin");
    }

    mod system_proxy_order {
        use super::super::apply_environment;
        use crate::system_proxy::SystemProxy;
        use std::process::Command;

        fn system_proxy() -> Option<SystemProxy> {
            Some(SystemProxy {
                http: Some("http://sys:8080".to_string()),
                https: Some("http://sys:8080".to_string()),
                bypass: Vec::new(),
            })
        }

        /// Windows 环境变量名不分大小写：先设了 `HTTP_PROXY` 再设 `http_proxy`，`Command` 里只留一项、
        /// 键名是先设的那个。
        fn env_of(command: &Command, name: &str) -> Option<String> {
            command
                .get_envs()
                .filter(|(key, _)| {
                    if cfg!(windows) {
                        key.eq_ignore_ascii_case(name)
                    } else {
                        *key == name
                    }
                })
                .last()
                .and_then(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        }

        /// 启动配置里写了 `ALL_PROXY`：判断时要看得到它，不能再补 `HTTPS_PROXY` 把它盖掉。
        #[test]
        fn caller_all_proxy_suppresses_system_https_proxy() {
            let mut command = Command::new("x");
            apply_environment(
                &mut command,
                [("ALL_PROXY", "socks5://mine:1080")],
                system_proxy,
            );
            assert_eq!(
                env_of(&command, "ALL_PROXY").as_deref(),
                Some("socks5://mine:1080")
            );
            assert_ne!(
                env_of(&command, "HTTPS_PROXY").as_deref(),
                Some("http://sys:8080")
            );
            assert_ne!(
                env_of(&command, "HTTP_PROXY").as_deref(),
                Some("http://sys:8080")
            );
        }

        /// 调用方没配（空串算没配）时照常补上系统代理。显式置空是为了不受跑测试那台机器
        /// login 环境里的代理变量影响。
        #[test]
        fn system_proxy_fills_in_after_caller_overrides() {
            let mut command = Command::new("x");
            apply_environment(
                &mut command,
                [
                    ("HTTPS_PROXY", ""),
                    ("https_proxy", ""),
                    ("HTTP_PROXY", ""),
                    ("http_proxy", ""),
                    ("ALL_PROXY", ""),
                    ("all_proxy", ""),
                ],
                system_proxy,
            );
            assert_eq!(
                env_of(&command, "HTTPS_PROXY").as_deref(),
                Some("http://sys:8080")
            );
            assert_eq!(
                env_of(&command, "http_proxy").as_deref(),
                Some("http://sys:8080")
            );
        }
    }

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
    /// 拉包的命令表现不是报错而是挂起。
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

    mod windows_registry_env {
        use super::super::windows_env::{
            WindowsRegEntry, build_windows_login_env, expand_windows_env_vars, get_ci,
        };

        fn reg(name: &str, value: &str, expand: bool) -> WindowsRegEntry {
            WindowsRegEntry {
                name: name.to_string(),
                value: value.to_string(),
                expand,
            }
        }

        #[test]
        fn expand_windows_env_vars_handles_known_unknown_and_edge_cases() {
            let lookup = |name: &str| match name.to_ascii_uppercase().as_str() {
                "SYSTEMROOT" => Some(r"C:\WINDOWS".to_string()),
                "A" => Some("alpha".to_string()),
                "B" => Some("beta".to_string()),
                "TEMP" => Some(r"C:\Temp".to_string()),
                _ => None,
            };
            assert_eq!(
                expand_windows_env_vars(r"%systemroot%\system32", &lookup),
                r"C:\WINDOWS\system32"
            );
            assert_eq!(expand_windows_env_vars("%A%%B%", &lookup), "alphabeta");
            assert_eq!(
                expand_windows_env_vars("%MISSING%%TEMP%", &lookup),
                r"%MISSING%C:\Temp"
            );
            assert_eq!(
                expand_windows_env_vars("%MISSING%B%TEMP%", &lookup),
                r"%MISSING%BC:\Temp"
            );
            assert_eq!(expand_windows_env_vars("%% 100%", &lookup), "%% 100%");
        }

        #[test]
        fn build_windows_login_env_merges_and_expands_registry_without_session_clobber() {
            let machine = [
                reg("USERNAME", "SYSTEM", false),
                reg("TEMP", r"%SystemRoot%\TEMP", true),
                reg("TMP", r"%SystemRoot%\TEMP", true),
                reg("OS", "Windows_NT", false),
                reg("custom_upper", "from_machine", false),
                reg("LITERAL_SZ", r"%USERPROFILE%\literal", false),
            ];
            let user = [
                reg("TEMP", r"%USERPROFILE%\AppData\Local\Temp", true),
                reg("CUSTOM_UPPER", "from_user", false),
                reg("LEVEL_TWO", r"%LEVEL_ONE%\sub", true),
                reg("LEVEL_ONE", r"%USERPROFILE%\tools", true),
            ];
            let process_vars = vec![
                ("SystemRoot".to_string(), r"C:\WINDOWS".to_string()),
                ("USERPROFILE".to_string(), r"C:\Users\alice".to_string()),
                ("USERNAME".to_string(), "alice".to_string()),
            ];

            let env = build_windows_login_env(&machine, &user, &process_vars);
            assert_eq!(get_ci(&env.vars, "USERNAME"), None);
            assert_eq!(get_ci(&env.vars, "TMP"), None);
            assert_eq!(
                get_ci(&env.vars, "TEMP"),
                Some(r"C:\Users\alice\AppData\Local\Temp")
            );
            assert_eq!(get_ci(&env.vars, "OS"), Some("Windows_NT"));
            assert_eq!(get_ci(&env.vars, "custom_upper"), Some("from_user"));
            assert_eq!(
                env.vars
                    .keys()
                    .filter(|k| k.eq_ignore_ascii_case("CUSTOM_UPPER"))
                    .count(),
                1
            );
            assert_eq!(
                get_ci(&env.vars, "LITERAL_SZ"),
                Some(r"%USERPROFILE%\literal")
            );
            assert_eq!(
                get_ci(&env.vars, "LEVEL_TWO"),
                Some(r"C:\Users\alice\tools\sub")
            );
        }

        #[test]
        fn build_windows_login_env_merges_and_deduplicates_paths() {
            let machine = [reg("Path", r"%SystemRoot%\system32;E:\Tools\", true)];
            let user = [
                reg("USER_BIN", r"%USERPROFILE%\bin", true),
                reg("Path", r"%USER_BIN%;E:/tools;E:\AI\tools\gh\bin;;C:\", true),
            ];
            let process_vars = vec![
                ("SystemRoot".to_string(), r"C:\WINDOWS".to_string()),
                ("USERPROFILE".to_string(), r"C:\Users\alice".to_string()),
                (
                    "Path".to_string(),
                    r"C:\WINDOWS\System32;E:\AI\tools\gh\bin\;C:/;D:\extra".to_string(),
                ),
            ];

            let env = build_windows_login_env(&machine, &user, &process_vars);
            assert_eq!(
                env.path,
                r"C:\WINDOWS\system32;E:\Tools\;C:\Users\alice\bin;E:\AI\tools\gh\bin;C:\;D:\extra"
            );
            assert_eq!(env.vars.get("PATH").map(String::as_str), Some(env.path.as_str()));
            assert_eq!(
                env.vars
                    .keys()
                    .filter(|k| k.eq_ignore_ascii_case("PATH"))
                    .count(),
                1
            );
        }
    }
}
