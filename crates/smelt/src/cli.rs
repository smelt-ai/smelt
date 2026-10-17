//! `smelt agent …`：产品智能体定义的 CLI。
//!
//! 无子命令时仍启动 GUI。这里只拦截明确的控制命令，一律经 smeltd 的 Control API
//! 读写：库只有运行时一个写入者，CLI 不绕过它。smeltd 没在跑时直接报错。

use smelt_model::agent_definition_store::{AgentDefinitionCreate, AgentDefinitionPatch};
use smelt_model::appearance_settings::SettingsPatch;
use smelt_model::automation::{
    Automation, AutomationAction, AutomationCommand, AutomationSchedule, AutomationTrigger,
    redact_automation_credentials,
};
use smelt_model::control_api::{
    AgentDefinitionCreateCall, AgentDefinitionDelete, AgentDefinitionGet, AgentDefinitionIdParams,
    AgentDefinitionList, AgentDefinitionUpdateCall, AgentDefinitionUpdateParams, AutomationGet,
    AutomationList, SettingsGet, SettingsUpdate,
};
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
            "agent" | "automation" | "settings" | "help" | "-h" | "--help" | "--version" | "-V",
        ) => Some(run(&args)),
        _ => None,
    }
}

fn run(args: &[String]) -> i32 {
    // 技能写在运行时那台机器上；运行时没在跑时后面的命令也会报连不上，这里不另报。
    let _ = control::<smelt_model::control_api::SkillsSyncControl>(EmptyParams::default());
    match args.get(1).map(String::as_str) {
        Some("--version" | "-V") => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("help" | "-h" | "--help") => {
            print_help();
            0
        }
        Some("agent") => run_agent(&args[2..]),
        Some("automation") => run_automation(&args[2..]),
        Some("settings") => run_settings(&args[2..]),
        _ => 2,
    }
}

fn print_help() {
    eprint!(
        "\
smelt {version}

用法：
  smelt                         启动图形界面
  smelt agent list|get|create|update|delete|chat|conversations|conversation-rename|plugins
  smelt automation list|get|create|update|enable|disable|run|delete
  smelt settings get
  smelt settings set appearance.theme_mode=light appearance.ui_font_px=16
  smelt settings set appearance.theme=\"Ayu Dark\"
  smelt settings set memory.embedding_base_url=http://localhost:11434/v1 memory.embedding_model=bge-m3

智能体：
  smelt agent create --name <名称> [--prompt <工作方式>] [--plugin <id>]... [--memory on|off]
  smelt agent update <id> [--name <名称>] [--prompt <工作方式>] [--memory on|off]

自动化（写操作需要 smeltd）：
  smelt automation create --name <名称> --agent <智能体id> --every-hours 1 [--prompt <单次输入>]
  smelt automation create --name <名称> --agent <智能体id> --daily 09:30
  smelt automation create --name <名称> --agent <智能体id> --webhook
  smelt automation update <id> [--name ...] [--prompt ...] [--every-hours N]
  smelt automation enable|disable|run|delete <id>
  smelt agent chat <id>              用该定义开一场对话（需要 smeltd）
  smelt agent conversations          列出产品对话（需要 smeltd）
  smelt agent conversation-rename <session_id> --title <标题>
  smelt agent plugins                列出可绑到智能体的 Pi 插件

stdout 为 JSON。
",
        version = env!("CARGO_PKG_VERSION")
    );
}

