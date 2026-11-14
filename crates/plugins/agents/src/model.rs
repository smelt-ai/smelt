//! 页面用的数据和纯函数：智能体的引用、机器、默认对话智能体、记忆的筛选与文案。

use super::*;

/// 一个智能体：在哪台机器上、id 是什么。每台机器有自己的智能体配置，两台上同 id 的是
/// 两回事。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AgentRef {
    pub(crate) runtime: RuntimeKey,
    pub(crate) id: String,
}

impl AgentRef {
    pub(crate) fn new(runtime: RuntimeKey, id: String) -> Self {
        Self { runtime, id }
    }

    /// 它现在的定义；机器没挂、还没读到或已经删了是 `None`。
    pub(crate) fn definition(&self, cx: &App) -> Option<AgentDefinition> {
        api::host(cx)
            .agent_settings(&self.runtime, cx)?
            .agents
            .iter()
            .find(|agent| agent.id == self.id)
            .cloned()
    }
}

/// 编辑器读到的那台机器的 Pi 配置。
#[derive(Default)]
pub(crate) struct AgentPiSnapshot {
    pub(crate) config: Option<api::model::pi_service::PiConfig>,
    /// 最近一次读取失败的原因。
    pub(crate) error: Option<String>,
    /// 读取的代数：晚回来的旧读取不能盖掉更新的那份。
    pub(crate) generation: u64,
}

/// 记忆卡片的两层数据：智能体自己的，和所有智能体共享的用户全局层。由运行时列出。
pub(crate) use api::model::agent_memory::AgentMemories;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryLayer {
    Agent,
    User,
}

impl MemoryLayer {
    pub(crate) fn wire(self) -> api::model::control_api::AgentMemoryLayer {
        match self {
            Self::Agent => api::model::control_api::AgentMemoryLayer::Agent,
            Self::User => api::model::control_api::AgentMemoryLayer::User,
        }
    }
}

/// 会话是否停在两步之间。记忆的读写元数据在工具结果里落库，模型还在想或工具还在跑
/// 时重扫可能赶在写文件之后、记元数据之前。
pub(crate) fn session_phase_settled(phase: api::model::daemon_state::DaemonPhase) -> bool {
    use api::model::daemon_state::DaemonPhase;
    !matches!(
        phase,
        DaemonPhase::Connecting | DaemonPhase::Thinking | DaemonPhase::ExecutingTool
    )
}

/// 编辑页的两个页签。配置是设一次就不怎么动的东西；记忆是会一直长的数据，单独一页，
/// 不把插件和 MCP 挤到后面去。
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum AgentEditorTab {
    #[default]
    Config,
    Memory,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum MemoryFilter {
    #[default]
    All,
    Pending,
    Rules,
    Facts,
}

impl MemoryFilter {
    pub(crate) const ALL: [Self; 4] = [Self::All, Self::Pending, Self::Rules, Self::Facts];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Pending => "待确认",
            Self::Rules => "规则",
            Self::Facts => "事实",
        }
    }

    pub(crate) fn tooltip(self) -> &'static str {
        match self {
            Self::All => "所有记忆",
            Self::Pending => "没生效、等你采纳的记忆",
            Self::Rules => "你本人的情况和你纠正过的做法，每轮都带上",
            Self::Facts => "项目、资源一类的信息，用到时才召回",
        }
    }

    pub(crate) fn matches(self, entry: &api::model::agent_memory::MemoryEntry) -> bool {
        let rule = api::model::agent_memory::is_rule_memory_kind(entry.kind.as_deref());
        match self {
            Self::All => true,
            Self::Pending => entry.pending_reason.is_some(),
            Self::Rules => rule,
            Self::Facts => !rule,
        }
    }
}

