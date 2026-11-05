//! `smelt agent …`：产品智能体定义。

use crate::{control, fail, flag_value, id_arg, json_id, on_off, print_ok, respond, usage};
use smelt_model::agent_definition_store::{AgentDefinitionCreate, AgentDefinitionPatch};
use smelt_model::control_api::{
    AgentDefinitionCreateCall, AgentDefinitionDelete, AgentDefinitionGet, AgentDefinitionIdParams,
    AgentDefinitionList, AgentDefinitionUpdateCall, AgentDefinitionUpdateParams,
    ConversationCreateForAgent, PiConfigGet,
};
use smelt_model::pi_service::PiConfig;
use smelt_runtime_client::protocol::control::EmptyParams;

pub(crate) fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => respond(
            control::<AgentDefinitionList>(EmptyParams::default())
                .map(|result| serde_json::json!({ "agents": result.agents })),
        ),
        Some("get") => match id_arg(rest) {
            Some(id) => respond(
                control::<AgentDefinitionGet>(AgentDefinitionIdParams { id: id.to_string() })
                    .map(|result| result.agent),
            ),
            None => usage("smelt agent get <id>"),
        },
        Some("create") => match parse_flags(rest) {
            Ok(flags) => respond(create(flags)),
            Err(message) => usage(&message),
        },
        Some("update") => {
            let Some(id) = id_arg(rest) else {
                return usage("smelt agent update <id> [--name ...] [--prompt ...]");
            };
            match parse_flags(&rest[1..]) {
                Ok(flags) => respond(update(id, flags)),
                Err(message) => usage(&message),
            }
        }
        Some("delete") => match id_arg(rest) {
            Some(id) => respond(
                control::<AgentDefinitionDelete>(AgentDefinitionIdParams { id: id.to_string() })
                    .map(|_| json_id(id)),
            ),
            None => usage("smelt agent delete <id>"),
        },
        Some("chat") => {
            let Some(id) = id_arg(rest) else {
                return usage("smelt agent chat <id> [--message <第一句话>]");
            };
            match message_flag(&rest[1..]) {
                Ok(message) => chat(id, message),
                Err(message) => usage(&message),
            }
        }
        Some(command) => usage(&format!("未知子命令: {command}")),
        None => usage("smelt agent list|get|create|update|delete|chat"),
    }
}

fn create(flags: Flags) -> Result<serde_json::Value, String> {
    let input = create_input(flags)?;
    check_bindings(&input.plugins, &input.mcp_servers, model_of(&input))?;
    control::<AgentDefinitionCreateCall>(input).map(|result| to_json(result.agent))
}

fn update(id: &str, flags: Flags) -> Result<serde_json::Value, String> {
    let patch = patch_input(flags)?;
    let model = match (&patch.model_provider, &patch.model_id) {
        (Some(provider), Some(model)) if !provider.is_empty() => Some((provider, model)),
        _ => None,
    };
    check_bindings(
        patch.plugins.as_deref().unwrap_or_default(),
        patch.mcp_servers.as_deref().unwrap_or_default(),
        model,
    )?;
    control::<AgentDefinitionUpdateCall>(AgentDefinitionUpdateParams {
        id: id.to_string(),
        patch,
    })
    .map(|result| to_json(result.agent))
}

fn to_json(value: impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_default()
}

fn model_of(input: &AgentDefinitionCreate) -> Option<(&String, &String)> {
    input.model_provider.as_ref().zip(input.model_id.as_ref())
}

/// 绑定的插件、MCP 服务器、模型必须是运行时那台机器上有的。写错的 id 运行时照存，
/// 对话里就悄悄少了这项能力，所以在这里先对一遍。都没写时不去问运行时。
fn check_bindings(
    plugins: &[String],
    mcp_servers: &[String],
    model: Option<(&String, &String)>,
) -> Result<(), String> {
    let wanted = |list: &[String]| list.iter().any(|item| !item.trim().is_empty());
    if !wanted(plugins) && !wanted(mcp_servers) && model.is_none() {
        return Ok(());
    }
    let config = control::<PiConfigGet>(EmptyParams::default())?;
    check_bindings_against(&config, plugins, mcp_servers, model)
}

fn check_bindings_against(
    config: &PiConfig,
    plugins: &[String],
    mcp_servers: &[String],
    model: Option<(&String, &String)>,
) -> Result<(), String> {
    for plugin in plugins
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
    {
        if !config.plugins.iter().any(|known| known.id == plugin) {
            return Err(format!("没有这个插件：{plugin}（smelt plugin list 查看）"));
        }
    }
    if mcp_servers.iter().any(|name| !name.trim().is_empty()) {
        let servers = config
            .mcp_servers
            .as_ref()
            .map_err(|error| format!("读不到 MCP 配置：{error}"))?;
        for name in mcp_servers
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
        {
            if !servers.iter().any(|server| server.name == name) {
                return Err(format!(
                    "没有这个 MCP 服务器：{name}（smelt mcp list 查看）"
                ));
            }
        }
    }
    if let Some((provider, model)) = model
        && !crate::pi::has_model(config, provider, model)
    {
        return Err(format!(
            "没有这个模型：{provider}/{model}（smelt model list 查看）"
        ));
    }
    Ok(())
}