fn run_agent(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("list") => match control::<AgentDefinitionList>(EmptyParams::default()) {
            Ok(result) => print_ok(&serde_json::json!({ "agents": result.agents })),
            Err(error) => fail(&error),
        },
        Some("get") => {
            let Some(id) = args.get(1).map(String::as_str).filter(|id| !id.is_empty()) else {
                return usage("smelt agent get <id>");
            };
            match control::<AgentDefinitionGet>(AgentDefinitionIdParams { id: id.to_string() }) {
                Ok(result) => print_ok(&result.agent),
                Err(error) => fail(&error),
            }
        }
        Some("create") => match parse_create(&args[1..]) {
            Ok(input) => match control::<AgentDefinitionCreateCall>(input) {
                Ok(result) => print_ok(&result.agent),
                Err(error) => fail(&error),
            },
            Err(message) => usage(&message),
        },
        Some("update") => {
            let Some(id) = args.get(1).map(String::as_str).filter(|id| !id.is_empty()) else {
                return usage("smelt agent update <id> [--name ...] [--prompt ...]");
            };
            match parse_patch(&args[2..]) {
                Ok(patch) => {
                    match control::<AgentDefinitionUpdateCall>(AgentDefinitionUpdateParams {
                        id: id.to_string(),
                        patch,
                    }) {
                        Ok(result) => print_ok(&result.agent),
                        Err(error) => fail(&error),
                    }
                }
                Err(message) => usage(&message),
            }
        }
        Some("delete") => {
            let Some(id) = args.get(1).map(String::as_str).filter(|id| !id.is_empty()) else {
                return usage("smelt agent delete <id>");
            };
            match control::<AgentDefinitionDelete>(AgentDefinitionIdParams { id: id.to_string() }) {
                Ok(_) => print_ok(&serde_json::json!({ "id": id })),
                Err(error) => fail(&error),
            }
        }
        Some("chat") => {
            let Some(id) = args.get(1).map(String::as_str).filter(|id| !id.is_empty()) else {
                return usage("smelt agent chat <id>");
            };
            match control::<smelt_model::control_api::ConversationCreateForAgent>(
                AgentDefinitionIdParams { id: id.to_string() },
            ) {
                Ok(created) => print_ok(&created),
                Err(error) => fail(&error),
            }
        }
        Some("conversations") => match smelt_client::session_ops::list_agent_conversations(
            &smelt_runtime_client::local::local_runtime(),
        ) {
            Ok(conversations) => print_ok(&serde_json::json!({ "conversations": conversations })),
            Err(error) => fail(&error),
        },
        Some("conversation-rename") => {
            let Some(id) = args.get(1).map(String::as_str).filter(|id| !id.is_empty()) else {
                return usage("smelt agent conversation-rename <session_id> --title <标题>");
            };
            let title = match conversation_title_flag(&args[2..]) {
                Ok(title) => title,
                Err(message) => return usage(&message),
            };
            match smelt_client::session_ops::rename_agent_conversation(
                &smelt_runtime_client::local::local_runtime(),
                id,
                &title,
            ) {
                Ok(renamed) => print_ok(&renamed),
                Err(error) => fail(&error),
            }
        }
        Some("plugins") => match plugin_catalog() {
            Ok(catalog) => print_ok(&catalog),
            Err(error) => fail(&error),
        },
        Some(command) => usage(&format!("未知子命令: {command}")),
        None => {
            print_help();
            2
        }
    }
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

fn run_automation(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("list") => match list_automations() {
            Ok(value) => print_ok(&value),
            Err(error) => fail(&error),
        },
        Some("get") => {
            let Some(id) = args.get(1).map(String::as_str).filter(|id| !id.is_empty()) else {
                return usage("smelt automation get <id>");
            };
            match get_automation(id) {
                Ok(value) => print_ok(&value),
                Err(error) => fail(&error),
            }
        }
        Some("create") => match parse_automation_create(&args[1..]) {
            Ok(automation) => match submit_automation(AutomationCommand::Upsert {
                automation: Box::new(automation),
            }) {
                Ok(value) => print_ok(&value),
                Err(error) => fail(&error),
            },
            Err(message) => usage(&message),
        },
        Some("enable" | "disable") => {
            let enabled = args[0] == "enable";
            let Some(id) = args.get(1).map(String::as_str).filter(|id| !id.is_empty()) else {
                return usage("smelt automation enable|disable <id>");
            };
            match submit_automation(AutomationCommand::SetEnabled {
                automation_id: id.to_string(),
                enabled,
            }) {
                Ok(value) => print_ok(&value),
                Err(error) => fail(&error),
            }
        }
        Some("run") => {
            let Some(id) = args.get(1).map(String::as_str).filter(|id| !id.is_empty()) else {
                return usage("smelt automation run <id>");
            };
            match submit_automation(AutomationCommand::RunOnce {
                automation_id: id.to_string(),
            }) {
                Ok(value) => print_ok(&value),
                Err(error) => fail(&error),
            }
        }
        Some("delete") => {
            let Some(id) = args.get(1).map(String::as_str).filter(|id| !id.is_empty()) else {
                return usage("smelt automation delete <id>");
            };
            match submit_automation(AutomationCommand::Delete {
                automation_id: id.to_string(),
            }) {
                Ok(_) => print_ok(&json_id(id)),
                Err(error) => fail(&error),
            }
        }
        Some("update") => {
            let Some(id) = args.get(1).map(String::as_str).filter(|id| !id.is_empty()) else {
                return usage("smelt automation update <id> [--name ...] [--prompt ...]");
            };
            match parse_automation_update(id, &args[2..]) {
                Ok(automation) => match submit_automation(AutomationCommand::Upsert {
                    automation: Box::new(automation),
                }) {
                    Ok(value) => print_ok(&value),
                    Err(error) => fail(&error),
                },
                Err(message) => usage(&message),
            }
        }
        Some(command) => usage(&format!("未知子命令: {command}")),
        None => usage("smelt automation list|get|create|update|enable|disable|run|delete"),
    }
}

