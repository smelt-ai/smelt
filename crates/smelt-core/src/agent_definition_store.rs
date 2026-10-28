pub use smelt_model::agent_definition_store::*;

pub use smelt_model::agent_paths::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agent_definition::AgentDefinition;
use crate::agent_kind::{
    ConversationAgentKind, ConversationLaunchSpec, PI_MCP_COMPANION_EXTENSIONS,
    SMELT_AGENT_INSTRUCTIONS_ENV, SMELT_AGENT_MCP_SERVERS_ENV, SMELT_AGENT_MEMORY_DIR_ENV,
    SMELT_AGENT_MODEL_ID_ENV, SMELT_AGENT_MODEL_PROVIDER_ENV, SMELT_AGENT_PLUGIN_ARGS_ENV,
    SMELT_AGENT_UNATTENDED_ENV, SMELT_USER_MEMORY_DIR_ENV,
};
use crate::automation::AutomationAction;
use crate::sqlite_state;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentExecutionDefinition {
    pub definition: AgentDefinition,
    pub kind: ConversationAgentKind,
    pub launch: ConversationLaunchSpec,
}

#[derive(Default, Deserialize)]
struct AgentUiProjection {
    #[serde(default, alias = "agent_presets")]
    agents: Vec<AgentDefinition>,
    #[serde(default)]
    acp_pi_cmd: Option<String>,
    #[serde(default)]
    acp_env: BTreeMap<String, BTreeMap<String, String>>,
}

fn load_agent_ui_projection() -> Result<Option<AgentUiProjection>, String> {
    if !sqlite_state::is_sqlite_state_enabled() {
        return Ok(None);
    }
    let store = sqlite_state::default_sqlite_store()?;
    let Some(snapshot) = store.get_agent_ui_snapshot()? else {
        return Ok(None);
    };
    Ok(Some(projection_from_snapshot(snapshot)))
}

fn projection_from_snapshot(snapshot: smelt_store::AgentUiSnapshot) -> AgentUiProjection {
    let acp_pi_cmd = snapshot
        .commands
        .iter()
        .find(|command| command.command_key == ConversationAgentKind::Pi.descriptor().config_key)
        .map(|command| command.command.clone());
    let mut acp_env = BTreeMap::new();
    for record in snapshot.env {
        acp_env
            .entry(record.engine_kind_id)
            .or_insert_with(BTreeMap::new)
            .insert(record.name, record.value);
    }
    AgentUiProjection {
        agents: snapshot.agents.into_iter().map(agent_from_record).collect(),
        acp_pi_cmd,
        acp_env,
    }
}

fn agent_from_record(record: smelt_store::AgentDefinitionRecord) -> AgentDefinition {
    AgentDefinition {
        id: record.id,
        name: record.name,
        description: record.description,
        engine_kind_id: record.engine_kind_id,
        prompt: record.prompt,
        plugins: serde_json::from_slice(&record.plugins_json).unwrap_or_default(),
        mcp_servers: serde_json::from_slice(&record.mcp_servers_json).unwrap_or_default(),
        context_folders: serde_json::from_slice(&record.context_folders_json).unwrap_or_default(),
        context_links: serde_json::from_slice(&record.context_links_json).unwrap_or_default(),
        model_provider: record.model_provider,
        model_id: record.model_id,
        memory_enabled: record.memory_enabled,
    }
}

pub fn load_agent_execution_definition(
    agent_definition_id: &str,
) -> Result<AgentExecutionDefinition, String> {
    let Some(config) = load_agent_ui_projection()? else {
        return Err("智能体配置不存在".to_string());
    };
    resolve_agent_execution_definition(config, agent_definition_id)
}

/// 智能体定义清单，按存档顺序返回。
///
/// 只读投影用（新建对话选项、按 id 取定义，调用方各自决定过滤什么）。这里**不**过滤
/// `is_ready()`：定义不完整恰恰是用户要在列表里看见并回桌面补的东西，悄悄藏起来
/// 只会让人对着一份缺条目的清单猜。配置缺失返回空表而不是错误——没建过智能体是
/// 正常状态，不是故障。
pub fn load_agent_definitions() -> Vec<AgentDefinition> {
    load_agent_ui_projection()
        .ok()
        .flatten()
        .map(|config| config.agents)
        .unwrap_or_default()
}

/// 会话 → 智能体定义 id 的映射，来自桌面工作区存档。
///
/// 「谁在跑」和「对话挂在侧栏哪一组」不是一回事。侧栏分组跟 cwd 走：开在用户
/// 项目里的对话进项目列表。指挥台仍要在行尾标出智能体身份，所以这里只读桌面
/// 存下来的绑定，不能从 cwd 反推——项目路径本身说明不了是哪个智能体。
///
/// 键是 daemon 的会话 id（存档里的 `acp.sid`），跟 `RemoteSessionRecord.id`、
/// `WorkspaceMenuSession.id` 同一口径，调用方可以直接查。
pub fn load_session_agent_definition_ids() -> BTreeMap<String, String> {
    let Ok(store) = crate::sqlite_state::default_sqlite_store() else {
        return BTreeMap::new();
    };
    store.session_agent_definition_ids().unwrap_or_default()
}

/// 测试用：几段 ACP 对话（会话 id、cwd、智能体定义 id）逐条落进运行时库，再按库里的
/// 绑定读回来。
#[cfg(test)]
fn session_agent_definition_ids(
    conversations: &[(&str, &str, Option<&str>)],
) -> BTreeMap<String, String> {
    let dir = tempfile::tempdir().expect("临时目录");
    let store =
        smelt_store::Store::open_or_create(dir.path().join(smelt_store::DATABASE_FILE_NAME))
            .expect("测试库");
    for (index, (sid, cwd, agent_definition_id)) in conversations.iter().enumerate() {
        store
            .create_workspace_conversation(&smelt_store::WorkspaceSessionRecord {
                id: format!("conv-{index}"),
                custom_title: None,
                last_updated_at: 0,
                terminals: Vec::new(),
                acp: Some(smelt_store::WorkspaceAcpRecord {
                    cwd: Some(cwd.to_string()),
                    sid: Some(sid.to_string()),
                    agent_definition_id: agent_definition_id.map(str::to_string),
                    ..smelt_store::WorkspaceAcpRecord::default()
                }),
            })
            .expect("写入对话");
    }
    store.session_agent_definition_ids().expect("读回绑定")
}

fn resolve_agent_execution_definition(
    config: AgentUiProjection,
    agent_definition_id: &str,
) -> Result<AgentExecutionDefinition, String> {
    let definition = config
        .agents
        .into_iter()
        .find(|definition| definition.id == agent_definition_id)
        .ok_or_else(|| format!("智能体不存在: {agent_definition_id}"))?;
    if !definition.is_ready() {
        return Err("智能体定义不完整".to_string());
    }
    let kind = definition
        .engine_kind()
        .ok_or_else(|| format!("不支持的智能体执行引擎: {}", definition.engine_kind_id))?;
    if kind != ConversationAgentKind::Pi {
        return Err(format!(
            "自动化当前只支持 Pi 智能体，不能运行 {}",
            kind.label()
        ));
    }
    let mut launch = kind.default_launch();
    if let Some(command) = config.acp_pi_cmd {
        launch.command = command;
    }
    for (name, value) in config.acp_env.get(kind.id()).into_iter().flatten() {
        launch.env.insert(name.clone(), value.clone());
    }
    launch = prepare_agent_definition_launch(launch, Some(&definition));
    Ok(AgentExecutionDefinition {
        definition,
        kind,
        launch,
    })
}