fn chat(id: &str, message: Option<String>) -> i32 {
    let created =
        match control::<ConversationCreateForAgent>(AgentDefinitionIdParams { id: id.to_string() })
        {
            Ok(created) => created,
            Err(error) => return fail(&error),
        };
    if let Some(text) = message {
        let input = smelt_model::conversation::ConversationInput::new(text, Vec::new());
        if let Err(error) = smelt_client::session_ops::submit_conversation_input(
            &smelt_runtime_client::local::local_runtime(),
            &created.session_id,
            &input,
            &[],
        ) {
            return fail(&format!(
                "对话已开（session_id {}），但第一句话没发出去：{error}",
                created.session_id
            ));
        }
    }
    print_ok(&created)
}

fn message_flag(args: &[String]) -> Result<Option<String>, String> {
    match args {
        [] => Ok(None),
        [flag, text] if flag == "--message" && !text.trim().is_empty() => Ok(Some(text.clone())),
        [flag, ..] if flag == "--message" => Err("--message 需要一句话".into()),
        [other, ..] => Err(format!("未知参数: {other}")),
    }
}

fn create_input(flags: Flags) -> Result<AgentDefinitionCreate, String> {
    let name = flags
        .name
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "smelt agent create --name <名称>".to_string())?;
    // `--model default` 就是不写。
    let (model_provider, model_id) = match flags.model {
        Some((provider, model)) if !provider.is_empty() => (Some(provider), Some(model)),
        _ => (None, None),
    };
    Ok(AgentDefinitionCreate {
        id: flags.id,
        name,
        prompt: flags.prompt.unwrap_or_default(),
        engine_kind_id: flags.engine_kind_id,
        plugins: flags.plugins.unwrap_or_default(),
        mcp_servers: flags.mcp_servers.unwrap_or_default(),
        context_folders: flags.folders.unwrap_or_default(),
        context_links: flags.links.unwrap_or_default(),
        model_provider,
        model_id,
        memory_enabled: flags.memory_enabled,
    })
}

fn patch_input(flags: Flags) -> Result<AgentDefinitionPatch, String> {
    if flags.id.is_some() {
        return Err("update 不能改 id".into());
    }
    let (model_provider, model_id) = flags.model.unzip();
    let patch = AgentDefinitionPatch {
        name: flags.name,
        prompt: flags.prompt,
        engine_kind_id: flags.engine_kind_id,
        plugins: flags.plugins,
        mcp_servers: flags.mcp_servers,
        context_folders: flags.folders,
        context_links: flags.links,
        model_provider,
        model_id,
        memory_enabled: flags.memory_enabled,
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
    /// 列表类选项写了就是整份替换（`None` 是没写）。
    plugins: Option<Vec<String>>,
    mcp_servers: Option<Vec<String>>,
    folders: Option<Vec<String>>,
    links: Option<Vec<String>>,
    /// 空的 provider / 模型 id 表示跟随 Pi 的默认模型。
    model: Option<(String, String)>,
    memory_enabled: Option<bool>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if !arg.starts_with('-') {
            return Err(format!("多余参数: {arg}"));
        }
        let value = flag_value(args, index)?;
        let push = |list: &mut Option<Vec<String>>, value: String| {
            list.get_or_insert_with(Vec::new).push(value)
        };
        match arg {
            "--id" => flags.id = Some(value),
            "--name" => flags.name = Some(value),
            "--prompt" => flags.prompt = Some(value),
            "--engine" => flags.engine_kind_id = Some(value),
            "--memory" => flags.memory_enabled = Some(on_off(arg, &value)?),
            "--plugin" => push(&mut flags.plugins, value),
            "--mcp" => push(&mut flags.mcp_servers, value),
            "--folder" => push(&mut flags.folders, value),
            "--link" => push(&mut flags.links, value),
            "--model" => flags.model = Some(parse_model(&value)?),
            other => return Err(format!("未知参数: {other}")),
        }
        index += 2;
    }
    Ok(flags)
}

