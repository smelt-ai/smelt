//! 产品智能体 / 自动化 / 启动命令 / 通知开关的宿主状态（SQLite 关系表）。
//!
//! 需要 `gpui::Global`，所以不能进不许引 GPUI 的 smelt-core。设置页怎么渲染
//! 这些字段仍在主 crate 的 settings 模块。

use gpui::{App, Global};
use std::collections::{BTreeMap, HashSet};

use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::automation::{Automation, AutomationRun, AutomationState};
pub use smelt_core::agent_definition::AgentDefinition;
use smelt_core::agent_host::{AgentHostEdit, AgentHostSettings};
use smelt_core::agent_kind::{
    AcpProfile, CONVERSATION_AGENTS, ConversationAgentKind, ConversationLaunchSpec,
};
use smelt_core::automation::AutomationFile;
use smelt_core::control_api::{AgentHostApplyEdit, AgentHostGet};
use smelt_runtime_client::protocol::control::EmptyParams;

/// 读写运行时配置的上限。一次合并写库，正常是毫秒级。
const RUNTIME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn default_true() -> bool {
    true
}

/// 各家 ACP agent 的启动命令，按 agent id 存。
///
/// 之所以不是四个具名字段：新增一家 agent 不该需要「加字段 + 加 serde 默认 +
/// 加两处 match」。这里只认 `CONVERSATION_AGENTS` 这张表，加一行即全通。
///
/// 落盘键名仍是历史的 `acp_cmd` / `acp_copilot_cmd` / …（由描述符的
/// `config_key` 提供），改名等于把用户自定义命令悄悄重置回默认。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationCommands(BTreeMap<&'static str, String>);

impl ConversationCommands {
    /// 出厂值：每家都按描述符的默认命令填一份，新装用户落盘的 JSON 与
    /// 重构前逐字节一致。
    fn with_defaults() -> Self {
        Self(
            CONVERSATION_AGENTS
                .iter()
                .map(|d| (d.id, (d.default_cmd)()))
                .collect(),
        )
    }

    fn get(&self, agent: ConversationAgentKind) -> String {
        self.0
            .get(agent.id())
            .cloned()
            .unwrap_or_else(|| agent.default_cmd())
    }

    fn set(&mut self, agent: ConversationAgentKind, cmd: String) {
        self.0.insert(agent.id(), cmd);
    }
}

