//! `smelt …` 命令行：让人和智能体在终端里操作本机 Smelt。
//!
//! GUI 的 `smelt` 和控制台的 `smelt-cli` 启动时都先交给 [`maybe_run`]，没有子命令时由
//! 它们各自去开窗口。这里只拦截明确的控制命令，一律经 smeltd 读写：库只有运行时一个
//! 写入者，CLI 不绕过它。smeltd 没在跑时直接报错。
//!
//! App 里的功能（开视图、开页面等）经 `smelt app` 调插件开给命令行的命令，运行时转给本机
//! 开着的 App；宿主不认识具体命令。
//!
//! 不开放的：配对设备和远程（等于把 shell 交出去）、需要浏览器的登录和凭据、采纳/删除记忆
//! （运行时只认签过名的桌面，且拒绝智能体会话进程树）、往桌面打字。

rust_i18n::i18n!("locales", fallback = "en");

mod agent;
mod app;
mod automation;
mod conversation;
mod engine;
mod history;
mod pi;
mod workspace;

use rust_i18n::t;
use smelt_model::appearance_settings::SettingsPatch;
use smelt_model::control_api::{NotifySend, NotifySendParams, SettingsGet, SettingsUpdate};
use smelt_runtime_client::protocol::control::{
    ControlCall, ControlClientError, ControlMethod, EmptyParams,
};
use std::collections::BTreeMap;

pub fn maybe_run<I, S>(args: I) -> Option<i32>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect();
    match args.get(1).map(String::as_str) {
        Some(
            "agent" | "app" | "conversation" | "project" | "worktree" | "history" | "automation"
            | "mcp" | "pi" | "model" | "quota" | "engine" | "hooks" | "settings" | "notify"
            | "help" | "-h" | "--help" | "--version" | "-V",
        ) => Some(run(&args)),
        _ => None,
    }
}

/// 命令行没有自己的语言设置，用法、报错和运行时报错的原因都按系统语言说。
pub fn use_system_locale() {
    let locale = sys_locale::get_locale().map_or(smelt_model::locale::FALLBACK_LOCALE, |tag| {
        smelt_model::locale::supported_locale(&tag)
    });
    rust_i18n::set_locale(locale);
}

fn run(args: &[String]) -> i32 {
    use_system_locale();
    let rest = &args[2..];
    match args.get(1).map(String::as_str) {
        Some("--version" | "-V") => {
            println!("{}", smelt_version::VERSION);
            0
        }
        Some("help" | "-h" | "--help") => {
            print_help();
            0
        }
        Some("agent") => agent::run(rest),
        Some("conversation") => conversation::run(rest),
        Some("project") => workspace::run_project(rest),
        Some("worktree") => workspace::run_worktree(rest),
        Some("history") => history::run(rest),
        Some("app") => app::run(rest),
        Some("automation") => automation::run(rest),
        Some("mcp") => pi::run_mcp(rest),
        Some("pi") => pi::run(rest),
        Some("model") => pi::run_model(rest),
        Some("quota") => engine::run_quota(rest),
        Some("engine") => engine::run(rest),
        Some("hooks") => engine::run_hooks(rest),
        Some("settings") => run_settings(rest),
        Some("notify") => match parse_notify(rest) {
            Ok(params) => respond(control::<NotifySend>(params)),
            Err(message) => usage(&message),
        },
        _ => 2,
    }
}

/// Linux 上只有运行时、没有图形界面，不列「启动图形界面」那一行。
pub fn print_help() {
    // Linux 上的 `smelt` 不带界面，也就没有开 App 和 Smelt 插件这两组命令。
    let (gui, plugins) = if cfg!(target_os = "linux") {
        (String::new(), String::new())
    } else {
        (
            format!("  smelt                         {}\n", t!("help.gui")),
            t!("help.plugins").into_owned(),
        )
    };
    eprint!(
        "{}",
        t!(
            "help.text",
            version = smelt_version::VERSION,
            gui = gui,
            plugins = plugins
        )
    );
}

/// `smelt notify [--title <标题>] <内容>`：内容是其余参数用空格连起来。
fn parse_notify(args: &[String]) -> Result<NotifySendParams, String> {
    let mut title = None;
    let mut words = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--title" => {
                index += 1;
                title = Some(
                    args.get(index)
                        .ok_or_else(|| t!("notify.title_missing").into_owned())?
                        .to_string(),
                );
            }
            word => words.push(word),
        }
        index += 1;
    }
    let message = words.join(" ");
    if message.trim().is_empty() {
        return Err(t!("notify.usage").into_owned());
    }
    Ok(NotifySendParams { title, message })
}

/// 一次 Control API 调用。smeltd 没在跑时给出能看懂的原因。
fn control<C: ControlCall>(params: C::Params) -> Result<C::Result, String> {
    control_within::<C>(params, std::time::Duration::from_secs(10))
}