/// `provider/模型id`；模型 id 里可以再有 `/`（OpenRouter 之类），只按第一个切。
fn parse_model(value: &str) -> Result<(String, String), String> {
    let value = value.trim();
    if value == "default" {
        return Ok((String::new(), String::new()));
    }
    match value.split_once('/') {
        Some((provider, model)) if !provider.trim().is_empty() && !model.trim().is_empty() => {
            Ok((provider.trim().to_string(), model.trim().to_string()))
        }
        _ => Err("--model 格式为 <provider>/<模型id>，或 default 跟随默认模型".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn memory_flag_turns_agent_memory_on_or_off() {
        let patch = |list: &[&str]| patch_input(parse_flags(&args(list))?);
        let create = |list: &[&str]| create_input(parse_flags(&args(list))?);
        assert_eq!(
            patch(&["--memory", "off"]).unwrap().memory_enabled,
            Some(false)
        );
        assert_eq!(
            create(&["--name", "写作", "--memory", "on"])
                .unwrap()
                .memory_enabled,
            Some(true)
        );
        assert_eq!(
            create(&["--name", "写作"]).unwrap().memory_enabled,
            None,
            "不写就用默认（开）"
        );
        assert!(patch(&["--memory", "maybe"]).is_err());
    }

    #[test]
    fn create_requires_a_name() {
        assert!(create_input(parse_flags(&[]).unwrap()).is_err());
        let created =
            create_input(parse_flags(&args(&["--name", "写作", "--plugin", "skill:a"])).unwrap())
                .unwrap();
        assert_eq!(created.name, "写作");
        assert_eq!(created.plugins, ["skill:a"]);
    }

    #[test]
    fn update_treats_repeated_list_flags_as_a_replace() {
        let patch = patch_input(
            parse_flags(&args(&[
                "--plugin", "skill:a", "--plugin", "skill:b", "--mcp", "figma",
            ]))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            patch.plugins.as_deref(),
            Some([String::from("skill:a"), String::from("skill:b")].as_slice())
        );
        assert_eq!(
            patch.mcp_servers.as_deref(),
            Some([String::from("figma")].as_slice())
        );
        assert_eq!(patch.context_folders, None, "没写的列表不动");

        let cleared = patch_input(parse_flags(&args(&["--mcp", ""])).unwrap()).unwrap();
        assert_eq!(
            cleared.mcp_servers.as_deref(),
            Some([String::new()].as_slice()),
            "写空就是清空，运行时会去掉空项"
        );
    }

    #[test]
    fn model_flag_picks_a_model_or_follows_the_default() {
        let patch =
            patch_input(parse_flags(&args(&["--model", "openrouter/anthropic/claude"])).unwrap())
                .unwrap();
        assert_eq!(patch.model_provider.as_deref(), Some("openrouter"));
        assert_eq!(patch.model_id.as_deref(), Some("anthropic/claude"));

        let reset = patch_input(parse_flags(&args(&["--model", "default"])).unwrap()).unwrap();
        assert_eq!(reset.model_provider.as_deref(), Some(""));
        assert_eq!(reset.model_id.as_deref(), Some(""));

        let created =
            create_input(parse_flags(&args(&["--name", "a", "--model", "default"])).unwrap())
                .unwrap();
        assert_eq!(created.model_provider, None);

        assert!(parse_flags(&args(&["--model", "claude"])).is_err());
        assert!(parse_flags(&args(&["--model", "/claude"])).is_err());
    }

    #[test]
    fn bindings_must_exist_on_the_runtime() {
        let config = PiConfig {
            model_settings: Err(String::new()),
            settings_summary: Err(String::new()),
            mcp_servers: Ok(vec![smelt_model::pi_mcp::PiMcpServer {
                name: "figma".into(),
                transport: smelt_model::pi_mcp::PiMcpTransport::Unknown,
                enabled: true,
                description: None,
                signed_in: false,
            }]),
            plugins: Vec::new(),
            model_choice_groups: vec![smelt_model::pi_model_settings::PiModelChoiceGroup {
                provider: "anthropic".into(),
                provider_name: "Anthropic".into(),
                models: vec![smelt_model::pi_model_settings::PiModelChoice {
                    provider: "anthropic".into(),
                    provider_name: "Anthropic".into(),
                    id: "claude-opus-5-5".into(),
                    name: "Opus".into(),
                }],
            }],
            auto_provider_ids: Vec::new(),
            paths: Default::default(),
        };
        let provider = String::from("anthropic");
        let model = String::from("claude-opus-5-5");
        let missing = String::from("nope");
        assert!(check_bindings_against(&config, &[], &["figma".into()], None).is_ok());
        assert!(check_bindings_against(&config, &[], &["rea".into()], None).is_err());
        assert!(check_bindings_against(&config, &["skill:a".into()], &[], None).is_err());
        assert!(check_bindings_against(&config, &[], &[], Some((&provider, &model))).is_ok());
        assert!(check_bindings_against(&config, &[], &[], Some((&provider, &missing))).is_err());
        assert!(check_bindings_against(&config, &[], &["".into()], None).is_ok());
    }

    #[test]
    fn chat_takes_an_optional_first_message() {
        assert_eq!(message_flag(&[]).unwrap(), None);
        assert_eq!(
            message_flag(&args(&["--message", "开始吧"])).unwrap(),
            Some("开始吧".into())
        );
        assert!(message_flag(&args(&["--message", " "])).is_err());
        assert!(message_flag(&args(&["--title", "x"])).is_err());
    }
}
