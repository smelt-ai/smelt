//! `smelt …` 命令行：让人和智能体在终端里操作本机 Smelt。
//!
//! GUI 的 `smelt` 和控制台的 `smelt-cli` 启动时都先交给 [`maybe_run`]，没有子命令时由
//! 它们各自去开窗口。这里只拦截明确的控制命令，一律经 smeltd 读写：库只有运行时一个
//! 写入者，CLI 不绕过它。smeltd 没在跑时直接报错。
//!
//! 不开放的：配对设备和远程（等于把 shell 交出去）、需要浏览器的登录、采纳/删除记忆
//! （运行时对智能体进程树拒绝）、钩子和往桌面打字。

mod agent;
mod automation;
mod conversation;
mod pi;

use smelt_model::appearance_settings::SettingsPatch;
use smelt_model::control_api::{QuotaGet, SettingsGet, SettingsUpdate};
use smelt_runtime_client::protocol::control::{ControlCall, ControlClientError, EmptyParams};
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
            "agent" | "conversation" | "automation" | "mcp" | "plugin" | "model" | "quota"
            | "settings" | "help" | "-h" | "--help" | "--version" | "-V",
        ) => Some(run(&args)),
        _ => None,
    }
}

fn run(args: &[String]) -> i32 {
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
        Some("automation") => automation::run(rest),
        Some("mcp") => pi::run_mcp(rest),
        Some("plugin") => pi::run_plugin(rest),
        Some("model") => pi::run_model(rest),
        Some("quota") => respond(control::<QuotaGet>(EmptyParams::default())),
        Some("settings") => run_settings(rest),
        _ => 2,
    }
}

/// Linux 上只有运行时、没有图形界面，不列「启动图形界面」那一行。
pub fn print_help() {
    let gui = if cfg!(target_os = "linux") {
        ""
    } else {
        "  smelt                         启动图形界面\n"
    };
    eprint!(
        "\
smelt {version}

用法（stdout 为 JSON，都需要 smeltd 在跑）：
{gui}
智能体（长期定义，不是正在跑的对话）：
  smelt agent list | get <id> | delete <id>
  smelt agent create --name <名称> [选项]
  smelt agent update <id> [选项]          列表类选项整份替换，写 \"\" 清空
      --prompt <工作方式>  --engine <id>  --memory on|off
      --plugin <id>...  --mcp <服务器名>...  --folder <路径>...  --link <地址>...
      --model <provider>/<模型id> | --model default
  smelt agent chat <id> [--message <第一句话>]   用该定义开一场对话

对话：
  smelt conversation list [--agent <智能体id>] [--archived]
  smelt conversation rename <id> --title <标题>
  smelt conversation archive|unarchive|pin|unpin|delete <id>

自动化：
  smelt automation list | get <id>
  smelt automation create --name <名称> (--agent <智能体id> [--prompt <指令>] | --shell <命令>) <触发>... [--notify on|off]
  smelt automation update <id> [--name ...] [--agent ...] [--prompt ...] [--shell ...] [<触发>...] [--notify on|off]
      触发（可以组合）：
      --every-minutes N  --every-hours N  --daily HH:MM
      --weekly mon,wed,fri@HH:MM          --during 30@09:00-18:00[@weekdays]
      --webhook  --event <主题> [--event-filter '{{\"字段\":\"值\"}}']
      update 时：给了定时就整组换掉定时，--event 同理，--webhook 已有就保留原地址；
      --no-schedule / --no-webhook / --no-event 去掉那一类
  smelt automation enable|disable|run|delete <id>
  smelt automation runs [<id>] [--limit N]     最近的运行
  smelt automation run-log <run_id>            一次运行的输出和对话记录
  smelt automation cancel <run_id>

能力（对所有智能体生效）：
  smelt mcp list | presets | add <预设id> | enable|disable|remove <服务器名>
  smelt plugin list | import --skill|--extension <路径> | remove <id>
  smelt model list | default <provider>/<模型id> | thinking off|minimal|low|medium|high|xhigh
  smelt quota

设置：
  smelt settings get
  smelt settings set appearance.theme_mode=light appearance.ui_font_px=16
  smelt settings set appearance.theme=\"Ayu Dark\"
  smelt settings set memory.embedding_base_url=http://localhost:11434/v1 memory.embedding_model=bge-m3
",
        version = smelt_version::VERSION,
    );
}

/// 一次 Control API 调用。smeltd 没在跑时给出能看懂的原因。
fn control<C: ControlCall>(params: C::Params) -> Result<C::Result, String> {
    smelt_client::daemon_state::local_runtime()
        .call::<C>(&params, std::time::Duration::from_secs(10))
        .map_err(|error| match error {
            ControlClientError::Protocol(error) => error.message,
            ControlClientError::Transport(message) => {
                format!("连不上 smeltd（需要它在运行）：{message}")
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
            .ok_or_else(|| format!("设置项必须是 key=value: {arg}"))?;
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

/// `--flag value` 里的 value。
fn flag_value(args: &[String], index: usize) -> Result<String, String> {
    args.get(index + 1)
        .cloned()
        .ok_or_else(|| format!("{} 需要一个值", args[index]))
}

fn on_off(flag: &str, value: &str) -> Result<bool, String> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(format!("{flag} 只能是 on 或 off，收到 {other}")),
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
    eprintln!("用法见 smelt --help");
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
        assert_eq!(maybe_run(["smelt", "--install-app"]), None);
    }

    #[test]
    fn unknown_subcommands_fail_without_touching_smeltd() {
        for group in [
            "agent",
            "conversation",
            "automation",
            "mcp",
            "plugin",
            "model",
        ] {
            assert_eq!(maybe_run(["smelt", group, "nope"]), Some(2), "{group}");
        }
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
