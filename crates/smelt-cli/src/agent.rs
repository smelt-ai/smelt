//! `smelt agent …`：产品智能体定义。

use crate::{control, fail, flag_value, id_arg, json_id, on_off, print_ok, respond, usage};
use rust_i18n::t;
use smelt_model::agent_definition_store::{AgentDefinitionCreate, AgentDefinitionPatch};
use smelt_model::control_api::{
    AgentDefinitionCreateCall, AgentDefinitionDelete, AgentDefinitionGet, AgentDefinitionIdParams,
    AgentDefinitionList, AgentDefinitionUpdateCall, AgentDefinitionUpdateParams, AgentMemoryList,
    ConversationCreateForAgent, PiConfigGet,
};
use smelt_model::pi_plugin_catalog::PiPluginKind;
use smelt_model::pi_service::PiConfig;
use smelt_runtime_client::protocol::control::EmptyParams;

pub(crate) fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => respond(control::<AgentDefinitionList>(EmptyParams::default()).map(
            |result| {
                let agents: Vec<_> = result.agents.into_iter().map(to_json).collect();
                serde_json::json!({ "agents": agents })
            },
        )),
        Some("get") => match id_arg(rest) {
            Some(id) => respond(
                control::<AgentDefinitionGet>(AgentDefinitionIdParams { id: id.to_string() })
                    .map(|result| to_json(result.agent)),
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
        // 只读。采纳、删除要用户在桌面上点：运行时拒绝智能体会话里发来的这两种请求。
        Some("memory") => match id_arg(rest) {
            Some(id) => respond(control::<AgentMemoryList>(AgentDefinitionIdParams {
                id: id.to_string(),
            })),
            None => usage("smelt agent memory <id>"),
        },
        Some("chat") => {
            let Some(id) = id_arg(rest) else {
                return usage(&t!("agent.chat_usage"));
            };
            match message_flag(&rest[1..]) {
                Ok(message) => chat(id, message),
                Err(message) => usage(&message),
            }
        }
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
        None => usage("smelt agent list|get|create|update|delete|chat|memory"),
    }
}

fn create(flags: Flags) -> Result<serde_json::Value, String> {
    let input = create_input(flags)?;
    check_bindings(&input.plugins, &input.mcp_servers, model_of(&input))?;
    control::<AgentDefinitionCreateCall>(input).map(|result| to_json(result.agent))
}

fn update(id: &str, flags: Flags) -> Result<serde_json::Value, String> {
    // 只对这次写了的查有没有；照抄过来的另一类是原先就绑着的，不因为目录里没了就改不动。
    let given: Vec<String> = [
        (PiPluginKind::Skill, &flags.skills),
        (PiPluginKind::Extension, &flags.extensions),
    ]
    .into_iter()
    .flat_map(|(kind, names)| catalog_ids(kind, names.as_deref().unwrap_or_default()))
    .collect();
    // 只写了一类时，另一类照旧，得先知道现在绑着哪些。
    let patch = patch_input(flags, || {
        control::<AgentDefinitionGet>(AgentDefinitionIdParams { id: id.to_string() })
            .map(|result| result.agent.plugins)
    })?;
    let model = match (&patch.model_provider, &patch.model_id) {
        (Some(provider), Some(model)) if !provider.is_empty() => Some((provider, model)),
        _ => None,
    };
    check_bindings(
        &given,
        patch.mcp_servers.as_deref().unwrap_or_default(),
        model,
    )?;
    control::<AgentDefinitionUpdateCall>(AgentDefinitionUpdateParams {
        id: id.to_string(),
        patch,
    })
    .map(|result| to_json(result.agent))
}

/// 智能体定义转 JSON。绑着的 Pi 技能和扩展在定义里是一份 `plugins`（`<类别>:<名字>`），
/// 输出拆成 `skills` 和 `extensions` 两份名字，和 `--skill` / `--extension` 对得上。
fn to_json(value: impl serde::Serialize) -> serde_json::Value {
    let mut value = serde_json::to_value(value).unwrap_or_default();
    if let Some(object) = value.as_object_mut()
        && let Some(serde_json::Value::Array(ids)) = object.remove("plugins")
    {
        let ids: Vec<String> = ids
            .into_iter()
            .filter_map(|id| id.as_str().map(str::to_string))
            .collect();
        for kind in [PiPluginKind::Skill, PiPluginKind::Extension] {
            let prefix = format!("{}:", kind.as_str());
            let names: Vec<&str> = ids
                .iter()
                .filter_map(|id| id.strip_prefix(&prefix))
                .collect();
            object.insert(kind_key(kind).to_string(), names.into());
        }
    }
    value
}

fn kind_key(kind: PiPluginKind) -> &'static str {
    match kind {
        PiPluginKind::Skill => "skills",
        PiPluginKind::Extension => "extensions",
    }
}