/// `session.launch.prepare`：注入智能体的长期指令、插件与 MCP 勾选、模型、记忆目录。
/// 插件目录和记忆目录都在运行时那台机器上，所以由运行时来补。
pub fn prepare_launch(params: LaunchPrepareParams) -> LaunchPrepareResult {
    let LaunchPrepareParams { launch, prep, cwd } = params;
    let definition = prep.agent_definition_id.as_deref().and_then(|id| {
        load_agent_definitions()
            .into_iter()
            .find(|agent| agent.id == id)
    });
    let launch = if prep.restore {
        let mut launch = repair_agent_definition_launch(launch, definition.as_ref());
        // 记忆目录由智能体 id 决定，升级前保存的会话也能补上。
        attach_agent_memory_dirs(&mut launch, prep.agent_definition_id.as_deref());
        launch
    } else {
        prepare_agent_definition_launch(launch, definition.as_ref())
    };
    let skills =
        crate::pi_plugin_catalog::loaded_skills_for_launch(&launch, cwd.as_deref().map(Path::new));
    let mcp_servers = crate::pi_mcp::loaded_mcp_servers_for_launch(&launch);
    LaunchPrepareResult {
        launch,
        skills,
        mcp_servers,
    }
}

/// `session.loaded_skills`：一场 Pi 会话按启动规格和工作目录会加载哪些技能、连哪些 MCP。
pub fn loaded_skills(params: LoadedSkillsParams) -> LoadedSkillsResult {
    LoadedSkillsResult {
        skills: crate::pi_plugin_catalog::loaded_skills_for_launch(
            &params.launch,
            params.cwd.as_deref().map(Path::new),
        ),
        mcp_servers: crate::pi_mcp::loaded_mcp_servers_for_launch(&params.launch),
    }
}

pub fn prepare_agent_definition_launch(
    mut launch: ConversationLaunchSpec,
    definition: Option<&AgentDefinition>,
) -> ConversationLaunchSpec {
    if let Some(instructions) = definition.map(build_agent_instructions) {
        if instructions.is_empty() {
            launch.env.remove(SMELT_AGENT_INSTRUCTIONS_ENV);
        } else {
            launch
                .env
                .insert(SMELT_AGENT_INSTRUCTIONS_ENV.to_string(), instructions);
        }
    }
    // 记忆目录总是下发，开关不在这里判断：用户可能在会话中途开关记忆，Pi 每轮向 daemon
    // 取元数据时拿到当前开关，关着就不注入、不写入。启动时就不下发的话，中途打开无从生效。
    attach_agent_memory_dirs(
        &mut launch,
        definition.map(|definition| definition.id.as_str()),
    );
    // 有人值守是默认；只有自动化运行在拿到启动参数后再标成无人值守。
    launch.env.remove(SMELT_AGENT_UNATTENDED_ENV);
    // MCP 只给勾了服务器的产品智能体下发；其余情况（裸 Pi、没勾）都不能留着旧值。
    launch.env.remove(SMELT_AGENT_MCP_SERVERS_ENV);
    // 只要是产品智能体就走勾选式加载。哪怕一个都没勾也要下发参数，因为那时
    // 需要的恰恰是「关掉 Pi 的自动发现」——不下发就等于全量加载。
    if let Some(definition) = definition {
        let mut args = crate::pi_plugin_catalog::launch_args_for_selection(
            &definition.plugins,
            &crate::pi_plugin_catalog::selectable_plugins(),
        );
        // 工作区自带的技能包跟着目录走，不进勾选列表：绑定一个目录就意味着连
        // 它的技能一起带上，否则用户每绑一次都要再去卡片里勾一遍。
        args.extend(crate::pi_plugin_catalog::workspace_skill_args(
            &agent_workspace_skill_roots(definition),
        ));
        // 只连勾选的 MCP 服务器：名单交给启动器里的 MCP 扩展过滤 `mcp.json`。没勾就
        // 什么都不加——不注册 MCP、不起任何服务器进程。勾了的要把 codemode / tool-search
        // 加回来，否则默认暴露方式下的 MCP 工具模型够不着。
        if !definition.mcp_servers.is_empty()
            && let Ok(encoded) = serde_json::to_string(&definition.mcp_servers)
        {
            launch
                .env
                .insert(SMELT_AGENT_MCP_SERVERS_ENV.to_string(), encoded);
            for extension in PI_MCP_COMPANION_EXTENSIONS {
                args.push(
                    crate::pi_plugin_catalog::PiPluginKind::Extension
                        .load_flag()
                        .to_string(),
                );
                args.push(extension.to_string());
            }
        }
        if let Ok(encoded) = serde_json::to_string(&args) {
            launch
                .env
                .insert(SMELT_AGENT_PLUGIN_ARGS_ENV.to_string(), encoded);
        }
        match definition.model_selection() {
            Some((provider, model)) => {
                launch.env.insert(
                    SMELT_AGENT_MODEL_PROVIDER_ENV.to_string(),
                    provider.to_string(),
                );
                launch
                    .env
                    .insert(SMELT_AGENT_MODEL_ID_ENV.to_string(), model.to_string());
            }
            None => {
                launch.env.remove(SMELT_AGENT_MODEL_PROVIDER_ENV);
                launch.env.remove(SMELT_AGENT_MODEL_ID_ENV);
            }
        }
    }
    launch
}

/// 会自带技能包的位置：智能体 space 根，以及每个绑定目录。
fn agent_workspace_skill_roots(definition: &AgentDefinition) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(space) = agent_space_root(&definition.id) {
        roots.push(space);
    }
    roots.extend(
        definition
            .context_folders
            .iter()
            .map(|folder| folder.trim())
            .filter(|folder| !folder.is_empty())
            .map(PathBuf::from),
    );
    roots
}