fn run_settings(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("get") => match control::<SettingsGet>(EmptyParams::default()) {
            Ok(result) => print_ok(&result),
            Err(error) => fail(&error),
        },
        Some("set") => match parse_settings_patch(&args[1..]) {
            Ok(patch) => match control::<SettingsUpdate>(patch) {
                Ok(result) => print_ok(&result),
                Err(error) => fail(&error),
            },
            Err(message) => usage(&message),
        },
        _ => usage("smelt settings get | smelt settings set key=value ..."),
    }
}

fn list_automations() -> Result<serde_json::Value, String> {
    let result = control::<AutomationList>(EmptyParams::default())?;
    Ok(serde_json::json!({ "automations": result.automations }))
}

fn get_automation(id: &str) -> Result<serde_json::Value, String> {
    let result = control::<AutomationGet>(AgentDefinitionIdParams { id: id.to_string() })?;
    serde_json::to_value(result.automation).map_err(|error| error.to_string())
}

fn submit_automation(command: AutomationCommand) -> Result<serde_json::Value, String> {
    let (result, snapshot) = smelt_client::session_ops::submit_automation_command(
        &smelt_runtime_client::local::local_runtime(),
        &command,
    )?;
    let snapshot = redact_automation_credentials(snapshot);
    serde_json::to_value(serde_json::json!({
        "result": result,
        "automations": snapshot.automations,
    }))
    .map_err(|error| error.to_string())
}

fn parse_automation_create(args: &[String]) -> Result<Automation, String> {
    let flags = parse_automation_flags(args)?;
    let name = flags
        .name
        .filter(|name| !name.trim().is_empty())
        .ok_or("需要 --name")?;
    let action = automation_action(flags.agent, flags.shell, flags.prompt)?;
    let trigger = automation_trigger(
        flags.every_hours,
        flags.every_minutes,
        flags.daily.as_deref(),
        flags.webhook,
        flags.event.as_deref(),
    )?;
    Ok(Automation {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        enabled: true,
        workspace_dir: None,
        trigger,
        action,
        sinks: Vec::new(),
    })
}

fn parse_automation_update(id: &str, args: &[String]) -> Result<Automation, String> {
    let flags = parse_automation_flags(args)?;
    let mut automation = load_automation_raw(id)?;
    let mut changed = false;
    if let Some(name) = flags.name.filter(|name| !name.trim().is_empty()) {
        automation.name = name;
        changed = true;
    }
    if flags.agent.is_some() || flags.shell.is_some() {
        let current_agent = automation.agent_definition_id().map(str::to_string);
        let current_prompt = automation.prompt().map(str::to_string);
        automation.action = automation_action(
            flags.agent.or(current_agent),
            flags.shell,
            flags.prompt.or(current_prompt),
        )?;
        changed = true;
    } else if let Some(prompt) = flags.prompt {
        match &mut automation.action {
            AutomationAction::Agent { prompt: slot, .. } => *slot = Some(prompt),
            AutomationAction::Shell { .. } => {
                return Err("这条自动化是 Shell 动作，不能改 --prompt".into());
            }
        }
        changed = true;
    }
    if flags.every_hours.is_some()
        || flags.every_minutes.is_some()
        || flags.daily.is_some()
        || flags.webhook
        || flags.event.is_some()
    {
        automation.trigger = automation_trigger(
            flags.every_hours,
            flags.every_minutes,
            flags.daily.as_deref(),
            flags.webhook,
            flags.event.as_deref(),
        )?;
        changed = true;
    }
    if !changed {
        return Err("smelt automation update <id> 需要至少一个字段".into());
    }
    Ok(automation)
}