/// 记忆页签里一层要显示的记忆。搜索不分大小写，名字、描述、文件名、正文任一命中即可。
/// 待确认要用户处理，排最前；其余按最近用过排，没用过的垫底，同档按名字。
pub(crate) fn visible_memories<'a>(
    entries: &'a [api::model::agent_memory::MemoryEntry],
    query: &str,
    filter: MemoryFilter,
) -> Vec<&'a api::model::agent_memory::MemoryEntry> {
    let query = query.trim().to_lowercase();
    let mut visible: Vec<_> = entries
        .iter()
        .filter(|entry| filter.matches(entry))
        .filter(|entry| {
            query.is_empty()
                || [
                    &entry.name,
                    &entry.description,
                    &entry.file_name,
                    &entry.body,
                ]
                .iter()
                .any(|field| field.to_lowercase().contains(&query))
        })
        .collect();
    visible.sort_by(|a, b| {
        b.pending_reason
            .is_some()
            .cmp(&a.pending_reason.is_some())
            .then_with(|| b.last_used.cmp(&a.last_used))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    visible
}

/// 配置页摘要行的条数。只报数，不列条目，记忆再多也是一行。
pub(crate) fn memory_summary_text(agent: usize, user: usize) -> String {
    if agent + user == 0 {
        "还没有记忆".to_string()
    } else {
        format!("这个智能体 {agent} 条 · 全局 {user} 条")
    }
}

pub(crate) fn save_on_input_event(event: &InputEvent) -> bool {
    matches!(event, InputEvent::Change | InputEvent::Blur)
}

/// 挂着的机器，本机在前，其余按显示名。
pub(crate) fn attached_agent_machines(cx: &App) -> Vec<api::Machine> {
    let mut machines = api::host(cx).machines(cx);
    machines.sort_by_cached_key(|machine| (machine.key != RuntimeKey::Local, machine.name.clone()));
    machines
}

/// 其中智能体配置已经读到的。
pub(crate) fn agent_machines(cx: &App) -> Vec<RuntimeKey> {
    let host = api::host(cx);
    attached_agent_machines(cx)
        .into_iter()
        .map(|machine| machine.key)
        .filter(|key| host.agent_settings(key, cx).is_some())
        .collect()
}

/// 那台机器上的智能体定义；配置还没读到是空的。
pub(crate) fn agents_on(runtime: &RuntimeKey, cx: &App) -> Vec<AgentDefinition> {
    api::host(cx)
        .agent_settings(runtime, cx)
        .map(|config| config.agents.clone())
        .unwrap_or_default()
}

/// 机器给人看的名字；已经移除的机器用它的 id。
pub(crate) fn machine_name(runtime: &RuntimeKey, cx: &App) -> String {
    api::host(cx)
        .machines(cx)
        .into_iter()
        .find(|machine| machine.key == *runtime)
        .map(|machine| machine.name)
        .unwrap_or_else(|| match runtime {
            RuntimeKey::Local => "本机".to_string(),
            RuntimeKey::Remote(id) => id.clone(),
        })
}

/// 机器眼下用不了的原因（离线、版本不对）；能用是 `None`。
pub(crate) fn machine_unavailable(runtime: &RuntimeKey, cx: &App) -> Option<String> {
    api::host(cx)
        .machines(cx)
        .into_iter()
        .find(|machine| machine.key == *runtime)
        .and_then(|machine| machine.unavailable)
}

/// 选择器里一个能开对话的智能体。
#[derive(Clone)]
pub(crate) struct ConversationAgentPick {
    pub(crate) agent: AgentRef,
    pub(crate) name: String,
}

/// 侧栏「新对话」这一下该做什么。
pub(crate) enum NewConversationIntent {
    /// 没有可对话的智能体。去创建，不要开会话。
    CreateAgent,
    /// 有可对话的智能体，但没有有效默认。让用户选一次。
    PickAgent,
    /// 有有效默认。直接用它开对话。
    Start(AgentRef),
}

/// 跨机器的「新对话」决议。默认智能体记在它所在那台机器的配置里，换默认时清掉别的机器
/// 上记着的，所以同一时间最多一台记着；按机器顺序取第一个有效的。每台机器的判定规则见
/// [`api::model::default_conversation::resolve_new_conversation`]。
pub(crate) struct NewConversationPlan {
    pub(crate) intent: NewConversationIntent,
    /// 有效默认的原文名字。只有 [`NewConversationIntent::Start`] 时有值。
    pub(crate) default_name: Option<String>,
    /// 各台机器上能开对话、且不是这次未提交草稿的智能体，按机器顺序。
    pub(crate) ready: Vec<ConversationAgentPick>,
    /// 记着的默认已经对不上任何可对话智能体的机器。调用方应清掉记录。
    pub(crate) stale_defaults: Vec<RuntimeKey>,
}

pub(crate) fn plan_new_conversation(draft: Option<&AgentRef>, cx: &App) -> NewConversationPlan {
    let mut start = None;
    let mut default_name = None;
    let mut ready = Vec::new();
    let mut stale_defaults = Vec::new();
    for key in agent_machines(cx) {
        let host = api::host(cx);
        let Some(config) = host.agent_settings(&key, cx) else {
            continue;
        };
        let draft_id = draft
            .filter(|draft| draft.runtime == key)
            .map(|draft| draft.id.as_str());
        let resolution = api::model::default_conversation::resolve_new_conversation(
            &config.agents,
            config.default_conversation_agent.as_deref(),
            draft_id,
        );
        if resolution.clear_stored_default {
            stale_defaults.push(key.clone());
        }
        if start.is_none()
            && let api::model::default_conversation::NewConversationIntent::Start { agent_id } =
                resolution.intent
        {
            start = Some(AgentRef::new(key.clone(), agent_id));
            default_name = resolution.default_name;
        }
        ready.extend(
            resolution
                .ready
                .into_iter()
                .map(|choice| ConversationAgentPick {
                    agent: AgentRef::new(key.clone(), choice.id),
                    name: choice.name,
                }),
        );
    }
    let intent = match start {
        Some(agent) => NewConversationIntent::Start(agent),
        None if ready.is_empty() => NewConversationIntent::CreateAgent,
        None => NewConversationIntent::PickAgent,
    };
    NewConversationPlan {
        intent,
        default_name,
        ready,
        stale_defaults,
    }
}

/// 它是不是侧栏「新对话」记着的默认。
pub(crate) fn is_default_conversation_agent(agent: &AgentRef, cx: &App) -> bool {
    api::host(cx)
        .agent_settings(&agent.runtime, cx)
        .and_then(|config| config.default_conversation_agent.as_deref())
        == Some(agent.id.as_str())
}

/// 设成「新对话」的默认：记在它那台机器上，别的机器上记着的清掉。
pub(crate) fn set_default_conversation_agent(agent: &AgentRef, cx: &mut App) {
    for key in agent_machines(cx) {
        let target = (key == agent.runtime).then(|| agent.id.clone());
        report_edit(
            api::edit_agent_settings(&key, move |c| c.default_conversation_agent = target, cx),
            cx,
        );
    }
}

pub(crate) fn clear_default_conversation_agent(runtime: &RuntimeKey, cx: &mut App) {
    report_edit(
        api::edit_agent_settings(runtime, |c| c.default_conversation_agent = None, cx),
        cx,
    );
}

/// 不等结果的配置修改：失败原因宿主记在 [`api::Host::agent_settings_error`]，页面从那里
/// 显示，这里只记日志。
pub(crate) fn report_edit(task: Task<Result<(), String>>, cx: &mut App) {
    cx.spawn(async move |_| {
        if let Err(error) = task.await {
            eprintln!("[agents] 改智能体配置失败：{error}");
        }
    })
    .detach();
}

pub(crate) fn is_cjk(ch: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&ch)
}