pub fn ensure_agent_space(agent_definition_id: &str) -> Option<PathBuf> {
    let dir = agent_space_root(agent_definition_id)?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 按智能体身份写入两个记忆目录的启动变量；不是产品智能体就清掉。
///
/// 产品智能体会话的启动快照一定带插件参数（哪怕一个都没勾也会下发，见
/// [`prepare_agent_definition_launch`]）。旧版重启只保留了指令和记忆目录，把插件和 MCP
/// 勾选丢了，存下来的快照从此就是裸 Pi。这样的快照按智能体当前定义重新组装；
/// 完好的快照原样沿用创建时的内容。
pub fn repair_agent_definition_launch(
    launch: ConversationLaunchSpec,
    definition: Option<&AgentDefinition>,
) -> ConversationLaunchSpec {
    match definition {
        Some(definition) if !launch.env.contains_key(SMELT_AGENT_PLUGIN_ARGS_ENV) => {
            prepare_agent_definition_launch(launch, Some(definition))
        }
        _ => launch,
    }
}

/// 记忆目录完全由智能体 id 决定，所以恢复升级前保存的会话时也能补上，不必像说明、插件那样
/// 沿用创建时的快照。只有产品智能体有记忆：普通会话上万一带着同名 env（设置里手配的、
/// 旧快照留下的），也不能让它冒充某个智能体去读写记忆。
pub fn attach_agent_memory_dirs(
    launch: &mut ConversationLaunchSpec,
    agent_definition_id: Option<&str>,
) {
    launch.env.remove(SMELT_AGENT_MEMORY_DIR_ENV);
    launch.env.remove(SMELT_USER_MEMORY_DIR_ENV);
    let Some(id) = agent_definition_id else {
        return;
    };
    for (name, dir) in [
        (SMELT_AGENT_MEMORY_DIR_ENV, agent_memory_dir(id)),
        (SMELT_USER_MEMORY_DIR_ENV, user_memory_dir()),
    ] {
        // 启动器只读不建目录；建不出来就不下发，免得模型写记忆时才发现路径不存在。
        if let Some(dir) = dir.filter(|dir| std::fs::create_dir_all(dir).is_ok()) {
            launch
                .env
                .insert(name.to_string(), dir.to_string_lossy().into_owned());
        }
    }
}

/// 把绑定的目录与链接渲染成 system prompt 片段。
///
/// Pi 只支持单个 `--cwd`，没有 `--add-dir`，所以多目录唯一的落地方式就是把
/// 绝对路径告诉它，让它用 read/bash 自己去取。
pub fn render_agent_context_section(definition: &AgentDefinition) -> Option<String> {
    let folders: Vec<&str> = definition
        .context_folders
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();
    let links: Vec<&str> = definition
        .context_links
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();
    if folders.is_empty() && links.is_empty() {
        return None;
    }
    let mut section = String::from("## 绑定的上下文\n");
    if !folders.is_empty() {
        section.push_str(
            "\n以下目录属于本智能体的工作范围，可直接用绝对路径读写（它们不在当前工作目录下）：\n",
        );
        for folder in folders {
            section.push_str("- ");
            section.push_str(folder);
            section.push('\n');
        }
    }
    if !links.is_empty() {
        section.push_str("\n以下链接是本智能体的参考资料：\n");
        for link in links {
            section.push_str("- ");
            section.push_str(link);
            section.push('\n');
        }
    }
    Some(section)
}

/// 工作方式与绑定上下文合成一段 system prompt。上下文放在后面，读起来是
/// 「先讲怎么做事，再讲手上有什么」。
pub fn build_agent_instructions(definition: &AgentDefinition) -> String {
    let prompt = definition.prompt.trim();
    let context = render_agent_context_section(definition);
    match (prompt.is_empty(), context) {
        (true, None) => String::new(),
        (true, Some(context)) => context,
        (false, None) => prompt.to_string(),
        (false, Some(context)) => format!("{prompt}\n\n{context}"),
    }
}

/// 自动化运行带的指令：和对话同一份工作方式与绑定上下文，再说明 space 在哪。
/// 对话开在 space 根，自动化开在 space 下的子目录，不说的话它不会往上找。
pub fn build_automation_run_instructions(
    definition: &AgentDefinition,
    space: Option<&Path>,
) -> String {
    let instructions = build_agent_instructions(definition);
    let Some(space) = space else {
        return instructions;
    };
    let section = format!(
        "## 你的空间\n\n你在对话里攒下的文件都在 {}，可以直接用绝对路径读写。这次是自动化运行，当前工作目录是它下面这条自动化自己的子目录，产出写在这里。\n",
        space.display()
    );
    if instructions.is_empty() {
        section
    } else {
        format!("{instructions}\n\n{section}")
    }
}

/// 工作台「聊天」每次新开一场对话用的空目录：`~/.smelt/workspaces/conversations/<uuid>`。
///
/// 跟智能体 space 不同——Pi 裸对话没有长期归属对象，不能共用一个 cwd，否则
/// 多场对话会互相踩工作文件。目录落在工作台对话根下，侧栏会把它收进「对话」
/// 而不是当成用户项目。
pub fn ensure_workbench_conversation_workspace() -> Option<PathBuf> {
    let root = workbench_conversation_workspace_root()?;
    let dir = root.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 智能体定义单行写入的失败原因。调用方按种类映射到 Control API 错误码。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentDefinitionError {
    Invalid(String),
    NotFound(String),
    Conflict(String),
    FailedPrecondition(String),
    Store(String),
}

impl std::fmt::Display for AgentDefinitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message)
            | Self::NotFound(message)
            | Self::Conflict(message)
            | Self::FailedPrecondition(message)
            | Self::Store(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for AgentDefinitionError {}

impl From<String> for AgentDefinitionError {
    fn from(message: String) -> Self {
        Self::Store(message)
    }
}

impl From<smelt_store::StoreError> for AgentDefinitionError {
    fn from(error: smelt_store::StoreError) -> Self {
        Self::Store(error.to_string())
    }
}

pub fn list_agent_definitions_on(
    store: &smelt_store::Store,
) -> Result<Vec<AgentDefinition>, AgentDefinitionError> {
    match store.get_agent_ui_snapshot() {
        Ok(Some(snapshot)) => Ok(snapshot.agents.into_iter().map(agent_from_record).collect()),
        Ok(None) => Ok(Vec::new()),
        Err(error) => Err(AgentDefinitionError::Store(error.to_string())),
    }
}

pub fn get_agent_definition_on(
    store: &smelt_store::Store,
    id: &str,
) -> Result<AgentDefinition, AgentDefinitionError> {
    let id = require_id(id)?;
    list_agent_definitions_on(store)?
        .into_iter()
        .find(|agent| agent.id == id)
        .ok_or_else(|| AgentDefinitionError::NotFound(format!("智能体不存在: {id}")))
}

pub fn create_agent_definition_on(
    store: &smelt_store::Store,
    input: AgentDefinitionCreate,
) -> Result<AgentDefinition, AgentDefinitionError> {
    let engine_kind_id = normalize_engine_kind(input.engine_kind_id.as_deref())?;
    let id = match input
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => {
            if list_agent_definitions_on(store)?
                .iter()
                .any(|agent| agent.id == id)
            {
                return Err(AgentDefinitionError::Conflict(format!(
                    "智能体已存在: {id}"
                )));
            }
            id.to_string()
        }
        None => uuid::Uuid::new_v4().to_string(),
    };
    let definition = AgentDefinition {
        id,
        name: input.name.trim().to_string(),
        description: String::new(),
        engine_kind_id,
        prompt: input.prompt,
        plugins: normalize_list(input.plugins),
        mcp_servers: normalize_list(input.mcp_servers),
        context_folders: normalize_list(input.context_folders),
        context_links: normalize_list(input.context_links),
        model_provider: input.model_provider.unwrap_or_default().trim().to_string(),
        model_id: input.model_id.unwrap_or_default().trim().to_string(),
        memory_enabled: input.memory_enabled.unwrap_or(true),
    };
    store
        .insert_agent_definition(&record_from_definition(&definition))
        .map_err(|error| AgentDefinitionError::Store(error.to_string()))?;
    Ok(definition)
}

pub fn update_agent_definition_on(
    store: &smelt_store::Store,
    id: &str,
    patch: AgentDefinitionPatch,
) -> Result<AgentDefinition, AgentDefinitionError> {
    let id = require_id(id)?;
    modify_agent_definition_on(store, &id, |definition| {
        if let Some(name) = patch.name {
            definition.name = name.trim().to_string();
        }
        if let Some(prompt) = patch.prompt {
            definition.prompt = prompt;
        }
        if let Some(engine_kind_id) = patch.engine_kind_id {
            definition.engine_kind_id = normalize_engine_kind(Some(&engine_kind_id))?;
        }
        if let Some(plugins) = patch.plugins {
            definition.plugins = normalize_list(plugins);
        }
        if let Some(mcp_servers) = patch.mcp_servers {
            definition.mcp_servers = normalize_list(mcp_servers);
        }
        if let Some(folders) = patch.context_folders {
            definition.context_folders = normalize_list(folders);
        }
        if let Some(links) = patch.context_links {
            definition.context_links = normalize_list(links);
        }
        if let Some(model_provider) = patch.model_provider {
            definition.model_provider = model_provider.trim().to_string();
        }
        if let Some(model_id) = patch.model_id {
            definition.model_id = model_id.trim().to_string();
        }
        if let Some(memory_enabled) = patch.memory_enabled {
            definition.memory_enabled = memory_enabled;
        }
        Ok(())
    })
}

/// 所有更新都走这里：读-改-写落在同一个 SQLite 写事务里（见
/// `Store::modify_agent_definition`），GUI 与 smeltd 并发改同一条定义也不会互相覆盖。
fn modify_agent_definition_on(
    store: &smelt_store::Store,
    id: &str,
    modify: impl FnOnce(&mut AgentDefinition) -> Result<(), AgentDefinitionError>,
) -> Result<AgentDefinition, AgentDefinitionError> {
    store
        .modify_agent_definition(id, |record| {
            let mut definition = agent_from_record(record.clone());
            modify(&mut definition)?;
            *record = record_from_definition(&definition);
            Ok::<(), AgentDefinitionError>(())
        })?
        .map(agent_from_record)
        .ok_or_else(|| AgentDefinitionError::NotFound(format!("智能体不存在: {id}")))
}

/// 三方合并：只把 `before → after` 之间真正被改过的字段落到库里当前的行上，
/// 其余字段保留库里的值（可能刚被 CLI / Control API 改过）。
fn take_edited_fields(
    current: &mut AgentDefinition,
    before: &AgentDefinition,
    after: &AgentDefinition,
) {
    // 穷举解构：AgentDefinition 新增字段时这里编译失败，逼着决定它是否参与合并。
    let AgentDefinition {
        id: _,
        name: _,
        description: _,
        engine_kind_id: _,
        prompt: _,
        plugins: _,
        mcp_servers: _,
        context_folders: _,
        context_links: _,
        model_provider: _,
        model_id: _,
        memory_enabled: _,
    } = after;
    macro_rules! take {
        ($($field:ident),* $(,)?) => {
            $(if before.$field != after.$field {
                current.$field = after.$field.clone();
            })*
        };
    }
    take!(
        name,
        description,
        engine_kind_id,
        prompt,
        plugins,
        mcp_servers,
        context_folders,
        context_links,
        model_provider,
        model_id,
        memory_enabled,
    );
}

pub fn delete_agent_definition_on(
    store: &smelt_store::Store,
    id: &str,
) -> Result<(), AgentDefinitionError> {
    let id = require_id(id)?;
    let _ = get_agent_definition_on(store, &id)?;
    let using = automations_using_agent(store, &id)?;
    if using != 0 {
        return Err(AgentDefinitionError::FailedPrecondition(format!(
            "请先删除该智能体关联的 {using} 条自动化"
        )));
    }
    let deleted = store
        .delete_agent_definition(&id)
        .map_err(|error| AgentDefinitionError::Store(error.to_string()))?;
    if !deleted {
        return Err(AgentDefinitionError::NotFound(format!(
            "智能体不存在: {id}"
        )));
    }
    // 记忆属于智能体本身，定义没了就一起删。space 里其余的是工作产物，不动。
    if let Some(dir) = agent_memory_dir(&id) {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("[agent] 删除智能体记忆失败 {}: {error}", dir.display());
            }
        }
    }
    Ok(())
}