impl Serialize for ConversationCommands {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        // 按表顺序写，键名用历史字段名。
        for descriptor in CONVERSATION_AGENTS {
            if let Some(cmd) = self.0.get(descriptor.id) {
                map.serialize_entry(descriptor.config_key, cmd)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ConversationCommands {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor;

        impl<'de> Visitor<'de> for MapVisitor {
            type Value = ConversationCommands;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("ACP agent 启动命令表")
            }

            fn visit_map<M: MapAccess<'de>>(
                self,
                mut access: M,
            ) -> Result<ConversationCommands, M::Error> {
                let mut cmds = BTreeMap::new();
                while let Some(key) = access.next_key::<String>()? {
                    // 这个类型被 `#[serde(flatten)]` 承接，会收到本结构体没
                    // 声明的全部键；不是命令键就照旧丢弃。
                    match CONVERSATION_AGENTS.iter().find(|d| d.config_key == key) {
                        Some(descriptor) => {
                            cmds.insert(descriptor.id, access.next_value::<String>()?);
                        }
                        None => {
                            access.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(ConversationCommands(cmds))
            }
        }

        deserializer.deserialize_map(MapVisitor)
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentHostState {
    /// 是否由 Smelt 管理各 CLI agent 的结构化 hooks。旧配置缺少此字段时，加载
    /// 迁移会将其开启并写回；用户明确保存 false 后，后续升级不会重新开启。
    #[serde(default)]
    pub agent_hooks_enabled: bool,
    /// 是否允许 Smelt 会话通过内置 MCP 相互发消息。旧配置缺少此字段时保持开启，
    /// 避免升级后悄悄改变既有会话的能力。
    #[serde(default = "default_true")]
    pub cross_agent_enabled: bool,
    #[serde(default = "default_true", alias = "notify_awaiting")]
    pub notify_approval: bool,
    #[serde(default = "default_true")]
    pub notify_input: bool,
    #[serde(default = "default_true")]
    pub notify_success: bool,
    #[serde(default = "default_true")]
    pub notify_failure: bool,
    #[serde(default = "default_true")]
    pub notify_terminal_bell: bool,
    /// 各家 ACP 会话的 agent 启动命令（空白分词）。只在命令行可表达的权限模式
    /// 仍由各 adapter 自己上报；OpenCode 这类只能走配置层的默认覆盖由描述符提供，
    /// 不塞进这个用户可编辑的命令字符串。
    ///
    /// `flatten` 让它继续占用旧配置里那几个平铺的键名，不引入嵌套层级。
    #[serde(flatten)]
    pub acp_cmds: ConversationCommands,
    /// 各家 ACP agent 额外注入的环境变量，按 agent id 存。
    ///
    /// 用途是把普通 ACP adapter 的额外环境变量结构化保存。原生 dsh profile
    /// 不读取这里：它的凭据和设置必须继续由 dsh 自己管理，两个 Host 才会看到
    /// 完全相同的运行时配置。
    ///
    /// 单独一个嵌套键（不像命令那样平铺）：这是新字段，没有历史键名包袱，嵌套
    /// 能让"加一家 agent"继续只改 `CONVERSATION_AGENTS` 一张表。未知 id 原样保留，
    /// 降级运行时不会把别的版本写的配置吃掉。
    #[serde(default)]
    pub acp_env: BTreeMap<String, BTreeMap<String, String>>,
    /// 每个 ACP agent 最近一次由用户显式选择的模型、推理档位等配置。
    ///
    /// key 只用稳定的 agent id，不带项目或 profile：同一家 Grok/OpenCode/Codex
    /// 在所有项目与 workspace 间共享最近选择；不同 agent 的私有 value id 不串用。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub acp_config_memory: BTreeMap<String, Vec<(String, String)>>,
    /// Profiles discovered from native `$DSH_HOME/profiles`. They are runtime
    /// facts, not Smelt settings, so they never enter the agent UI snapshot.
    #[serde(skip)]
    native_dsh_profiles: Vec<AcpProfile>,
    /// 手动添加的 workspace（同一家 agent 可以有好几个，比如 Claude 的默认
    /// `.claude` 和自定义的 `.claude-quant` 并存）。基础 agent 槽位不变、
    /// 走各自默认路径；这里只装"额外"的。
    #[serde(default)]
    pub profiles: Vec<AcpProfile>,
    /// 用户定义的本地智能体。旧版本曾将它们作为会话「预设」保存；alias 只负责
    /// 无损读取旧配置，新写入统一使用产品模型里的 `agents`。
    #[serde(
        default,
        alias = "agent_presets",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub agents: Vec<AgentDefinition>,
    /// daemon 自动化投影；GUI 不据此调度，也不写回智能体配置快照。
    #[serde(skip)]
    pub automations: Vec<Automation>,
    #[serde(skip)]
    pub automation_store_id: String,
    #[serde(skip)]
    pub retired_automation_store_ids: HashSet<String>,
    #[serde(skip)]
    pub automation_revision: u64,
    #[serde(skip)]
    pub automation_states: Vec<AutomationState>,
    #[serde(skip)]
    pub automation_runs: Vec<AutomationRun>,
    #[serde(skip)]
    pub automation_store_error: Option<String>,
    #[serde(skip)]
    pub webhook_base_url: Option<String>,
    #[serde(skip)]
    pub persistence_error: Option<String>,
    /// 侧栏「新对话」默认用哪个智能体。
    #[serde(skip)]
    pub default_conversation_agent: Option<String>,
    /// 运行时那部分（命令、env、profile、定义、hook 开关）是否已从运行时加载。
    /// 没加载前是出厂值，不能拿去启动会话，也不接受修改。
    #[serde(skip)]
    pub runtime_loaded: bool,
}

impl AgentHostState {
    /// 某个 agent 种类当前生效的启动命令。
    pub fn acp_cmd_for(&self, agent: ConversationAgentKind) -> String {
        self.acp_cmds.get(agent)
    }

    /// 改某个 agent 的启动命令（设置页各条输入框共用）。
    pub fn set_acp_cmd_for(&mut self, agent: ConversationAgentKind, cmd: String) {
        self.acp_cmds.set(agent, cmd);
    }

    /// `set_acp_cmd_for` 的链式版本，便于在构造表达式里覆盖单家命令
    /// （命令不再是具名字段，`..Default::default()` 那种写法覆盖不到）。
    #[must_use]
    pub fn with_acp_cmd(mut self, agent: ConversationAgentKind, cmd: impl Into<String>) -> Self {
        self.set_acp_cmd_for(agent, cmd.into());
        self
    }

    pub fn find_profile(&self, id: &str) -> Option<&AcpProfile> {
        self.profiles
            .iter()
            .chain(&self.native_dsh_profiles)
            .find(|p| p.id == id)
    }

    pub fn all_profiles(&self) -> impl Iterator<Item = &AcpProfile> {
        self.profiles.iter().chain(&self.native_dsh_profiles)
    }

    /// 某个 agent 当前生效的额外环境变量。
    pub fn acp_env_for(&self, agent: ConversationAgentKind) -> BTreeMap<String, String> {
        self.acp_env.get(agent.id()).cloned().unwrap_or_default()
    }

    /// 改某个 agent 的额外环境变量；清空即删键，别在配置里留一堆空对象。
    pub fn set_acp_env_for(&mut self, agent: ConversationAgentKind, env: BTreeMap<String, String>) {
        if env.is_empty() {
            self.acp_env.remove(agent.id());
        } else {
            self.acp_env.insert(agent.id().to_string(), env);
        }
    }

    /// 某个 agent 最近由用户选中的配置。返回去重后的副本，避免手改配置文件产生
    /// 同一 config id 多个值，恢复时连续发送互相覆盖的请求。
    pub fn remembered_acp_config(&self, agent: ConversationAgentKind) -> Vec<(String, String)> {
        let mut remembered = Vec::new();
        for (config_id, value_id) in self.acp_config_memory.get(agent.id()).into_iter().flatten() {
            upsert_config_value(&mut remembered, config_id.clone(), value_id.clone());
        }
        remembered
    }

    /// 记录一次用户显式选择。同一配置只保留最后一个值；profile/cwd 不参与 key。
    pub fn remember_acp_config_value(
        &mut self,
        agent: ConversationAgentKind,
        config_id: String,
        value_id: String,
    ) {
        if config_id.trim().is_empty() || value_id.trim().is_empty() {
            return;
        }
        let values = self
            .acp_config_memory
            .entry(agent.id().to_string())
            .or_default();
        upsert_config_value(values, config_id, value_id);
    }

    /// 新会话的初始配置：先铺该 agent 的最近选择，再由会话自身存档逐项覆盖。
    /// 因此冷恢复不会把某条会话原本的模型顶掉，同时能继承它尚未显式设置的档位。
    pub fn initial_acp_config(
        &self,
        agent: ConversationAgentKind,
        session_values: &[(String, String)],
    ) -> Vec<(String, String)> {
        let mut values = self.remembered_acp_config(agent);
        for (config_id, value_id) in session_values {
            upsert_config_value(&mut values, config_id.clone(), value_id.clone());
        }
        values
    }

    /// 产品智能体由用户显式创建并长期托管，新开对话默认可自主执行。模型和推理
    /// 档位仍继承最近选择，但权限不能继承普通对话里可能选过的逐次审批。
    pub fn initial_agent_conversation_config(
        &self,
        agent: ConversationAgentKind,
    ) -> Vec<(String, String)> {
        let mut values = self.initial_acp_config(agent, &[]);
        if let Some(mode) = agent.task_params().full_access_mode {
            upsert_config_value(&mut values, "mode".to_string(), mode.to_string());
        }
        values
    }

    /// 某个 agent 当前生效的完整启动规格：命令 + 用户配的环境变量。
    ///
    /// 唯一接缝。会话侧一律走这里，不要再自己 `from_command(acp_cmd_for(..))`
    /// ——那样拼出来的规格永远少了环境变量，且漏一处就是"设置里填了不生效"。
    pub fn acp_launch_for(&self, agent: ConversationAgentKind) -> ConversationLaunchSpec {
        // DSH 的运行时事实（模型、凭据、插件与 profile）只属于原生 dsh。
        // “适配器 DSH”是主界面未选择额外 workspace 时的默认入口，因此也必须
        // 指向已经安装 Smelt Host 的原生 profile，而不能退回历史私有运行时。
        if !agent.is_bare_kind()
            && let Some(launch) = self
                .native_dsh_profiles
                .first()
                .and_then(|profile| profile.launch_spec().ok())
        {
            return launch;
        }
        let mut launch = agent.default_launch();
        launch.command = self.acp_cmd_for(agent);
        // 用户显式配置优先于出厂覆盖；这样既默认全权限，也保留降权和自定义
        // OPENCODE_CONFIG_CONTENT 的出口。
        for (name, value) in self.acp_env_for(agent) {
            launch.env.insert(name, value);
        }
        launch
    }

    pub fn profile_launch_spec(
        &self,
        profile: &AcpProfile,
    ) -> Result<ConversationLaunchSpec, String> {
        let kind = profile.kind().ok_or_else(|| {
            format!(
                "workspace profile `{}` 使用了未知 ACP Agent `{}`",
                profile.id, profile.kind_id
            )
        })?;
        let mut launch = profile.launch_spec()?;
        let native_dsh = profile.is_native_dsh();
        if !native_dsh {
            launch.command = self.acp_cmd_for(kind);
        }
        // workspace 目录那条环境变量是 profile 的立身之本，不能被用户配的同名
        // 变量顶掉；其余按 kind 继承并允许显式覆盖出厂环境，这样既能默认全权限，
        // 也能在设置里把某个 OpenCode workspace 降权。
        if !native_dsh {
            let workspace_env = profile.env_var()?;
            for (name, value) in self.acp_env_for(kind) {
                if name != workspace_env {
                    launch.env.insert(name, value);
                }
            }
        }
        Ok(launch)
    }

    /// 将智能体定义解析成执行引擎启动规格。工作目录、账号/profile 与模型属于
    /// Conversation/Run 的执行上下文，不能从定义里偷偷带进来。
    pub fn resolve_agent(
        &self,
        definition: &AgentDefinition,
    ) -> Result<(ConversationAgentKind, ConversationLaunchSpec), String> {
        let agent = definition.engine_kind().ok_or_else(|| {
            format!(
                "智能体 `{}` 使用了未知执行引擎 `{}`",
                definition.name, definition.engine_kind_id
            )
        })?;
        Ok((agent, self.acp_launch_for(agent)))
    }

    pub fn automations_for<'a>(&'a self, agent_id: &str) -> Vec<&'a Automation> {
        self.automations
            .iter()
            .filter(|automation| automation.agent_definition_id() == Some(agent_id))
            .collect()
    }

    pub fn automation_state_for(&self, automation_id: &str) -> Option<&AutomationState> {
        self.automation_states
            .iter()
            .find(|state| state.automation_id == automation_id)
    }

    pub fn active_automation_run(&self, automation_id: &str) -> Option<&AutomationRun> {
        self.automation_runs
            .iter()
            .find(|run| run.automation_id == automation_id && run.blocks_overlap())
    }

    pub fn latest_automation_run(&self, automation_id: &str) -> Option<&AutomationRun> {
        self.automation_runs
            .iter()
            .filter(|run| run.automation_id == automation_id)
            .max_by_key(|run| (run.created_at, run.id.as_str()))
    }

    pub fn automation_runs_for(&self, automation_id: &str) -> Vec<&AutomationRun> {
        let mut runs = self
            .automation_runs
            .iter()
            .filter(|run| run.automation_id == automation_id)
            .collect::<Vec<_>>();
        runs.sort_by_key(|run| std::cmp::Reverse((run.created_at, run.id.as_str())));
        runs
    }

    pub fn is_retired_automation_store(&self, store_id: &str) -> bool {
        self.retired_automation_store_ids.contains(store_id)
    }

    pub fn replace_automation_snapshot(&mut self, snapshot: AutomationFile) -> bool {
        if snapshot.store_id == self.automation_store_id {
            if snapshot.revision < self.automation_revision {
                return false;
            }
            if snapshot.revision == self.automation_revision
                && snapshot.automations == self.automations
                && snapshot.states == self.automation_states
                && snapshot.runs == self.automation_runs
                && snapshot.store_error == self.automation_store_error
                && snapshot.webhook_base_url == self.webhook_base_url
            {
                return false;
            }
        } else {
            if self
                .retired_automation_store_ids
                .contains(&snapshot.store_id)
            {
                return false;
            }
            if !self.automation_store_id.is_empty() {
                self.retired_automation_store_ids
                    .insert(self.automation_store_id.clone());
            }
        }
        self.automation_store_id = snapshot.store_id;
        self.automation_revision = snapshot.revision;
        self.automations = snapshot.automations;
        self.automation_states = snapshot.states;
        self.automation_runs = snapshot.runs;
        self.automation_store_error = snapshot.store_error;
        self.webhook_base_url = snapshot.webhook_base_url;
        true
    }
}

impl Default for AgentHostState {
    fn default() -> Self {
        Self {
            agent_hooks_enabled: true,
            cross_agent_enabled: true,
            notify_approval: true,
            notify_input: true,
            notify_success: true,
            notify_failure: true,
            notify_terminal_bell: true,
            acp_cmds: ConversationCommands::with_defaults(),
            acp_env: BTreeMap::new(),
            acp_config_memory: BTreeMap::new(),
            native_dsh_profiles: Vec::new(),
            profiles: Vec::new(),
            agents: Vec::new(),
            automations: Vec::new(),
            automation_store_id: String::new(),
            retired_automation_store_ids: HashSet::new(),
            automation_revision: 0,
            automation_states: Vec::new(),
            automation_runs: Vec::new(),
            automation_store_error: None,
            webhook_base_url: None,
            persistence_error: None,
            default_conversation_agent: None,
            runtime_loaded: false,
        }
    }
}

fn upsert_config_value(values: &mut Vec<(String, String)>, config_id: String, value_id: String) {
    if let Some((_, current)) = values.iter_mut().find(|(id, _)| id == &config_id) {
        *current = value_id;
    } else {
        values.push((config_id, value_id));
    }
}

impl Global for AgentHostState {}

// ===================== 运行时那部分：缓存 + 异步提交 =====================

impl AgentHostState {
    /// 交给运行时的那部分配置。通知开关、自动化投影、错误状态不在里面。
    pub fn runtime_settings(&self) -> AgentHostSettings {
        AgentHostSettings {
            hooks_enabled: self.agent_hooks_enabled,
            cross_agent_enabled: self.cross_agent_enabled,
            commands: self
                .acp_cmds
                .0
                .iter()
                .map(|(id, cmd)| ((*id).to_string(), cmd.clone()))
                .collect(),
            env: self.acp_env.clone(),
            config_memory: self.acp_config_memory.clone(),
            profiles: self.profiles.clone(),
            native_dsh_profiles: self.native_dsh_profiles.clone(),
            agents: self.agents.clone(),
            default_conversation_agent: self.default_conversation_agent.clone(),
        }
    }

    /// 用运行时给的权威配置替换缓存里运行时那部分，并标记为已加载。
    pub fn apply_runtime_settings(&mut self, settings: AgentHostSettings) {
        self.agent_hooks_enabled = settings.hooks_enabled;
        self.cross_agent_enabled = settings.cross_agent_enabled;
        self.acp_cmds = ConversationCommands(
            CONVERSATION_AGENTS
                .iter()
                .filter_map(|d| settings.commands.get(d.id).map(|cmd| (d.id, cmd.clone())))
                .collect(),
        );
        self.acp_env = settings.env;
        self.acp_config_memory = settings.config_memory;
        self.profiles = settings.profiles;
        self.native_dsh_profiles = settings.native_dsh_profiles;
        self.agents = settings.agents;
        self.default_conversation_agent = settings.default_conversation_agent;
        self.runtime_loaded = true;
    }

    fn notify_prefs(&self) -> NotifyPrefs {
        NotifyPrefs {
            approval: self.notify_approval,
            input: self.notify_input,
            success: self.notify_success,
            failure: self.notify_failure,
            terminal_bell: self.notify_terminal_bell,
        }
    }

    fn set_notify_prefs(&mut self, prefs: NotifyPrefs) {
        self.notify_approval = prefs.approval;
        self.notify_input = prefs.input;
        self.notify_success = prefs.success;
        self.notify_failure = prefs.failure;
        self.notify_terminal_bell = prefs.terminal_bell;
    }
}

/// 通知怎么提醒是客户端自己的事（设计文档 D1），每台设备可以不同，不交给运行时。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct NotifyPrefs {
    approval: bool,
    input: bool,
    success: bool,
    failure: bool,
    terminal_bell: bool,
}

/// 读客户端库里的通知偏好；没有时用出厂值。
fn load_notify_prefs() -> NotifyPrefs {
    let (scope, key) = smelt_client_store::keys::NOTIFY_PREFS;
    smelt_client_store::client_store()
        .and_then(|store| store.get::<NotifyPrefs>(scope, key))
        .ok()
        .flatten()
        .unwrap_or_else(|| AgentHostState::default().notify_prefs())
}

fn save_notify_prefs(prefs: NotifyPrefs) -> Result<(), String> {
    let (scope, key) = smelt_client_store::keys::NOTIFY_PREFS;
    smelt_client_store::client_store()
        .and_then(|store| store.put(scope, key, &prefs))
        .map_err(|error| error.to_string())
}

/// 启动时的初始状态：客户端的通知偏好立刻可用；运行时那部分还没加载，
/// 不接受修改，见 [`start_agent_host_load`]。
pub fn initial_agent_host_state() -> AgentHostState {
    let mut state = AgentHostState::default();
    state.set_notify_prefs(load_notify_prefs());
    state
}

fn fetch_agent_host() -> Result<AgentHostSettings, String> {
    smelt_client::daemon_state::local_runtime()
        .call::<AgentHostGet>(&EmptyParams::default(), RUNTIME_TIMEOUT)
        .map_err(|error| error.to_string())
}

/// 运行时那部分配置加载好了没有。没好之前，依赖它的启动和恢复要等。
pub fn agent_host_loaded(cx: &App) -> bool {
    cx.try_global::<AgentHostState>()
        .is_some_and(|state| state.runtime_loaded)
}

/// 后台从运行时加载配置；运行时还没起来就按退避重试，直到拿到为止。
/// `on_first_load` 在第一次加载成功后调用一次。
pub fn start_agent_host_load(cx: &mut App, on_first_load: impl FnOnce(&mut App) + 'static) {
    cx.spawn(async move |cx| {
        let mut attempt = 0u32;
        loop {
            let fetched = cx
                .background_executor()
                .spawn(async { fetch_agent_host() })
                .await;
            match fetched {
                Ok(settings) => {
                    cx.update(|cx| {
                        let mut state = cx.global::<AgentHostState>().clone();
                        state.apply_runtime_settings(settings);
                        state.persistence_error = None;
                        cx.set_global(state);
                        on_first_load(cx);
                    });
                    return;
                }
                Err(error) => {
                    // 运行时刚被拉起、正在升级时连不上是常态，先安静重试几次。
                    if attempt >= 4 {
                        cx.update(|cx| {
                            let mut state = cx.global::<AgentHostState>().clone();
                            state.persistence_error =
                                Some(format!("读取运行时的智能体配置失败，正在重试：{error}"));
                            cx.set_global(state);
                        });
                    }
                }
            }
            cx.background_executor()
                .timer(smelt_client::daemon_state::daemon_reconnect_backoff(attempt))
                .await;
            attempt = attempt.saturating_add(1);
        }
    })
    .detach();
}

/// 修改按提交顺序逐个发给运行时。并发发出去，运行时处理的先后和回包的先后都可能
/// 乱，界面就会停在一个旧版本上。
struct AgentHostEditQueue {
    tx: smol::channel::Sender<AgentHostEdit>,
}

impl Global for AgentHostEditQueue {}

fn submit_agent_host_edit(edit: AgentHostEdit, cx: &mut App) {
    if !cx.has_global::<AgentHostEditQueue>() {
        let (tx, rx) = smol::channel::unbounded::<AgentHostEdit>();
        cx.set_global(AgentHostEditQueue { tx });
        cx.spawn(async move |cx| {
            while let Ok(edit) = rx.recv().await {
                let applied = cx
                    .background_executor()
                    .spawn(async move {
                        smelt_client::daemon_state::local_runtime()
                            .call::<AgentHostApplyEdit>(&edit, RUNTIME_TIMEOUT)
                            .map_err(|error| error.to_string())
                    })
                    .await;
                // 后面还有排着的修改时，这一包不是最终状态，先不覆盖本地。
                let drained = rx.is_empty();
                let settled = match applied {
                    Ok(settings) => Ok(settings),
                    Err(error) if drained => {
                        // 失败后以运行时为准，把本地乐观改动退回去。
                        let fresh = cx
                            .background_executor()
                            .spawn(async { fetch_agent_host() })
                            .await;
                        Err((error, fresh.ok()))
                    }
                    Err(error) => Err((error, None)),
                };
                cx.update(|cx| {
                    let mut state = cx.global::<AgentHostState>().clone();
                    match settled {
                        Ok(settings) => {
                            if drained {
                                state.apply_runtime_settings(settings);
                            }
                        }
                        Err((error, fresh)) => {
                            eprintln!("[agent-ui] 保存配置失败: {error}");
                            if let Some(settings) = fresh {
                                state.apply_runtime_settings(settings);
                            }
                            state.persistence_error = Some(format!("保存智能体配置失败：{error}"));
                        }
                    }
                    cx.set_global(state);
                });
            }
        })
        .detach();
    }
    let _ = cx.global::<AgentHostEditQueue>().tx.try_send(edit);
}

/// 改一次配置。本地立刻生效；运行时那部分按「这次改了什么」异步提交，以运行时
/// 合并后的结果为准。返回 false 表示这次没有接受：运行时配置还没加载，或客户端
/// 偏好写不进去。
pub fn try_apply_agent_host(f: impl FnOnce(&mut AgentHostState), cx: &mut App) -> bool {
    let current = cx.global::<AgentHostState>().clone();
    if !current.runtime_loaded {
        let mut current = current;
        current.persistence_error = Some("运行时的智能体配置还没读到，稍后再改".to_string());
        cx.set_global(current);
        return false;
    }
    let mut candidate = current.clone();
    f(&mut candidate);
    candidate.persistence_error = None;
    if candidate.notify_prefs() != current.notify_prefs()
        && let Err(error) = save_notify_prefs(candidate.notify_prefs())
    {
        eprintln!("[agent-ui] 保存通知偏好失败: {error}");
        let mut current = current;
        current.persistence_error = Some(format!("保存通知偏好失败：{error}"));
        cx.set_global(current);
        return false;
    }
    let before = current.runtime_settings();
    let after = candidate.runtime_settings();
    cx.set_global(candidate);
    if before != after {
        submit_agent_host_edit(AgentHostEdit { before, after }, cx);
    }
    true
}

/// 从运行时重新取一份。CLI / Control API 改过定义后，打开智能体面时对齐。
pub fn reload_agent_definitions(cx: &mut App) {
    if !agent_host_loaded(cx) {
        return;
    }
    cx.spawn(async move |cx| {
        let fetched = cx
            .background_executor()
            .spawn(async { fetch_agent_host() })
            .await;
        cx.update(|cx| {
            let mut state = cx.global::<AgentHostState>().clone();
            match fetched {
                Ok(settings) => state.apply_runtime_settings(settings),
                Err(error) => {
                    state.persistence_error = Some(format!("读取智能体配置失败：{error}"))
                }
            }
            cx.set_global(state);
        });
    })
    .detach();
}

pub fn apply_agent_host(f: impl FnOnce(&mut AgentHostState), cx: &mut App) {
    let _ = try_apply_agent_host(f, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_definitions_round_trip_and_legacy_configs_migrate() {
        let legacy: AgentHostState = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(legacy.agents.is_empty());

        let mut config = AgentHostState::default();
        config.agents.push(AgentDefinition {
            id: "reviewer".into(),
            name: "代码审查".into(),
            description: "审查变更".into(),
            engine_kind_id: ConversationAgentKind::Codex.id().into(),
            prompt: "先检查正确性和回归风险".into(),
            plugins: Vec::new(),
            context_folders: Vec::new(),
            context_links: Vec::new(),
            ..Default::default()
        });

        let serialized = serde_json::to_value(&config).unwrap();
        assert_eq!(serialized["agents"][0]["engine_kind"], "codex");
        assert!(serialized["agents"][0].get("agent_id").is_none());

        let restored: AgentHostState = serde_json::from_value(serialized).unwrap();
        assert_eq!(restored.agents[0].name, "代码审查");
        assert_eq!(restored.agents[0].description, "审查变更");
        assert_eq!(
            restored.resolve_agent(&restored.agents[0]).unwrap().0,
            ConversationAgentKind::Codex
        );

        let legacy: AgentHostState = serde_json::from_value(serde_json::json!({
            "agent_presets": [{
                "id": "legacy",
                "name": "旧智能体",
                "agent_id": "pi",
                "profile_id": null,
                "prompt": "保留旧指令"
            }]
        }))
        .unwrap();
        assert_eq!(legacy.agents[0].id, "legacy");
        assert_eq!(legacy.agents[0].prompt, "保留旧指令");
        assert!(legacy.agents[0].description.is_empty());
        let serialized = serde_json::to_value(legacy).unwrap();
        assert!(serialized.get("agents").is_some());
        assert!(serialized.get("agent_presets").is_none());
    }

    #[test]
    fn agent_definition_does_not_persist_run_execution_context() {
        let legacy: AgentHostState = serde_json::from_value(serde_json::json!({
            "agents": [{
                "id": "quant",
                "name": "量化智能体",
                "agent_id": "pi",
                "profile_id": "old-account",
                "working_directory": "/old/project",
                "prompt": "分析行情"
            }]
        }))
        .unwrap();

        let serialized = serde_json::to_value(legacy).unwrap();
        let definition = &serialized["agents"][0];
        assert!(definition.get("profile_id").is_none());
        assert!(definition.get("working_directory").is_none());
        assert!(serialized.get("automations").is_none());
    }

    #[test]
    fn legacy_automations_are_ignored() {
        let config: AgentHostState = serde_json::from_value(serde_json::json!({
            "automations": [{
                "id": "open-scan",
                "agent_id": "quant",
                "name": "开盘扫描",
                "enabled": true,
                "prompt": "拉取行情并输出",
                "cwd": "/tmp/quant",
                "permission_mode": "ask",
                "kind": "schedule",
                "schedule": {"type": "weekdays", "hour": 9, "minute": 35}
            }]
        }))
        .unwrap();

        assert!(config.automations.is_empty());
        assert!(
            serde_json::to_value(config)
                .unwrap()
                .get("automations")
                .is_none()
        );
    }

    #[test]
    fn automation_snapshot_rejects_stale_revisions_and_retired_stores() {
        let mut config = AgentHostState::default();
        let mut store_a = AutomationFile {
            store_id: "store-a".into(),
            revision: 10,
            ..Default::default()
        };
        assert!(config.replace_automation_snapshot(store_a.clone()));
        assert!(
            !config.replace_automation_snapshot(store_a.clone()),
            "同一 revision 的相同投影不应再刷新"
        );
        store_a.webhook_base_url = Some("http://127.0.0.1:17827".into());
        assert!(
            config.replace_automation_snapshot(store_a.clone()),
            "同 revision 补上 webhook 根地址仍要应用"
        );

        store_a.revision = 9;
        assert!(!config.replace_automation_snapshot(store_a.clone()));
        assert_eq!(config.automation_revision, 10);

        let store_b = AutomationFile {
            store_id: "store-b".into(),
            revision: 1,
            ..Default::default()
        };
        assert!(config.replace_automation_snapshot(store_b));
        assert_eq!(config.automation_store_id, "store-b");
        assert_eq!(config.automation_revision, 1);

        store_a.revision = 11;
        assert!(!config.replace_automation_snapshot(store_a));
        assert_eq!(config.automation_store_id, "store-b");
    }

    /// 设置页填的环境变量必须真的跟着会话走。之前会话侧是自己
    /// `from_command(acp_cmd_for(..))` 拼的规格，环境变量在那条路上会被整段丢掉，
    /// 而症状要等到真发一轮才暴露成"key 没配"。
    #[test]
    fn launch_spec_carries_the_env_from_settings() {
        let mut config = AgentHostState::default();
        config.set_acp_env_for(
            ConversationAgentKind::Dsh,
            BTreeMap::from([("DEEPSEEK_API_KEY".into(), "sk-abc".into())]),
        );

        let launch = config.acp_launch_for(ConversationAgentKind::Dsh);
        assert_eq!(
            launch.command,
            config.acp_cmd_for(ConversationAgentKind::Dsh)
        );
        assert_eq!(
            launch.env.get("DEEPSEEK_API_KEY").map(String::as_str),
            Some("sk-abc")
        );

        // 没配的那家不能被别家的变量污染。
        assert!(
            config
                .acp_launch_for(ConversationAgentKind::Claude)
                .env
                .is_empty()
        );
    }

    #[test]
    fn opencode_acp_defaults_to_process_local_full_permissions() {
        let launch = AgentHostState::default().acp_launch_for(ConversationAgentKind::OpenCode);

        assert_eq!(launch.command, "opencode acp");
        assert_eq!(
            launch
                .env
                .get("OPENCODE_CONFIG_CONTENT")
                .map(String::as_str),
            Some(r#"{"permission":"allow"}"#)
        );
    }

    #[test]
    fn remembered_acp_config_is_keyed_by_agent_not_profile_or_project() {
        let mut config = AgentHostState::default();
        config.remember_acp_config_value(
            ConversationAgentKind::Grok,
            "model".into(),
            "grok-4".into(),
        );
        config.remember_acp_config_value(
            ConversationAgentKind::OpenCode,
            "model".into(),
            "deepseek-v4".into(),
        );
        config.remember_acp_config_value(
            ConversationAgentKind::Codex,
            "reasoning_effort".into(),
            "high".into(),
        );

        assert_eq!(
            config.remembered_acp_config(ConversationAgentKind::Grok),
            vec![("model".into(), "grok-4".into())]
        );
        assert_eq!(
            config.remembered_acp_config(ConversationAgentKind::OpenCode),
            vec![("model".into(), "deepseek-v4".into())]
        );
        assert_eq!(
            config.remembered_acp_config(ConversationAgentKind::Codex),
            vec![("reasoning_effort".into(), "high".into())]
        );
        assert!(
            config
                .remembered_acp_config(ConversationAgentKind::Claude)
                .is_empty()
        );

        let round_trip: AgentHostState =
            serde_json::from_value(serde_json::to_value(config).unwrap()).unwrap();
        assert_eq!(
            round_trip.remembered_acp_config(ConversationAgentKind::OpenCode),
            vec![("model".into(), "deepseek-v4".into())]
        );
    }

    #[test]
    fn session_config_overrides_agent_memory_and_inherits_missing_values() {
        let mut config = AgentHostState::default();
        config.remember_acp_config_value(
            ConversationAgentKind::Codex,
            "model".into(),
            "gpt-5.6".into(),
        );
        config.remember_acp_config_value(
            ConversationAgentKind::Codex,
            "reasoning_effort".into(),
            "medium".into(),
        );

        assert_eq!(
            config.initial_acp_config(ConversationAgentKind::Codex, &[]),
            vec![
                ("model".into(), "gpt-5.6".into()),
                ("reasoning_effort".into(), "medium".into()),
            ]
        );
        assert_eq!(
            config.initial_acp_config(
                ConversationAgentKind::Codex,
                &[("reasoning_effort".into(), "high".into())],
            ),
            vec![
                ("model".into(), "gpt-5.6".into()),
                ("reasoning_effort".into(), "high".into()),
            ]
        );
    }

    #[test]
    fn product_agent_conversation_forces_full_access_without_losing_recent_config() {
        let mut config = AgentHostState::default();
        config.remember_acp_config_value(
            ConversationAgentKind::Pi,
            "model".into(),
            "openai/gpt-5".into(),
        );
        config.remember_acp_config_value(
            ConversationAgentKind::Pi,
            "mode".into(),
            "default".into(),
        );
        config.remember_acp_config_value(
            ConversationAgentKind::Pi,
            "thought_level".into(),
            "high".into(),
        );

        assert!(
            config
                .initial_acp_config(ConversationAgentKind::Pi, &[])
                .contains(&("mode".into(), "default".into()))
        );
        assert_eq!(
            config.initial_agent_conversation_config(ConversationAgentKind::Pi),
            vec![
                ("model".into(), "openai/gpt-5".into()),
                ("mode".into(), "bypassPermissions".into()),
                ("thought_level".into(), "high".into()),
            ]
        );
    }

    /// 清空即删键：配置里不该留一堆空对象，否则每次存盘都在长。
    #[test]
    fn clearing_the_env_drops_the_key_instead_of_storing_an_empty_map() {
        let mut config = AgentHostState::default();
        config.set_acp_env_for(
            ConversationAgentKind::Dsh,
            BTreeMap::from([("K".to_string(), "v".to_string())]),
        );
        config.set_acp_env_for(ConversationAgentKind::Dsh, BTreeMap::new());

        assert!(config.acp_env.is_empty());
        assert_eq!(
            serde_json::to_value(&config).unwrap()["acp_env"],
            serde_json::json!({})
        );
    }

    /// workspace profile 继承 kind 的环境变量（给 dsh 配一次 key，它的每个
    /// workspace 都能用），但 profile 自己那条数据目录变量不能被顶掉。
    #[test]
    fn profile_inherits_kind_env_without_losing_its_workspace_dir() {
        let profile = AcpProfile {
            native_launch: None,
            id: "p1".into(),
            kind_id: ConversationAgentKind::Claude.id().into(),
            label: "Claude Quant".into(),
            workspace_dir: "/tmp/quant".into(),
        };
        let dir_var = profile
            .env_var()
            .expect("Claude 支持 workspace")
            .to_string();
        let mut config = AgentHostState::default();
        config.set_acp_env_for(
            ConversationAgentKind::Claude,
            BTreeMap::from([
                ("ANTHROPIC_BASE_URL".to_string(), "https://gw".to_string()),
                (dir_var.clone(), "/tmp/hijacked".to_string()),
            ]),
        );

        let launch = config.profile_launch_spec(&profile).expect("有效 profile");
        assert_eq!(
            launch.env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://gw")
        );
        assert_eq!(
            launch.env.get(&dir_var).map(String::as_str),
            Some("/tmp/quant"),
            "profile 的数据目录是它的立身之本，不能被同名变量顶掉"
        );
    }

    #[test]
    fn native_dsh_profile_keeps_credentials_owned_by_dsh() {
        let profile = AcpProfile {
            native_launch: Some(smelt_core::agent_kind::dsh_profile_launch_spec("team")),
            id: "dsh-native-team".into(),
            kind_id: ConversationAgentKind::Dsh.id().into(),
            label: "DeepSeek Harness · team".into(),
            workspace_dir: "team".into(),
        };
        let mut config = AgentHostState::default();
        config.set_acp_env_for(
            ConversationAgentKind::Dsh,
            BTreeMap::from([("DEEPSEEK_API_KEY".into(), "smelt-only".into())]),
        );

        let launch = config
            .profile_launch_spec(&profile)
            .expect("原生 DSH profile");
        assert!(launch.env.is_empty());
        assert!(launch.command.contains("--profile team"));
    }

    #[test]
    fn default_dsh_launch_prefers_the_discovered_native_profile() {
        let native = AcpProfile {
            native_launch: Some(smelt_core::agent_kind::dsh_profile_launch_spec("web")),
            id: "dsh-native-web".into(),
            kind_id: ConversationAgentKind::Dsh.id().into(),
            label: "DeepSeek Harness · web".into(),
            workspace_dir: "web".into(),
        };
        let mut config = AgentHostState {
            native_dsh_profiles: vec![native],
            ..Default::default()
        };
        config.set_acp_cmd_for(ConversationAgentKind::Dsh, "legacy-private-dsh".into());
        config.set_acp_env_for(
            ConversationAgentKind::Dsh,
            BTreeMap::from([("DEEPSEEK_API_KEY".into(), "legacy-key".into())]),
        );

        let launch = config.acp_launch_for(ConversationAgentKind::Dsh);

        assert!(launch.command.contains("--profile web"));
        assert!(launch.env.is_empty());
    }

    /// 存档契约：旧 agent_ui.json 里那几个平铺的命令键必须继续被认，
    /// 且写回时键名不变——否则升级会把用户自定义命令重置回默认。
    #[test]
    fn legacy_flat_command_keys_round_trip() {
        let legacy = serde_json::json!({
            "acp_cmd": "claude-custom --acp",
            "acp_copilot_cmd": "copilot-custom --acp",
            "acp_codex_cmd": "codex-custom --acp",
            "acp_grok_cmd": "grok-custom stdio",
            "notify_success": false,
        });

        let config: AgentHostState = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            config.acp_cmd_for(ConversationAgentKind::Claude),
            "claude-custom --acp"
        );
        assert_eq!(
            config.acp_cmd_for(ConversationAgentKind::Copilot),
            "copilot-custom --acp"
        );
        assert_eq!(
            config.acp_cmd_for(ConversationAgentKind::Codex),
            "codex-custom --acp"
        );
        assert_eq!(
            config.acp_cmd_for(ConversationAgentKind::Grok),
            "grok-custom stdio"
        );
        assert!(!config.notify_success);

        let saved = serde_json::to_value(&config).unwrap();
        assert_eq!(saved["acp_cmd"], "claude-custom --acp");
        assert_eq!(saved["acp_grok_cmd"], "grok-custom stdio");
    }

    /// 缺键 = 用出厂默认（原先靠 `#[serde(default = ...)]`，现在靠查表兜底）。
    #[test]
    fn missing_command_key_falls_back_to_descriptor_default() {
        let config: AgentHostState = serde_json::from_value(serde_json::json!({})).unwrap();
        for kind in ConversationAgentKind::ALL {
            assert_eq!(config.acp_cmd_for(kind), kind.default_cmd());
        }
    }

    /// 新装用户落盘的键集合与重构前一致：每家一条，不多不少。
    #[test]
    fn default_config_writes_one_key_per_registered_agent() {
        let saved = serde_json::to_value(AgentHostState::default()).unwrap();
        for descriptor in CONVERSATION_AGENTS {
            assert_eq!(
                saved[descriptor.config_key],
                serde_json::json!((descriptor.default_cmd)()),
                "{} 落盘缺失或不匹配",
                descriptor.config_key
            );
        }
    }

    #[test]
    fn cross_agent_messaging_defaults_to_enabled_for_legacy_config() {
        let legacy: AgentHostState = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(legacy.cross_agent_enabled);

        let disabled = AgentHostState {
            cross_agent_enabled: false,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(disabled).unwrap()["cross_agent_enabled"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn profile_launch_uses_configured_agent_command_and_workspace_env() {
        let mut config = AgentHostState::default();
        config.set_acp_cmd_for(ConversationAgentKind::Claude, "claude-custom --acp".into());
        let profile = AcpProfile {
            native_launch: None,
            id: "quant".into(),
            kind_id: "claude".into(),
            label: "Quant".into(),
            workspace_dir: "~/Claude Workspaces/quant".into(),
        };

        let launch = config.profile_launch_spec(&profile).expect("有效 profile");

        assert_eq!(launch.command, "claude-custom --acp");
        assert_eq!(
            launch.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("~/Claude Workspaces/quant")
        );
    }

    #[test]
    fn unknown_profile_cannot_resolve_a_launch_spec() {
        let profile = AcpProfile {
            native_launch: None,
            id: "future-profile".into(),
            kind_id: "future-agent".into(),
            label: "Future Agent".into(),
            workspace_dir: "~/.future-agent".into(),
        };

        let error = AgentHostState::default()
            .profile_launch_spec(&profile)
            .expect_err("未知 Agent 不能借用默认启动规格");
        assert!(error.contains("future-agent"));
    }
}