#[derive(Default)]
struct AutomationFlags {
    name: Option<String>,
    agent: Option<String>,
    prompt: Option<String>,
    shell: Option<String>,
    every_hours: Option<u32>,
    every_minutes: Option<u32>,
    daily: Option<String>,
    webhook: bool,
    event: Option<String>,
}

fn parse_automation_flags(args: &[String]) -> Result<AutomationFlags, String> {
    let mut flags = AutomationFlags::default();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let take = |index: usize, flag: &str| -> Result<String, String> {
            args.get(index + 1)
                .cloned()
                .ok_or_else(|| format!("{flag} 需要一个值"))
        };
        match flag {
            "--webhook" => {
                flags.webhook = true;
                index += 1;
                continue;
            }
            "--name" => flags.name = Some(take(index, flag)?),
            "--agent" => flags.agent = Some(take(index, flag)?),
            "--prompt" => flags.prompt = Some(take(index, flag)?),
            "--shell" => flags.shell = Some(take(index, flag)?),
            "--event" => flags.event = Some(take(index, flag)?),
            "--every-hours" => {
                flags.every_hours = Some(
                    take(index, flag)?
                        .parse::<u32>()
                        .map_err(|_| "--every-hours 必须是正整数".to_string())?,
                )
            }
            "--every-minutes" => {
                flags.every_minutes = Some(
                    take(index, flag)?
                        .parse::<u32>()
                        .map_err(|_| "--every-minutes 必须是正整数".to_string())?,
                )
            }
            "--daily" => flags.daily = Some(take(index, flag)?),
            other => return Err(format!("未知参数: {other}")),
        }
        index += 2;
    }
    Ok(flags)
}

fn automation_action(
    agent: Option<String>,
    shell: Option<String>,
    prompt: Option<String>,
) -> Result<AutomationAction, String> {
    match (agent, shell) {
        (Some(agent_id), None) => Ok(AutomationAction::agent(agent_id, prompt)),
        (None, Some(command)) => Ok(AutomationAction::shell(command, Vec::new())),
        _ => Err("需要 --agent <id> 或 --shell <命令>，不能同时有".into()),
    }
}

fn automation_trigger(
    every_hours: Option<u32>,
    every_minutes: Option<u32>,
    daily: Option<&str>,
    webhook: bool,
    event: Option<&str>,
) -> Result<AutomationTrigger, String> {
    let kinds = usize::from(every_hours.is_some())
        + usize::from(every_minutes.is_some())
        + usize::from(daily.is_some())
        + usize::from(webhook)
        + usize::from(event.is_some());
    if kinds != 1 {
        return Err(
            "触发条件只能选一种：--every-hours、--every-minutes、--daily、--webhook 或 --event"
                .into(),
        );
    }
    if let Some(hours) = every_hours {
        return Ok(AutomationTrigger::schedule(
            AutomationSchedule::EveryHours { hours },
        ));
    }
    if let Some(minutes) = every_minutes {
        return Ok(AutomationTrigger::schedule(
            AutomationSchedule::EveryMinutes { minutes },
        ));
    }
    if let Some(clock) = daily {
        let (hour, minute) = parse_clock(clock)?;
        return Ok(AutomationTrigger::schedule(AutomationSchedule::Daily {
            hour,
            minute,
        }));
    }
    if webhook {
        return Ok(AutomationTrigger::webhook_with_secret(
            uuid::Uuid::new_v4().to_string(),
        ));
    }
    if let Some(topic) = event {
        let topic = topic.trim();
        if topic.is_empty() {
            return Err("--event 需要主题".into());
        }
        return Ok(AutomationTrigger::event(topic, None));
    }
    Err("缺少触发条件".into())
}