/// 把 GUI 内存里的定义清单对齐到 SQLite 单行写入，避免 `put_agent_ui_snapshot` 整表覆盖。
///
/// `before` 是 GUI 编辑前的副本，可能已落后于库（CLI / Control API 在此期间改过）。
/// 改动按字段三方合并，不拿旧副本整行回写；被别人删掉的定义报 NotFound，不会复活。
pub fn sync_agent_definitions_on(
    store: &smelt_store::Store,
    before: &[AgentDefinition],
    after: &[AgentDefinition],
) -> Result<(), AgentDefinitionError> {
    for agent in after {
        let id = require_id(&agent.id)?;
        match before.iter().find(|item| item.id == id) {
            None => {
                store
                    .insert_agent_definition(&record_from_definition(agent))
                    .map_err(|error| AgentDefinitionError::Store(error.to_string()))?;
            }
            Some(previous) if previous != agent => {
                modify_agent_definition_on(store, &id, |current| {
                    take_edited_fields(current, previous, agent);
                    Ok(())
                })?;
            }
            Some(_) => {}
        }
    }
    for previous in before {
        if after.iter().any(|agent| agent.id == previous.id) {
            continue;
        }
        delete_agent_definition_on(store, &previous.id)?;
    }
    Ok(())
}

pub fn list_agent_definitions() -> Result<Vec<AgentDefinition>, AgentDefinitionError> {
    list_agent_definitions_on(&default_store()?)
}

pub fn create_agent_definition(
    input: AgentDefinitionCreate,
) -> Result<AgentDefinition, AgentDefinitionError> {
    create_agent_definition_on(&default_store()?, input)
}

pub fn update_agent_definition(
    id: &str,
    patch: AgentDefinitionPatch,
) -> Result<AgentDefinition, AgentDefinitionError> {
    update_agent_definition_on(&default_store()?, id, patch)
}

pub fn delete_agent_definition(id: &str) -> Result<(), AgentDefinitionError> {
    delete_agent_definition_on(&default_store()?, id)
}

pub fn sync_agent_definitions(
    before: &[AgentDefinition],
    after: &[AgentDefinition],
) -> Result<(), AgentDefinitionError> {
    sync_agent_definitions_on(&default_store()?, before, after)
}

fn default_store() -> Result<smelt_store::Store, AgentDefinitionError> {
    sqlite_state::default_sqlite_store().map_err(AgentDefinitionError::Store)
}

fn require_id(id: &str) -> Result<String, AgentDefinitionError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(AgentDefinitionError::Invalid("智能体 id 不能为空".into()));
    }
    Ok(id.to_string())
}

fn normalize_engine_kind(engine_kind_id: Option<&str>) -> Result<String, AgentDefinitionError> {
    let engine_kind_id = engine_kind_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| ConversationAgentKind::Pi.id());
    if ConversationAgentKind::from_id(engine_kind_id).is_none() {
        return Err(AgentDefinitionError::Invalid(format!(
            "不支持的智能体执行引擎: {engine_kind_id}"
        )));
    }
    Ok(engine_kind_id.to_string())
}

