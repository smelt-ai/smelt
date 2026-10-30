//! 智能体的新建、编辑、删除和记忆审阅。
//!
//! 定义和记忆都在运行时那台机器上，没有推送：页面打开时取一次，自己改完用返回值刷新。
//! 编辑只把改了的字段发给运行时，桌面同时在改别的字段时不会被盖掉。

use crate::api::catalog::AgentDefinitionSummary;
use crate::api::error::SmeltError;

/// 编辑页上的一份智能体配置。新建、保存都用它；执行引擎眼下只有 Pi，不在这里选。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentDraft {
    pub name: String,
    /// 长期工作方式。
    pub prompt: String,
    /// 要加载的插件 id（`skill:<名字>` / `extension:<名字>`）。空 = 不加载任何插件。
    pub plugins: Vec<String>,
    /// 要连的 MCP 服务器名。空 = 一个都不连。
    pub mcp_servers: Vec<String>,
    /// 绑定的目录（运行时那台机器上的绝对路径）。
    pub context_folders: Vec<String>,
    pub context_links: Vec<String>,
    /// 和 `model_id` 成对；都空 = 跟随这台机器的默认模型。
    pub model_provider: String,
    pub model_id: String,
    /// 关掉后不注入、不写入记忆，已有的留着。
    pub memory_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub provider: String,
    pub id: String,
    /// 「provider / 模型名」。
    pub label: String,
}

/// 一家 provider 下能选的模型。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelGroup {
    pub provider: String,
    pub provider_name: String,
    pub models: Vec<ModelOption>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginKind {
    Skill,
    Extension,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOption {
    pub id: String,
    pub kind: PluginKind,
    pub name: String,
    pub description: String,
    /// 坏了的原因（软链断了之类）。坏的插件不能新勾上。
    pub broken: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpOption {
    pub name: String,
    pub description: Option<String>,
    /// 在能力库里停用了：勾上也不会连。
    pub enabled: bool,
}

/// 这台机器能力库里能选的模型、插件、MCP。
///
/// 每一项为 `None` 是**没读到**，不是「这台机器上没有」：界面这时照原值显示智能体选了的
/// 东西，不能把它们标成「已经没有了」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentChoices {
    pub models: Option<Vec<ModelGroup>>,
    pub plugins: Option<Vec<PluginOption>>,
    pub mcp_servers: Option<Vec<McpOption>>,
    /// 没读到（或读到一部分）的原因，几条用「；」连起来。
    pub error: Option<String>,
}

/// 编辑页要的全部：当前配置（新建时是空白草稿），和这台机器上能选的东西。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEditor {
    pub draft: AgentDraft,
    pub choices: AgentChoices,
}

/// 只取能力库里的选项（详情页把 id 显示成名字用）。连不上才报错；能力库读不到时照样
/// 返回，原因在 `error` 里。
pub fn agent_choices(runtime_id: String) -> Result<AgentChoices, SmeltError> {
    crate::fleet::fleet().node(&runtime_id)?.agents.choices()
}

/// 打开编辑页。`agent_id` 为空是新建。
pub fn agent_editor(
    runtime_id: String,
    agent_id: Option<String>,
) -> Result<AgentEditor, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .agents
        .editor(agent_id.as_deref())
}

pub fn create_agent(
    runtime_id: String,
    draft: AgentDraft,
) -> Result<AgentDefinitionSummary, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .agents
        .create(draft)
}

/// 保存编辑。`original` 是打开编辑页时的那份，只有和它不同的字段才发给运行时。
pub fn update_agent(
    runtime_id: String,
    agent_id: String,
    original: AgentDraft,
    edited: AgentDraft,
) -> Result<AgentDefinitionSummary, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .agents
        .update(&agent_id, &original, &edited)
}

/// 删除智能体，连同它自己的记忆。还有自动化在用它时运行时会拒绝，并说明有几条。
pub fn delete_agent(runtime_id: String, agent_id: String) -> Result<(), SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .agents
        .delete(&agent_id)
}

// ---------------------------------------------------------------------------
// 记忆
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryLayer {
    /// 这个智能体自己的。
    Agent,
    /// 所有智能体共用的（用户全局）。
    User,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryItem {
    pub file_name: String,
    pub name: String,
    pub description: String,
    /// 规则类（用户本人的情况、纠正过的做法）：每轮都带上。否则是用到时才召回的事实。
    pub rule: bool,
    /// 待确认的原因。待确认的不注入、不召回，采纳后才生效。
    pub pending_reason: Option<String>,
    /// 事实类且很久没用过。
    pub stale: bool,
    pub read_count: u64,
    /// 最近一次被读或被改（unix 毫秒）。
    pub last_used_ms: Option<i64>,
    /// 正文（去掉 frontmatter）。
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryShelf {
    /// 待确认的在前，其余按最近用过排，没用过的垫底。
    pub items: Vec<MemoryItem>,
    /// 已生效的规则超出常驻预算，启动时只会注入一部分。
    pub core_over_budget: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMemories {
    pub agent: MemoryShelf,
    pub user: MemoryShelf,
    /// 两层加起来待确认的条数。
    pub pending: u32,
}

pub fn agent_memories(runtime_id: String, agent_id: String) -> Result<AgentMemories, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .agents
        .memories(&agent_id)
}

/// 采纳一条待确认的记忆，返回刷新后的两层。
pub fn approve_memory(
    runtime_id: String,
    agent_id: String,
    layer: MemoryLayer,
    file_name: String,
) -> Result<AgentMemories, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .agents
        .approve_memory(&agent_id, layer, &file_name)
}

/// 删掉一条记忆，返回刷新后的两层。
pub fn delete_memory(
    runtime_id: String,
    agent_id: String,
    layer: MemoryLayer,
    file_name: String,
) -> Result<AgentMemories, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .agents
        .delete_memory(&agent_id, layer, &file_name)
}