/// 同 [`control`]，给要等网络、git 或包管理器的慢调用一个更长的上限。
fn control_within<C: ControlCall>(
    params: C::Params,
    timeout: std::time::Duration,
) -> Result<C::Result, String> {
    smelt_client::daemon_state::local_runtime()
        .call::<C>(&params, timeout)
        .map_err(|error| match error {
            ControlClientError::Protocol(error) => error.text(),
            ControlClientError::Transport(message)
                if C::METHOD == ControlMethod::SettingsUpdate =>
            {
                t!("control.settings_transport", error = message).into_owned()
            }
            ControlClientError::InvalidResponse(message)
                if C::METHOD == ControlMethod::SettingsUpdate =>
            {
                t!("control.settings_invalid_response", error = message).into_owned()
            }
            ControlClientError::Transport(message) => {
                t!("control.unreachable", error = message).into_owned()
            }
            other => other.to_string(),
        })
}

fn run_settings(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("get") => respond(control::<SettingsGet>(EmptyParams::default())),
        Some("set") => match parse_settings_patch(&args[1..]) {
            Ok(patch) => respond(control::<SettingsUpdate>(patch)),
            Err(message) => usage(&message),
        },
        _ => usage("smelt settings get | smelt settings set key=value ..."),
    }
}

fn parse_settings_patch(args: &[String]) -> Result<SettingsPatch, String> {
    if args.is_empty() {
        return Err("smelt settings set key=value ...".into());
    }
    let mut settings = BTreeMap::new();
    for arg in args {
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| t!("settings.not_key_value", arg = arg).into_owned())?;
        let parsed = if let Ok(number) = value.parse::<u64>() {
            serde_json::json!(number)
        } else if let Ok(number) = value.parse::<f64>() {
            serde_json::json!(number)
        } else {
            serde_json::json!(value)
        };
        settings.insert(key.to_string(), parsed);
    }
    Ok(SettingsPatch { settings })
}

/// 子命令后面的第一个位置参数（id、名称）。
fn id_arg(args: &[String]) -> Option<&str> {
    args.first()
        .map(String::as_str)
        .filter(|id| !id.trim().is_empty() && !id.starts_with("--"))
}

/// 命令行给的路径换成绝对路径：在的话解开软链，和桌面选目录得到的写法一致。
fn absolute(path: &str) -> Result<String, String> {
    let path = std::path::Path::new(path);
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| error.to_string())
}

/// `--flag value` 里的 value。
fn flag_value(args: &[String], index: usize) -> Result<String, String> {
    args.get(index + 1)
        .cloned()
        .ok_or_else(|| t!("flag.value_missing", flag = args[index]).into_owned())
}

fn on_off(flag: &str, value: &str) -> Result<bool, String> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(t!("flag.on_off", flag = flag, value = other).into_owned()),
    }
}

fn respond<T: serde::Serialize>(result: Result<T, String>) -> i32 {
    match result {
        Ok(value) => print_ok(&value),
        Err(error) => fail(&error),
    }
}

fn print_ok(value: &impl serde::Serialize) -> i32 {
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn json_id(id: &str) -> serde_json::Value {
    serde_json::json!({ "id": id })
}

fn usage(message: &str) -> i32 {
    eprintln!("{message}");
    eprintln!("{}", t!("usage_hint"));
    2
}

fn fail(message: &str) -> i32 {
    eprintln!("{message}");
    1
}

#[cfg(test)]
pub(crate) fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| arg.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_and_version_are_cli_commands() {
        assert_eq!(maybe_run(["smelt", "--help"]), Some(0));
        assert_eq!(maybe_run(["smelt", "--version"]), Some(0));
        assert_eq!(maybe_run(["smelt"]), None);
    }

    #[test]
    fn unknown_subcommands_fail_without_touching_smeltd() {
        for group in ["agent", "conversation", "automation", "mcp", "pi", "model"] {
            assert_eq!(maybe_run(["smelt", group, "nope"]), Some(2), "{group}");
        }
        assert_eq!(maybe_run(["smelt", "pi", "skill", "nope"]), Some(2));
        assert_eq!(maybe_run(["smelt", "pi", "extension", "nope"]), Some(2));
    }

    #[test]
    fn smelt_plugins_are_left_to_the_desktop_app() {
        // `smelt plugin` 是 Smelt 自己的界面插件，由桌面程序处理；Pi 的技能和扩展在 `smelt pi`。
        assert_eq!(maybe_run(["smelt", "plugin", "status"]), None);
    }

    #[test]
    fn notify_joins_words_and_takes_a_title() {
        assert_eq!(
            parse_notify(&args(&["--title", "巡检", "磁盘", "告警"])).unwrap(),
            NotifySendParams {
                title: Some("巡检".into()),
                message: "磁盘 告警".into(),
            }
        );
        assert_eq!(parse_notify(&args(&["好了"])).unwrap().title, None);
        assert!(parse_notify(&args(&[])).is_err());
        assert!(parse_notify(&args(&["--title", "只有标题"])).is_err());
        assert!(parse_notify(&args(&["内容", "--title"])).is_err());
        assert_eq!(maybe_run(["smelt", "notify"]), Some(2), "没内容不碰 smeltd");
    }

    #[test]
    fn settings_set_parses_typed_values() {
        let patch = parse_settings_patch(&args(&[
            "appearance.theme_mode=light",
            "appearance.ui_font_px=18",
        ]))
        .unwrap();
        assert_eq!(
            patch.settings.get("appearance.theme_mode"),
            Some(&serde_json::json!("light"))
        );
        assert_eq!(
            patch.settings.get("appearance.ui_font_px"),
            Some(&serde_json::json!(18))
        );
    }
}