fn kind_of(id: &str) -> Option<PiPluginKind> {
    [PiPluginKind::Skill, PiPluginKind::Extension]
        .into_iter()
        .find(|kind| id.starts_with(&format!("{}:", kind.as_str())))
}

/// `--skill` / `--extension` 写的名字转成目录 id；写 `""` 是清空那一类，不留空项。
fn catalog_ids(kind: PiPluginKind, names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .map(|name| crate::pi::catalog_id(kind, name))
        .collect()
}

/// 改后绑着的技能和扩展。两类各自整份替换：写了哪类换哪类，没写的那类从 `current` 照抄；
/// 都没写是 `None`（不动），这时不去读 `current`。
fn merged_bindings(
    skills: Option<&[String]>,
    extensions: Option<&[String]>,
    current: impl FnOnce() -> Result<Vec<String>, String>,
) -> Result<Option<Vec<String>>, String> {
    let given = [
        (PiPluginKind::Skill, skills),
        (PiPluginKind::Extension, extensions),
    ];
    if given.iter().all(|(_, names)| names.is_none()) {
        return Ok(None);
    }
    let current = if given.iter().all(|(_, names)| names.is_some()) {
        Vec::new()
    } else {
        current()?
    };
    let mut ids = Vec::new();
    for (kind, names) in given {
        match names {
            Some(names) => ids.extend(catalog_ids(kind, names)),
            None => ids.extend(
                current
                    .iter()
                    .filter(|id| kind_of(id) == Some(kind))
                    .cloned(),
            ),
        }
    }
    Ok(Some(ids))
}

fn model_of(input: &AgentDefinitionCreate) -> Option<(&String, &String)> {
    input.model_provider.as_ref().zip(input.model_id.as_ref())
}

/// 绑定的技能和扩展、MCP 服务器、模型必须是运行时那台机器上有的。写错的 id 运行时照存，
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
            return Err(match plugin.split_once(':') {
                Some(("extension", name)) => t!("agent.no_such_extension", name = name),
                Some((_, name)) => t!("agent.no_such_skill", name = name),
                None => t!("agent.no_such_skill", name = plugin),
            }
            .into_owned());
        }
    }
    if mcp_servers.iter().any(|name| !name.trim().is_empty()) {
        let servers = config
            .mcp_servers
            .as_ref()
            .map_err(|error| t!("mcp_config_unreadable", error = error).into_owned())?;
        for name in mcp_servers
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
        {
            if !servers.iter().any(|server| server.name == name) {
                return Err(t!("agent.no_such_mcp_server", name = name).into_owned());
            }
        }
    }
    if let Some((provider, model)) = model
        && !crate::pi::has_model(config, provider, model)
    {
        return Err(t!("no_such_model", provider = provider, model = model).into_owned());
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
            return fail(&t!(
                "agent.first_message_failed",
                session_id = created.session_id,
                error = error
            ));
        }
    }
    print_ok(&created)
}

fn message_flag(args: &[String]) -> Result<Option<String>, String> {
    match args {
        [] => Ok(None),
        [flag, text] if flag == "--message" && !text.trim().is_empty() => Ok(Some(text.clone())),
        [flag, ..] if flag == "--message" => Err(t!("agent.message_missing").into_owned()),
        [other, ..] => Err(t!("unknown_argument", arg = other).into_owned()),
    }
}

fn create_input(flags: Flags) -> Result<AgentDefinitionCreate, String> {
    let name = flags
        .name
        .filter(|name| !name.is_empty())
        .ok_or_else(|| t!("agent.create_usage").into_owned())?;
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
        plugins: merged_bindings(flags.skills.as_deref(), flags.extensions.as_deref(), || {
            Ok(Vec::new())
        })?
        .unwrap_or_default(),
        mcp_servers: flags.mcp_servers.unwrap_or_default(),
        context_folders: flags.folders.unwrap_or_default(),
        context_links: flags.links.unwrap_or_default(),
        model_provider,
        model_id,
        memory_enabled: flags.memory_enabled,
    })
}

/// `current` 读这个智能体现在绑着的技能和扩展，只写了其中一类时才调。
fn patch_input(
    flags: Flags,
    current: impl FnOnce() -> Result<Vec<String>, String>,
) -> Result<AgentDefinitionPatch, String> {
    if flags.id.is_some() {
        return Err(t!("agent.update_id").into_owned());
    }
    let (model_provider, model_id) = flags.model.unzip();
    let patch = AgentDefinitionPatch {
        name: flags.name,
        prompt: flags.prompt,
        engine_kind_id: flags.engine_kind_id,
        plugins: merged_bindings(
            flags.skills.as_deref(),
            flags.extensions.as_deref(),
            current,
        )?,
        mcp_servers: flags.mcp_servers,
        context_folders: flags.folders,
        context_links: flags.links,
        model_provider,
        model_id,
        memory_enabled: flags.memory_enabled,
    };
    if patch == AgentDefinitionPatch::default() {
        return Err(t!("agent.update_empty").into_owned());
    }
    Ok(patch)
}