fn normalize_list(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        let value = value.trim().to_string();
        if value.is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        out.push(value);
    }
    out
}

fn record_from_definition(agent: &AgentDefinition) -> smelt_store::AgentDefinitionRecord {
    smelt_store::AgentDefinitionRecord {
        id: agent.id.clone(),
        name: agent.name.clone(),
        description: agent.description.clone(),
        engine_kind_id: agent.engine_kind_id.clone(),
        prompt: agent.prompt.clone(),
        plugins_json: serde_json::to_vec(&agent.plugins).unwrap_or_else(|_| b"[]".to_vec()),
        mcp_servers_json: serde_json::to_vec(&agent.mcp_servers).unwrap_or_else(|_| b"[]".to_vec()),
        context_folders_json: serde_json::to_vec(&agent.context_folders)
            .unwrap_or_else(|_| b"[]".to_vec()),
        context_links_json: serde_json::to_vec(&agent.context_links)
            .unwrap_or_else(|_| b"[]".to_vec()),
        model_provider: agent.model_provider.clone(),
        model_id: agent.model_id.clone(),
        memory_enabled: agent.memory_enabled,
    }
}

fn automations_using_agent(
    store: &smelt_store::Store,
    agent_id: &str,
) -> Result<usize, AgentDefinitionError> {
    let Some(snapshot) = store
        .get_automation_snapshot()
        .map_err(|error| AgentDefinitionError::Store(error.to_string()))?
    else {
        return Ok(0);
    };
    let mut count = 0;
    for automation in snapshot.automations {
        let action: AutomationAction = serde_json::from_slice(&automation.action_json)
            .map_err(|error| AgentDefinitionError::Store(error.to_string()))?;
        if action.agent_definition_id() == Some(agent_id) {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_agent_instructions_are_pi_system_prompt_input_not_a_user_message() {
        let definition = AgentDefinition {
            id: "quant".into(),
            name: "量化智能体".into(),
            description: String::new(),
            engine_kind_id: crate::agent_kind::ConversationAgentKind::Pi.id().into(),
            prompt: "  先验证行情时间\n再给出结论  ".into(),
            plugins: Vec::new(),
            context_folders: Vec::new(),
            context_links: Vec::new(),
            ..Default::default()
        };
        let launch = crate::agent_kind::ConversationLaunchSpec::from_command("smelt-pi-agent")
            .with_env("EXISTING", "keep");

        let launch = prepare_agent_definition_launch(launch, Some(&definition));

        assert_eq!(launch.env.get("EXISTING").map(String::as_str), Some("keep"));
        assert_eq!(
            launch
                .env
                .get(crate::agent_kind::SMELT_AGENT_INSTRUCTIONS_ENV)
                .map(String::as_str),
            Some("先验证行情时间\n再给出结论")
        );
        assert_eq!(launch.command, "smelt-pi-agent");
    }

    /// GUI 拿着打开面板时的旧副本改了 prompt；在此期间 CLI / Control API 改了 name。
    /// GUI 保存只能落下它自己改过的字段，不能用旧副本把 CLI 的修改覆盖回去。
    #[test]
    fn gui_sync_merges_onto_concurrent_daemon_update() {
        let (_dir, store) = open_store();
        let created = create_agent_definition_on(
            &store,
            AgentDefinitionCreate {
                id: Some("writer".into()),
                name: "旧名字".into(),
                engine_kind_id: Some("pi".into()),
                prompt: "旧 prompt".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let gui_before = vec![created];

        update_agent_definition_on(
            &store,
            "writer",
            AgentDefinitionPatch {
                name: Some("CLI 改的名字".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let mut gui_after = gui_before.clone();
        gui_after[0].prompt = "GUI 改的 prompt".into();
        sync_agent_definitions_on(&store, &gui_before, &gui_after).unwrap();

        let stored = get_agent_definition_on(&store, "writer").unwrap();
        assert_eq!(stored.name, "CLI 改的名字");
        assert_eq!(stored.prompt, "GUI 改的 prompt");
    }

    /// GUI 打开面板后 CLI 删掉了某个智能体，GUI 再改它：这是真冲突，要报 NotFound
    /// 让界面重新对齐，不能用旧副本把它悄悄复活。
    #[test]
    fn gui_sync_does_not_resurrect_concurrently_deleted_agent() {
        let (_dir, store) = open_store();
        let created = create_agent_definition_on(
            &store,
            AgentDefinitionCreate {
                id: Some("gone".into()),
                name: "要被删的".into(),
                engine_kind_id: Some("pi".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let gui_before = vec![created];
        delete_agent_definition_on(&store, "gone").unwrap();

        let mut gui_after = gui_before.clone();
        gui_after[0].prompt = "改一下".into();
        let error = sync_agent_definitions_on(&store, &gui_before, &gui_after).unwrap_err();

        assert!(matches!(error, AgentDefinitionError::NotFound(_)));
        assert!(list_agent_definitions_on(&store).unwrap().is_empty());
    }

    fn definition_with_context(folders: &[&str], links: &[&str]) -> AgentDefinition {
        AgentDefinition {
            id: "space-agent".into(),
            name: "Space".into(),
            engine_kind_id: "pi".into(),
            prompt: "先读需求再动手".into(),
            context_folders: folders.iter().map(|it| it.to_string()).collect(),
            context_links: links.iter().map(|it| it.to_string()).collect(),
            ..Default::default()
        }
    }

    /// Pi 只接受单个 cwd，没有 `--add-dir`。绑定的目录只能靠 system prompt
    /// 里的绝对路径生效，所以这段拼接是多目录唯一的落地点。
    #[test]
    fn bound_context_is_appended_after_the_working_instructions() {
        let definition = definition_with_context(&["/data/novel"], &["https://example.com/spec"]);

        let instructions = build_agent_instructions(&definition);

        let prompt_at = instructions.find("先读需求再动手").unwrap();
        let context_at = instructions.find("## 绑定的上下文").unwrap();
        assert!(prompt_at < context_at, "工作方式要排在上下文前面");
        assert!(instructions.contains("- /data/novel"));
        assert!(instructions.contains("- https://example.com/spec"));
    }

    /// 没绑定任何东西时不能凭空多出一个空章节，否则每个智能体都要背一段废话。
    #[test]
    fn instructions_stay_untouched_without_any_bound_context() {
        let definition = definition_with_context(&[], &["   "]);

        assert_eq!(build_agent_instructions(&definition), "先读需求再动手");
        assert!(render_agent_context_section(&definition).is_none());
    }

    /// 工作方式和上下文都空时要清掉 env，不能让上一次的残留继续生效。
    #[test]
    fn empty_definition_clears_a_stale_instructions_environment_value() {
        let mut definition = definition_with_context(&[], &[]);
        definition.prompt = "   ".into();
        let mut launch = ConversationAgentKind::Pi.default_launch();
        launch
            .env
            .insert(SMELT_AGENT_INSTRUCTIONS_ENV.into(), "stale".into());

        let launch = prepare_agent_definition_launch(launch, Some(&definition));

        assert_eq!(launch.env.get(SMELT_AGENT_INSTRUCTIONS_ENV), None);
    }

    #[test]
    fn bound_model_is_injected_into_the_launch_environment() {
        let mut definition = definition_with_context(&[], &[]);
        definition.model_provider = "Token-X".into();
        definition.model_id = "Claude-Opus-5".into();
        let launch = prepare_agent_definition_launch(
            ConversationAgentKind::Pi.default_launch(),
            Some(&definition),
        );
        assert_eq!(
            launch
                .env
                .get(SMELT_AGENT_MODEL_PROVIDER_ENV)
                .map(String::as_str),
            Some("Token-X")
        );
        assert_eq!(
            launch.env.get(SMELT_AGENT_MODEL_ID_ENV).map(String::as_str),
            Some("Claude-Opus-5")
        );
    }

    /// 智能体对话可以开在任意目录（智能体自己的 space，或某个普通仓库），
    /// 所以归属只能读桌面存下来的绑定。这条测试锁住「开在普通项目里的对话
    /// 照样认得出主人」——正是 cwd 反推做不到的那一半。
    #[test]
    fn a_conversation_started_inside_a_plain_repo_still_resolves_its_agent() {
        let map = session_agent_definition_ids(&[
            ("acp-1", "/Users/me/Desktop/novel", Some("writer")),
            ("acp-2", "/Users/me/.smelt/agents/quant", Some("quant")),
            ("acp-3", "/repo", None),
        ]);

        assert_eq!(map.get("acp-1").map(String::as_str), Some("writer"));
        assert_eq!(map.get("acp-2").map(String::as_str), Some("quant"));
        // 普通 ACP 会话没有归属，不能凭空补一个。
        assert_eq!(map.get("acp-3"), None);
        assert_eq!(map.len(), 2);
    }

    /// 空串等于没绑定。留着它会让移动端拿一个配不上任何定义的 id 去查表，
    /// 最后显示成通用兜底标签，比干脆不标还费解。
    #[test]
    fn blank_bindings_are_not_treated_as_an_agent() {
        let map = session_agent_definition_ids(&[("acp-1", "/repo", Some("  "))]);
        assert_eq!(map.get("acp-1"), None);
        assert!(map.is_empty());
    }

    /// 历史页、工具面板只拿得到一个 cwd，靠反解认出「现在在某个智能体的上下文
    /// 里」，从而按智能体而不是裸引擎去续接。
    #[test]
    fn a_space_path_resolves_back_to_its_agent_definition() {
        let space = agent_space_root("space-agent").unwrap();

        assert_eq!(
            agent_definition_id_for_space(&space).as_deref(),
            Some("space-agent")
        );
        // 子目录是智能体的工作产物，不是另一个智能体。
        assert_eq!(agent_definition_id_for_space(&space.join("notes")), None);
        assert_eq!(
            agent_definition_id_for_space(std::path::Path::new("/Users/me/Desktop/project")),
            None
        );
    }

    /// 绑定目录自带的技能包要跟着目录一起进会话，不用再去插件卡片里勾一遍。
    #[test]
    fn skills_shipped_inside_a_bound_folder_are_loaded_with_it() {
        let folder = std::env::temp_dir().join(format!("smelt-bound-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(folder.join(".agents/skills/demo")).unwrap();
        std::fs::create_dir_all(folder.join(".claude/skills/demo")).unwrap();
        let definition = definition_with_context(&[folder.to_string_lossy().as_ref()], &[]);

        let launch = prepare_agent_definition_launch(
            ConversationAgentKind::Pi.default_launch(),
            Some(&definition),
        );

        let args: Vec<String> =
            serde_json::from_str(launch.env.get(SMELT_AGENT_PLUGIN_ARGS_ENV).unwrap()).unwrap();
        let expected = folder.join(".claude/skills").to_string_lossy().into_owned();
        assert!(
            args.contains(&expected),
            "绑定目录的兼容技能没进启动参数: {args:?}"
        );
        let expected = folder.join(".agents/skills").to_string_lossy().into_owned();
        assert!(
            args.contains(&expected),
            "绑定目录的通用 Agent Skills 没进启动参数: {args:?}"
        );
        // 自动发现仍然要关掉，否则勾选式加载就成了摆设。
        assert!(args.contains(&"--no-skills".to_string()));
        std::fs::remove_dir_all(&folder).ok();
    }

    /// 被旧版重启丢了插件参数的快照按当前定义补全；完好的快照不动。
    #[test]
    fn launch_that_lost_its_selection_is_rebuilt_from_the_definition() {
        let mut definition = definition_with_context(&[], &[]);
        definition.mcp_servers = vec!["rea".into()];
        let mut damaged = ConversationAgentKind::Pi.default_launch();
        damaged
            .env
            .insert(SMELT_AGENT_INSTRUCTIONS_ENV.to_string(), "old".into());

        let repaired = repair_agent_definition_launch(damaged.clone(), Some(&definition));
        assert!(repaired.env.contains_key(SMELT_AGENT_PLUGIN_ARGS_ENV));
        let selected: Vec<String> =
            serde_json::from_str(repaired.env.get(SMELT_AGENT_MCP_SERVERS_ENV).unwrap()).unwrap();
        assert_eq!(selected, ["rea"]);

        let intact = prepare_agent_definition_launch(damaged.clone(), Some(&definition));
        definition.mcp_servers = vec!["figma".into()];
        assert_eq!(
            repair_agent_definition_launch(intact.clone(), Some(&definition)),
            intact,
            "完好的快照沿用创建时的勾选"
        );
        assert_eq!(
            repair_agent_definition_launch(damaged.clone(), None),
            damaged
        );
    }

    /// 智能体只连勾选的 MCP 服务器；没勾就完全不碰 MCP，也不残留上一次的名单。
    #[test]
    fn mcp_selection_reaches_the_launcher_only_when_checked() {
        let mut definition = definition_with_context(&[], &[]);
        let mut stale = ConversationAgentKind::Pi.default_launch();
        stale
            .env
            .insert(SMELT_AGENT_MCP_SERVERS_ENV.to_string(), "[\"old\"]".into());

        let launch = prepare_agent_definition_launch(stale.clone(), Some(&definition));
        assert!(!launch.env.contains_key(SMELT_AGENT_MCP_SERVERS_ENV));
        let args: Vec<String> =
            serde_json::from_str(launch.env.get(SMELT_AGENT_PLUGIN_ARGS_ENV).unwrap()).unwrap();
        assert!(
            !args.iter().any(|arg| arg.starts_with("builtin:")),
            "没勾 MCP 就不该加载内置扩展: {args:?}"
        );

        definition.mcp_servers = vec!["figma".into(), "rea".into()];
        let launch = prepare_agent_definition_launch(stale.clone(), Some(&definition));
        let selected: Vec<String> =
            serde_json::from_str(launch.env.get(SMELT_AGENT_MCP_SERVERS_ENV).unwrap()).unwrap();
        assert_eq!(selected, ["figma", "rea"]);
        let args: Vec<String> =
            serde_json::from_str(launch.env.get(SMELT_AGENT_PLUGIN_ARGS_ENV).unwrap()).unwrap();
        for extension in PI_MCP_COMPANION_EXTENSIONS {
            assert!(
                args.contains(&extension.to_string()),
                "缺 {extension}: {args:?}"
            );
        }

        let plain = prepare_agent_definition_launch(stale, None);
        assert!(
            !plain.env.contains_key(SMELT_AGENT_MCP_SERVERS_ENV),
            "裸 Pi 走内置 MCP 读全量 mcp.json，不能带着智能体的名单"
        );
    }

    /// space 根目录必须由 id 唯一决定：同一个智能体的每段对话都要落回同一处，
    /// 否则 agent 攒下的文件下次就找不到了。
    #[test]
    fn agent_space_root_is_stable_per_definition_and_rejects_path_escapes() {
        let first = agent_space_root("space-agent").unwrap();
        assert_eq!(first, agent_space_root("space-agent").unwrap());
        assert!(first.ends_with("space-agent"));
        assert!(is_agent_space(&first));

        assert!(agent_space_root("../../etc").is_none());
        assert!(agent_space_root(" ").is_none());
    }

    #[test]
    fn legacy_agent_presets_resolve_without_reviving_profile_context() {
        let config: AgentUiProjection = serde_json::from_value(serde_json::json!({
            "agent_presets": [{
                "id": "legacy",
                "name": "Legacy Pi",
                "agent_id": "pi",
                "profile_id": "removed-profile",
                "working_directory": "/old/project",
                "prompt": "Keep these instructions"
            }]
        }))
        .unwrap();

        let execution = resolve_agent_execution_definition(config, "legacy").unwrap();

        assert_eq!(execution.kind, ConversationAgentKind::Pi);
        assert_eq!(execution.definition.id, "legacy");
        assert_eq!(
            execution.launch.env.get(SMELT_AGENT_INSTRUCTIONS_ENV),
            Some(&"Keep these instructions".to_string())
        );
    }

    #[test]
    fn typed_snapshot_supplies_agents_command_and_env() {
        let snapshot = smelt_store::AgentUiSnapshot {
            agent_hooks_enabled: true,
            commands: vec![smelt_store::AgentConversationCommandRecord {
                command_key: ConversationAgentKind::Pi
                    .descriptor()
                    .config_key
                    .to_string(),
                command: "smelt-pi-agent --x".into(),
            }],
            env: vec![smelt_store::AgentAcpEnvRecord {
                engine_kind_id: "pi".into(),
                name: "FOO".into(),
                value: "bar".into(),
            }],
            config_memory: Vec::new(),
            agents: vec![smelt_store::AgentDefinitionRecord {
                id: "writer".into(),
                name: "Writer".into(),
                description: String::new(),
                engine_kind_id: "pi".into(),
                prompt: "Write".into(),
                plugins_json: b"[]".to_vec(),
                mcp_servers_json: b"[]".to_vec(),
                context_folders_json: b"[]".to_vec(),
                context_links_json: b"[]".to_vec(),
                model_provider: String::new(),
                model_id: String::new(),
                memory_enabled: true,
            }],
            profiles: Vec::new(),
        };

        let execution =
            resolve_agent_execution_definition(projection_from_snapshot(snapshot), "writer")
                .unwrap();

        assert_eq!(execution.launch.command, "smelt-pi-agent --x");
        assert_eq!(execution.launch.env.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn non_pi_product_agent_is_rejected_for_automation() {
        let config: AgentUiProjection = serde_json::from_value(serde_json::json!({
            "agents": [{
                "id": "codex-agent",
                "name": "Codex",
                "agent_id": "codex",
                "prompt": "Review code"
            }]
        }))
        .unwrap();

        let error = resolve_agent_execution_definition(config, "codex-agent").unwrap_err();

        assert!(error.contains("只支持 Pi"));
    }

    #[test]
    fn product_instructions_override_a_conflicting_runtime_environment_value() {
        let definition = AgentDefinition {
            id: "agent-1".into(),
            name: "Reviewer".into(),
            engine_kind_id: "pi".into(),
            prompt: "Review carefully".into(),
            ..Default::default()
        };
        let mut launch = ConversationAgentKind::Pi.default_launch();
        launch.env.insert(
            SMELT_AGENT_INSTRUCTIONS_ENV.into(),
            "stale instructions".into(),
        );
        let launch = prepare_agent_definition_launch(launch, Some(&definition));
        assert_eq!(
            launch.env.get(SMELT_AGENT_INSTRUCTIONS_ENV),
            Some(&"Review carefully".to_string())
        );
    }

    #[test]
    fn product_agent_launch_carries_both_memory_directories() {
        let definition = definition_with_context(&[], &[]);
        // 有人值守是默认：只有自动化运行在拿到启动参数后才加上无人值守标记。
        let stale = ConversationAgentKind::Pi
            .default_launch()
            .with_env(SMELT_AGENT_UNATTENDED_ENV, "1");

        let launch = prepare_agent_definition_launch(stale, Some(&definition));
        assert_eq!(launch.env.get(SMELT_AGENT_UNATTENDED_ENV), None);

        let agent_dir = agent_memory_dir("space-agent").unwrap();
        let user_dir = user_memory_dir().unwrap();
        assert_eq!(
            launch
                .env
                .get(SMELT_AGENT_MEMORY_DIR_ENV)
                .map(PathBuf::from),
            Some(agent_dir.clone())
        );
        assert_eq!(
            launch.env.get(SMELT_USER_MEMORY_DIR_ENV).map(PathBuf::from),
            Some(user_dir.clone())
        );
        // 启动器只读不建目录；目录不存在时模型第一次写记忆就会失败。
        assert!(agent_dir.is_dir());
        assert!(user_dir.is_dir());
        assert!(agent_dir.starts_with(agent_space_root("space-agent").unwrap()));
    }

    /// 开关不在启动时判断：关着记忆启动的会话也要拿到目录，用户中途打开时下一轮才能生效。
    #[test]
    fn an_agent_with_memory_turned_off_still_launches_with_its_memory_directories() {
        let mut definition = definition_with_context(&[], &[]);
        definition.memory_enabled = false;

        let launch = prepare_agent_definition_launch(
            ConversationAgentKind::Pi.default_launch(),
            Some(&definition),
        );

        assert_eq!(
            launch
                .env
                .get(SMELT_AGENT_MEMORY_DIR_ENV)
                .map(PathBuf::from),
            agent_memory_dir("space-agent")
        );
    }

    /// 普通 Pi 会话没有记忆。设置里万一配了同名 env，也不能让它冒充产品智能体。
    #[test]
    fn plain_launch_never_carries_memory_directories() {
        let mut launch = ConversationAgentKind::Pi.default_launch();
        launch
            .env
            .insert(SMELT_AGENT_MEMORY_DIR_ENV.into(), "/tmp/stale".into());
        launch
            .env
            .insert(SMELT_USER_MEMORY_DIR_ENV.into(), "/tmp/stale".into());

        let launch = prepare_agent_definition_launch(launch, None);

        assert_eq!(launch.env.get(SMELT_AGENT_MEMORY_DIR_ENV), None);
        assert_eq!(launch.env.get(SMELT_USER_MEMORY_DIR_ENV), None);
    }

    /// 升级前保存的会话没有记忆变量；恢复时按它记录的智能体 id 补上。
    #[test]
    fn restored_sessions_get_memory_directories_from_their_agent_id() {
        let mut launch = ConversationAgentKind::Pi.default_launch();

        attach_agent_memory_dirs(&mut launch, Some("space-agent"));

        assert_eq!(
            launch
                .env
                .get(SMELT_AGENT_MEMORY_DIR_ENV)
                .map(PathBuf::from),
            agent_memory_dir("space-agent")
        );
        assert_eq!(
            launch.env.get(SMELT_USER_MEMORY_DIR_ENV).map(PathBuf::from),
            user_memory_dir()
        );

        attach_agent_memory_dirs(&mut launch, None);
        assert_eq!(launch.env.get(SMELT_AGENT_MEMORY_DIR_ENV), None);
        assert_eq!(launch.env.get(SMELT_USER_MEMORY_DIR_ENV), None);
    }

    #[test]
    fn deleting_an_agent_deletes_its_memory_but_keeps_the_user_layer() {
        let (_dir, store) = open_store();
        let id = format!("memory-{}", uuid::Uuid::new_v4());
        create_sample(&store, &id);
        let agent_dir = agent_memory_dir(&id).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("MEMORY.md"), "- [a](a.md) — a\n").unwrap();
        let space_note = agent_space_root(&id).unwrap().join("notes.md");
        std::fs::write(&space_note, "工作产物").unwrap();
        let user_dir = user_memory_dir().unwrap();
        std::fs::create_dir_all(&user_dir).unwrap();

        delete_agent_definition_on(&store, &id).unwrap();

        assert!(!agent_dir.exists(), "智能体记忆要跟着删掉");
        assert!(space_note.exists(), "只删记忆，不动智能体目录里的工作产物");
        assert!(user_dir.is_dir(), "用户全局记忆不属于任何一个智能体");
        let _ = std::fs::remove_dir_all(agent_space_root(&id).unwrap());
    }

    #[test]
    fn workbench_conversation_workspace_is_not_a_project_repo() {
        let root = workbench_conversation_workspace_root().expect("home dir");
        assert!(
            root.ends_with(std::path::Path::new("workspaces/conversations")),
            "{root:?}"
        );
        assert!(is_workbench_conversation_workspace(&root.join("chat-1")));
        assert!(!is_workbench_conversation_workspace(std::path::Path::new(
            "/workspace/project"
        )));
    }

    #[test]
    fn ensure_workbench_conversation_workspace_lands_in_conversation_rail() {
        let dir = ensure_workbench_conversation_workspace().expect("home dir");
        assert!(is_workbench_conversation_workspace(&dir));
        let cwd = dir.to_string_lossy();
        assert!(crate::session_control::is_agent_conversation(
            None,
            None,
            Some(cwd.as_ref())
        ));
        assert!(crate::session_control::is_product_conversation(
            None,
            None,
            None,
            Some(cwd.as_ref())
        ));
        // 自动化 Run 借用同一目录时仍然归自动化，不进「对话」。
        assert!(!crate::session_control::is_agent_conversation(
            Some("auto-1"),
            None,
            Some(cwd.as_ref())
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn open_store() -> (tempfile::TempDir, smelt_store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        (dir, store)
    }

    fn create_sample(store: &smelt_store::Store, id: &str) -> AgentDefinition {
        create_agent_definition_on(
            store,
            AgentDefinitionCreate {
                id: Some(id.into()),
                name: "写作助手".into(),
                prompt: "先列提纲".into(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn create_list_update_and_delete_are_row_level() {
        let (_dir, store) = open_store();
        let created = create_sample(&store, "writer");
        assert_eq!(created.engine_kind_id, ConversationAgentKind::Pi.id());
        assert_eq!(list_agent_definitions_on(&store).unwrap().len(), 1);

        let updated = update_agent_definition_on(
            &store,
            "writer",
            AgentDefinitionPatch {
                prompt: Some("改成先读原文".into()),
                plugins: Some(vec!["skill:demo".into(), " skill:demo ".into(), "".into()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.prompt, "改成先读原文");
        assert_eq!(updated.plugins, ["skill:demo"]);

        delete_agent_definition_on(&store, "writer").unwrap();
        assert!(list_agent_definitions_on(&store).unwrap().is_empty());
    }

    #[test]
    fn create_rejects_duplicate_ids_and_unknown_engines() {
        let (_dir, store) = open_store();
        create_sample(&store, "writer");
        let duplicate = create_agent_definition_on(
            &store,
            AgentDefinitionCreate {
                id: Some("writer".into()),
                name: "另一个".into(),
                ..Default::default()
            },
        );
        assert!(matches!(duplicate, Err(AgentDefinitionError::Conflict(_))));

        let unknown = create_agent_definition_on(
            &store,
            AgentDefinitionCreate {
                engine_kind_id: Some("nope".into()),
                name: "坏的".into(),
                ..Default::default()
            },
        );
        assert!(matches!(unknown, Err(AgentDefinitionError::Invalid(_))));
    }

    #[test]
    fn delete_is_blocked_when_an_automation_still_references_the_agent() {
        let (_dir, store) = open_store();
        create_sample(&store, "writer");
        let action = AutomationAction::agent("writer", Some("跑一次".into()));
        store
            .put_automation_snapshot(&smelt_store::AutomationSnapshot {
                schema_version: 1,
                store_id: "store-1".into(),
                revision: 1,
                timezone_fingerprint: "test".into(),
                automations: vec![smelt_store::AutomationRecord {
                    id: "auto-1".into(),
                    name: "定时写".into(),
                    enabled: true,
                    workspace_dir: None,
                    trigger_json: b"{}".to_vec(),
                    action_json: serde_json::to_vec(&action).unwrap(),
                    sinks_json: b"[]".to_vec(),
                }],
                states: Vec::new(),
                runs: Vec::new(),
            })
            .unwrap();

        let error = delete_agent_definition_on(&store, "writer").unwrap_err();
        assert!(matches!(error, AgentDefinitionError::FailedPrecondition(_)));
        assert_eq!(list_agent_definitions_on(&store).unwrap().len(), 1);
    }

    #[test]
    fn syncing_definitions_does_not_clobber_rows_the_caller_did_not_touch() {
        let (_dir, store) = open_store();
        create_sample(&store, "writer");
        create_sample(&store, "reviewer");
        let before = vec![AgentDefinition {
            id: "writer".into(),
            name: "写作助手".into(),
            engine_kind_id: ConversationAgentKind::Pi.id().into(),
            prompt: "先列提纲".into(),
            ..Default::default()
        }];
        let after = vec![AgentDefinition {
            id: "writer".into(),
            name: "写作助手".into(),
            engine_kind_id: ConversationAgentKind::Pi.id().into(),
            prompt: "改稿".into(),
            ..Default::default()
        }];

        sync_agent_definitions_on(&store, &before, &after).unwrap();

        let listed = list_agent_definitions_on(&store).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed
                .iter()
                .find(|agent| agent.id == "writer")
                .unwrap()
                .prompt,
            "改稿"
        );
        assert!(listed.iter().any(|agent| agent.id == "reviewer"));
    }
}