/// 目录和页头上的缩写。中文取前两字，「日志」比单独一个「日」认得出来；
/// 英文取开头的两个字母，像 Pi。
pub(crate) fn agent_catalog_monogram(name: &str) -> String {
    let name = name.trim();
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return "智".to_string();
    };
    if is_cjk(first) {
        return match chars.next() {
            Some(second) if is_cjk(second) => format!("{first}{second}"),
            _ => first.to_string(),
        };
    }
    let mut word = String::new();
    word.push(first);
    for ch in chars {
        if word.chars().count() >= 2 || ch.is_whitespace() || is_cjk(ch) {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            word.push(ch);
        } else {
            break;
        }
    }
    word
}

pub(crate) fn excerpt_text(value: &str, limit: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = value.chars();
    let mut compact = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        compact.push('…');
    }
    compact
}

/// 产品智能体的执行引擎注册点在 `smelt_core`：运行时也要照同一份名单给手机产出
/// 新建对话选项，注册表留在 GUI crate 里两边就会各注册各的。
pub(crate) use api::model::agent_definition::agent_engine_kinds;

pub(crate) fn agent_display_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "未命名".to_string()
    } else {
        name.to_string()
    }
}

/// 卡片正文大约两行，窄窗口也不会把省略号裁掉。
pub(crate) fn agent_card_prompt(prompt: &str) -> Option<String> {
    let compact = excerpt_text(prompt, 64);
    if compact.is_empty() {
        None
    } else {
        Some(compact)
    }
}

pub(crate) fn agent_instructions_footer(prompt_chars: usize) -> String {
    if prompt_chars == 0 {
        "会作为系统提示词带进每次对话和自动化。".to_string()
    } else {
        format!("{prompt_chars} 字 · 会作为系统提示词带进每次对话和自动化")
    }
}

pub(crate) fn agent_conversation_start_error(agent: &AgentDefinition) -> Option<&'static str> {
    if agent.engine_kind().is_none() {
        Some("请选择一个可用的执行引擎")
    } else {
        None
    }
}

pub(crate) fn agent_usage_summary(conversations: usize, automations: usize) -> String {
    match (conversations, automations) {
        (0, 0) => "还没有对话".to_string(),
        (count, 0) => format!("{count} 段对话"),
        (0, count) => format!("{count} 条自动化"),
        (conversations, automations) => format!("{conversations} 段对话 · {automations} 条自动化"),
    }
}