#[derive(Default)]
struct Flags {
    id: Option<String>,
    name: Option<String>,
    prompt: Option<String>,
    engine_kind_id: Option<String>,
    /// 列表类选项写了就是整份替换（`None` 是没写）。技能和扩展各算一份。
    skills: Option<Vec<String>>,
    extensions: Option<Vec<String>>,
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
            return Err(t!("extra_argument", arg = arg).into_owned());
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
            "--skill" => push(&mut flags.skills, value),
            "--extension" => push(&mut flags.extensions, value),
            "--mcp" => push(&mut flags.mcp_servers, value),
            "--folder" => push(&mut flags.folders, value),
            "--link" => push(&mut flags.links, value),
            "--model" => flags.model = Some(parse_model(&value)?),
            other => return Err(t!("unknown_argument", arg = other).into_owned()),
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
        _ => Err(t!("agent.model_format").into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn memory_flag_turns_agent_memory_on_or_off() {
        let patch = |list: &[&str]| patch_input(parse_flags(&args(list))?, no_current);
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

    /// 两类都写了或都没写时用不到现在绑着的。
    fn no_current() -> Result<Vec<String>, String> {
        panic!("不该去读现在绑着的技能和扩展")
    }

    #[test]
    fn writing_one_kind_keeps_the_other() {
        let current = || Ok(vec!["skill:old".to_string(), "extension:keep".to_string()]);
        let flags = parse_flags(&args(&["--skill", "new"])).unwrap();
        assert_eq!(
            patch_input(flags, current).unwrap().plugins.unwrap(),
            ["skill:new", "extension:keep"]
        );
        let cleared = parse_flags(&args(&["--extension", ""])).unwrap();
        assert_eq!(
            patch_input(cleared, current).unwrap().plugins.unwrap(),
            ["skill:old"]
        );
        let untouched = parse_flags(&args(&["--prompt", "x"])).unwrap();
        assert_eq!(patch_input(untouched, no_current).unwrap().plugins, None);
    }

    #[test]
    fn output_splits_bindings_into_skills_and_extensions() {
        let json = to_json(serde_json::json!({
            "id": "a",
            "plugins": ["skill:writer", "extension:git", "skill:review"],
        }));
        assert_eq!(json["skills"], serde_json::json!(["writer", "review"]));
        assert_eq!(json["extensions"], serde_json::json!(["git"]));
        assert!(json.get("plugins").is_none());
    }

    #[test]
    fn create_requires_a_name() {
        assert!(create_input(parse_flags(&[]).unwrap()).is_err());
        let created =
            create_input(parse_flags(&args(&["--name", "写作", "--skill", "a"])).unwrap()).unwrap();
        assert_eq!(created.name, "写作");
        assert_eq!(created.plugins, ["skill:a"]);
    }

    #[test]
    fn update_treats_repeated_list_flags_as_a_replace() {
        let patch = patch_input(
            parse_flags(&args(&[
                "--skill",
                "a",
                "--skill",
                "skill:b",
                "--extension",
                "e",
                "--mcp",
                "figma",
            ]))
            .unwrap(),
            no_current,
        )
        .unwrap();
        assert_eq!(
            patch.plugins.as_deref(),
            Some(
                [
                    String::from("skill:a"),
                    String::from("skill:b"),
                    String::from("extension:e")
                ]
                .as_slice()
            )
        );
        assert_eq!(
            patch.mcp_servers.as_deref(),
            Some([String::from("figma")].as_slice())
        );
        assert_eq!(patch.context_folders, None, "没写的列表不动");

        let cleared = patch_input(parse_flags(&args(&["--mcp", ""])).unwrap(), no_current).unwrap();
        assert_eq!(
            cleared.mcp_servers.as_deref(),
            Some([String::new()].as_slice()),
            "写空就是清空，运行时会去掉空项"
        );
    }

    #[test]
    fn model_flag_picks_a_model_or_follows_the_default() {
        let patch = patch_input(
            parse_flags(&args(&["--model", "openrouter/anthropic/claude"])).unwrap(),
            no_current,
        )
        .unwrap();
        assert_eq!(patch.model_provider.as_deref(), Some("openrouter"));
        assert_eq!(patch.model_id.as_deref(), Some("anthropic/claude"));

        let reset = patch_input(
            parse_flags(&args(&["--model", "default"])).unwrap(),
            no_current,
        )
        .unwrap();
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