/// 运行时给的是去掉 webhook 令牌的投影；提交回去时运行时按位置换回真令牌。
fn load_automation_raw(id: &str) -> Result<Automation, String> {
    control::<AutomationGet>(AgentDefinitionIdParams { id: id.to_string() })
        .map(|result| result.automation)
}

/// 插件在运行时那台机器上，由运行时扫描。
fn plugin_catalog() -> Result<serde_json::Value, String> {
    let config = control::<smelt_model::control_api::PiConfigGet>(EmptyParams::default())?;
    let plugins: Vec<serde_json::Value> = config
        .plugins
        .into_iter()
        .map(|plugin| {
            serde_json::json!({
                "id": plugin.id,
                "kind": plugin.kind.as_str(),
                "name": plugin.name,
                "description": plugin.description,
                "origin": plugin.origin,
                "broken": plugin.broken,
            })
        })
        .collect();
    Ok(serde_json::json!({ "plugins": plugins }))
}

fn parse_clock(value: &str) -> Result<(u8, u8), String> {
    let (hour, minute) = value.split_once(':').ok_or(" --daily 格式为 HH:MM")?;
    let hour = hour.parse::<u8>().map_err(|_| "小时无效".to_string())?;
    let minute = minute.parse::<u8>().map_err(|_| "分钟无效".to_string())?;
    if hour > 23 || minute > 59 {
        return Err("时间必须在 00:00 到 23:59".into());
    }
    Ok((hour, minute))
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

fn print_ok(value: &impl serde::Serialize) -> i32 {
    match print_json(value) {
        Ok(()) => 0,
        Err(error) => fail(&error),
    }
}

fn json_id(id: &str) -> serde_json::Value {
    serde_json::json!({ "id": id })
}

fn parse_create(args: &[String]) -> Result<AgentDefinitionCreate, String> {
    let flags = parse_flags(args)?;
    let name = flags
        .name
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "smelt agent create --name <名称>".to_string())?;
    Ok(AgentDefinitionCreate {
        id: flags.id,
        name,
        prompt: flags.prompt.unwrap_or_default(),
        engine_kind_id: flags.engine_kind_id,
        plugins: flags.plugins,
        context_folders: flags.folders,
        context_links: flags.links,
        memory_enabled: flags.memory_enabled,
        ..Default::default()
    })
}

fn parse_patch(args: &[String]) -> Result<AgentDefinitionPatch, String> {
    let flags = parse_flags(args)?;
    if flags.id.is_some() {
        return Err("update 不能改 id".into());
    }
    let patch = AgentDefinitionPatch {
        name: flags.name,
        prompt: flags.prompt,
        engine_kind_id: flags.engine_kind_id,
        plugins: flags.plugins_set.then_some(flags.plugins),
        context_folders: flags.folders_set.then_some(flags.folders),
        context_links: flags.links_set.then_some(flags.links),
        memory_enabled: flags.memory_enabled,
        ..Default::default()
    };
    if patch == AgentDefinitionPatch::default() {
        return Err("smelt agent update <id> 需要至少一个字段".into());
    }
    Ok(patch)
}

#[derive(Default)]
struct Flags {
    id: Option<String>,
    name: Option<String>,
    prompt: Option<String>,
    engine_kind_id: Option<String>,
    plugins: Vec<String>,
    folders: Vec<String>,
    links: Vec<String>,
    plugins_set: bool,
    folders_set: bool,
    links_set: bool,
    memory_enabled: Option<bool>,
}

fn conversation_title_flag(args: &[String]) -> Result<String, String> {
    let mut title = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--title" => {
                title = Some(
                    args.get(index + 1)
                        .cloned()
                        .ok_or_else(|| "--title 需要一个值".to_string())?,
                );
                index += 2;
            }
            other => return Err(format!("未知参数: {other}")),
        }
    }
    title
        .filter(|title| !title.trim().is_empty())
        .ok_or_else(|| "smelt agent conversation-rename <session_id> --title <标题>".to_string())
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let value = |index: usize, flag: &str| -> Result<String, String> {
            args.get(index + 1)
                .cloned()
                .ok_or_else(|| format!("{flag} 需要一个值"))
        };
        match arg {
            "--id" => flags.id = Some(value(index, arg)?),
            "--name" => flags.name = Some(value(index, arg)?),
            "--prompt" => flags.prompt = Some(value(index, arg)?),
            "--engine" => flags.engine_kind_id = Some(value(index, arg)?),
            "--memory" => {
                flags.memory_enabled = Some(match value(index, arg)?.as_str() {
                    "on" => true,
                    "off" => false,
                    other => return Err(format!("--memory 只能是 on 或 off，收到 {other}")),
                });
            }
            "--plugin" => {
                flags.plugins_set = true;
                flags.plugins.push(value(index, arg)?);
            }
            "--folder" => {
                flags.folders_set = true;
                flags.folders.push(value(index, arg)?);
            }
            "--link" => {
                flags.links_set = true;
                flags.links.push(value(index, arg)?);
            }
            other if other.starts_with('-') => {
                return Err(format!("未知参数: {other}"));
            }
            other => return Err(format!("多余参数: {other}")),
        }
        index += 2;
    }
    Ok(flags)
}

fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    println!("{json}");
    Ok(())
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
mod tests {
    use super::*;

    #[test]
    fn memory_flag_turns_agent_memory_on_or_off() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_patch(&args(&["--memory", "off"]))
                .unwrap()
                .memory_enabled,
            Some(false)
        );
        assert_eq!(
            parse_create(&args(&["--name", "写作", "--memory", "on"]))
                .unwrap()
                .memory_enabled,
            Some(true)
        );
        assert_eq!(
            parse_create(&args(&["--name", "写作"]))
                .unwrap()
                .memory_enabled,
            None,
            "不写就用默认（开）"
        );
        assert!(parse_patch(&args(&["--memory", "maybe"])).is_err());
    }

    #[test]
    fn help_and_version_are_cli_commands() {
        assert_eq!(maybe_run(["smelt", "--help"]), Some(0));
        assert_eq!(maybe_run(["smelt", "--version"]), Some(0));
        assert_eq!(maybe_run(["smelt"]), None);
        assert_eq!(maybe_run(["smelt", "--install-app"]), None);
    }

    #[test]
    fn create_requires_a_name() {
        assert!(parse_create(&[]).is_err());
        let created = parse_create(&[
            "--name".into(),
            "写作".into(),
            "--plugin".into(),
            "skill:a".into(),
        ])
        .unwrap();
        assert_eq!(created.name, "写作");
        assert_eq!(created.plugins, ["skill:a"]);
    }

    #[test]
    fn update_treats_repeated_plugin_flags_as_a_replace() {
        let patch = parse_patch(&[
            "--plugin".into(),
            "skill:a".into(),
            "--plugin".into(),
            "skill:b".into(),
        ])
        .unwrap();
        assert_eq!(
            patch.plugins.as_deref(),
            Some([String::from("skill:a"), String::from("skill:b")].as_slice())
        );
    }

    #[test]
    fn automation_create_builds_a_hourly_agent_job() {
        let automation = parse_automation_create(&[
            "--name".into(),
            "盯盘".into(),
            "--agent".into(),
            "writer".into(),
            "--every-hours".into(),
            "1".into(),
            "--prompt".into(),
            "看一下持仓".into(),
        ])
        .unwrap();
        assert_eq!(automation.name, "盯盘");
        assert_eq!(automation.agent_definition_id(), Some("writer"));
        assert_eq!(automation.prompt(), Some("看一下持仓"));
        assert_eq!(
            automation.trigger.schedule_values(),
            &[AutomationSchedule::EveryHours { hours: 1 }]
        );
    }

    #[test]
    fn automation_create_accepts_webhook_without_a_schedule() {
        let automation = parse_automation_create(&[
            "--name".into(),
            "外部".into(),
            "--agent".into(),
            "writer".into(),
            "--webhook".into(),
        ])
        .unwrap();
        assert!(!automation.trigger.webhooks.is_empty());
        assert!(automation.trigger.schedule_values().is_empty());
    }

    #[test]
    fn settings_set_parses_typed_values() {
        let patch = parse_settings_patch(&[
            "appearance.theme_mode=light".into(),
            "appearance.ui_font_px=18".into(),
        ])
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
