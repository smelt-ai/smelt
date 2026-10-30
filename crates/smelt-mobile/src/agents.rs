//! 一台机器上智能体的新建、编辑、删除和记忆审阅。Dart 入口见 `crate::api::agents`。
//!
//! 全是一问一答：定义和记忆都没有推送，也不在这里缓存。编辑页要的模型、插件、MCP 选项
//! 取自这台机器的能力库（`pi.config.get`），取不到时照样能改名字和指令。

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use smelt_model::agent_definition::AgentDefinition;
use smelt_model::agent_definition_store::{AgentDefinitionCreate, AgentDefinitionPatch};
use smelt_model::agent_memory::{self as model_memory, MemoryEntry, MemoryListing};
use smelt_model::control_api::{
    AgentDefinitionCreateCall, AgentDefinitionDelete, AgentDefinitionGet, AgentDefinitionIdParams,
    AgentDefinitionUpdateCall, AgentDefinitionUpdateParams, AgentMemoryApprove, AgentMemoryDelete,
    AgentMemoryFileParams, AgentMemoryLayer, AgentMemoryList, PiConfigGet,
};
use smelt_model::pi_plugin_catalog::PiPluginKind;
use smelt_model::pi_service::PiConfig;
use smelt_runtime_client::protocol::control::EmptyParams;

use crate::api::agents::{
    AgentDraft, AgentEditor, AgentMemories, McpOption, MemoryItem, MemoryLayer, MemoryShelf,
    ModelGroup, ModelOption, PluginKind, PluginOption,
};
use crate::api::catalog::AgentDefinitionSummary;
use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::catalog::control_error;
use crate::connection::CurrentRuntime;

/// 这台机器的连接从哪来。生产用节点的 `NodeSource`（见 `crate::fleet`），测试注入假的。
pub(crate) trait RuntimeSource: Send + Sync {
    fn current(&self) -> Option<CurrentRuntime>;
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// 读写定义和记忆。
    pub call_timeout: Duration,
    /// 读能力库：要扫插件目录、读几份配置文件。
    pub slow_call_timeout: Duration,
}

impl Timing {
    pub(crate) const PRODUCTION: Self = Self {
        call_timeout: Duration::from_secs(15),
        slow_call_timeout: Duration::from_secs(30),
    };
}

pub(crate) struct Agents {
    source: Arc<dyn RuntimeSource>,
    timing: Timing,
}

impl Agents {
    pub(crate) fn new(source: Arc<dyn RuntimeSource>, timing: Timing) -> Self {
        Self { source, timing }
    }

    fn runtime(&self) -> Result<CurrentRuntime, SmeltError> {
        self.source
            .current()
            .ok_or_else(|| error(SmeltErrorKind::Offline, "还没有连上这台机器"))
    }

    pub(crate) fn editor(&self, agent_id: Option<&str>) -> Result<AgentEditor, SmeltError> {
        let runtime = self.runtime()?;
        let draft = match agent_id {
            Some(id) => draft_of(&self.definition(&runtime, id)?),
            None => AgentDraft {
                memory_enabled: true,
                ..AgentDraft::default()
            },
        };
        let config = runtime
            .client
            .call::<PiConfigGet>(&EmptyParams::default(), self.timing.slow_call_timeout)
            .map_err(control_error);
        Ok(match config {
            Ok(config) => editor_with(draft, config),
            Err(failure) => AgentEditor {
                draft,
                models: Vec::new(),
                plugins: Vec::new(),
                mcp_servers: Vec::new(),
                choices_error: Some(failure.message),
            },
        })
    }

    fn definition(
        &self,
        runtime: &CurrentRuntime,
        id: &str,
    ) -> Result<AgentDefinition, SmeltError> {
        runtime
            .client
            .call::<AgentDefinitionGet>(
                &AgentDefinitionIdParams { id: id.to_string() },
                self.timing.call_timeout,
            )
            .map(|result| result.agent)
            .map_err(control_error)
    }

    pub(crate) fn create(&self, draft: AgentDraft) -> Result<AgentDefinitionSummary, SmeltError> {
        let draft = tidy(draft)?;
        let runtime = self.runtime()?;
        let (model_provider, model_id) = model_pair(&draft);
        runtime
            .client
            .call::<AgentDefinitionCreateCall>(
                &AgentDefinitionCreate {
                    name: draft.name,
                    prompt: draft.prompt,
                    plugins: draft.plugins,
                    mcp_servers: draft.mcp_servers,
                    context_folders: draft.context_folders,
                    context_links: draft.context_links,
                    model_provider: (!model_provider.is_empty()).then_some(model_provider),
                    model_id: (!model_id.is_empty()).then_some(model_id),
                    memory_enabled: Some(draft.memory_enabled),
                    ..AgentDefinitionCreate::default()
                },
                self.timing.call_timeout,
            )
            .map(|result| crate::catalog::agent_summary(&result.agent))
            .map_err(control_error)
    }

    pub(crate) fn update(
        &self,
        id: &str,
        original: &AgentDraft,
        edited: &AgentDraft,
    ) -> Result<AgentDefinitionSummary, SmeltError> {
        let edited = tidy(edited.clone())?;
        let runtime = self.runtime()?;
        let patch = patch_between(original, &edited);
        let agent = if patch == AgentDefinitionPatch::default() {
            self.definition(&runtime, id)?
        } else {
            runtime
                .client
                .call::<AgentDefinitionUpdateCall>(
                    &AgentDefinitionUpdateParams {
                        id: id.to_string(),
                        patch,
                    },
                    self.timing.call_timeout,
                )
                .map(|result| result.agent)
                .map_err(control_error)?
        };
        Ok(crate::catalog::agent_summary(&agent))
    }

    pub(crate) fn delete(&self, id: &str) -> Result<(), SmeltError> {
        let runtime = self.runtime()?;
        runtime
            .client
            .call::<AgentDefinitionDelete>(
                &AgentDefinitionIdParams { id: id.to_string() },
                self.timing.call_timeout,
            )
            .map(|_| ())
            .map_err(control_error)
    }

    pub(crate) fn memories(&self, agent_id: &str) -> Result<AgentMemories, SmeltError> {
        let runtime = self.runtime()?;
        self.list_memories(&runtime, agent_id)
    }

    fn list_memories(
        &self,
        runtime: &CurrentRuntime,
        agent_id: &str,
    ) -> Result<AgentMemories, SmeltError> {
        runtime
            .client
            .call::<AgentMemoryList>(
                &AgentDefinitionIdParams {
                    id: agent_id.to_string(),
                },
                self.timing.call_timeout,
            )
            .map(memories_view)
            .map_err(control_error)
    }

    pub(crate) fn approve_memory(
        &self,
        agent_id: &str,
        layer: MemoryLayer,
        file_name: &str,
    ) -> Result<AgentMemories, SmeltError> {
        let runtime = self.runtime()?;
        runtime
            .client
            .call::<AgentMemoryApprove>(
                &memory_file(agent_id, layer, file_name),
                self.timing.call_timeout,
            )
            .map_err(control_error)?;
        self.list_memories(&runtime, agent_id)
    }

    pub(crate) fn delete_memory(
        &self,
        agent_id: &str,
        layer: MemoryLayer,
        file_name: &str,
    ) -> Result<AgentMemories, SmeltError> {
        let runtime = self.runtime()?;
        runtime
            .client
            .call::<AgentMemoryDelete>(
                &memory_file(agent_id, layer, file_name),
                self.timing.call_timeout,
            )
            .map_err(control_error)?;
        self.list_memories(&runtime, agent_id)
    }
}

// ---------------------------------------------------------------------------
// 投影
// ---------------------------------------------------------------------------

fn draft_of(definition: &AgentDefinition) -> AgentDraft {
    AgentDraft {
        name: definition.name.clone(),
        prompt: definition.prompt.clone(),
        plugins: definition.plugins.clone(),
        mcp_servers: definition.mcp_servers.clone(),
        context_folders: definition.context_folders.clone(),
        context_links: definition.context_links.clone(),
        model_provider: definition.model_provider.clone(),
        model_id: definition.model_id.clone(),
        memory_enabled: definition.memory_enabled,
    }
}

/// 名字不能空；列表去掉空项和首尾空白。长期指令原样保留（缩进、空行都是用户写的）。
fn tidy(mut draft: AgentDraft) -> Result<AgentDraft, SmeltError> {
    draft.name = draft.name.trim().to_string();
    if draft.name.is_empty() {
        return Err(error(SmeltErrorKind::Other, "智能体要有名字"));
    }
    for list in [
        &mut draft.plugins,
        &mut draft.mcp_servers,
        &mut draft.context_folders,
        &mut draft.context_links,
    ] {
        *list = list
            .iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
    }
    Ok(draft)
}

/// 模型要成对：只填了一半当没填，跟随默认。
fn model_pair(draft: &AgentDraft) -> (String, String) {
    let provider = draft.model_provider.trim();
    let id = draft.model_id.trim();
    if provider.is_empty() || id.is_empty() {
        (String::new(), String::new())
    } else {
        (provider.to_string(), id.to_string())
    }
}

/// 只有和打开时不同的字段才发：桌面同时改了别的字段，不会被这边打开时的旧值盖掉。
fn patch_between(original: &AgentDraft, edited: &AgentDraft) -> AgentDefinitionPatch {
    fn changed<T: PartialEq + Clone>(before: &T, after: &T) -> Option<T> {
        (before != after).then(|| after.clone())
    }
    let (provider_before, id_before) = model_pair(original);
    let (provider_after, id_after) = model_pair(edited);
    let model_changed = (provider_before, id_before) != (provider_after.clone(), id_after.clone());
    AgentDefinitionPatch {
        name: changed(&original.name.trim().to_string(), &edited.name),
        prompt: changed(&original.prompt, &edited.prompt),
        engine_kind_id: None,
        plugins: changed(&original.plugins, &edited.plugins),
        mcp_servers: changed(&original.mcp_servers, &edited.mcp_servers),
        context_folders: changed(&original.context_folders, &edited.context_folders),
        context_links: changed(&original.context_links, &edited.context_links),
        model_provider: model_changed.then_some(provider_after),
        model_id: model_changed.then_some(id_after),
        memory_enabled: changed(&original.memory_enabled, &edited.memory_enabled),
    }
}

fn editor_with(draft: AgentDraft, config: PiConfig) -> AgentEditor {
    let (mcp_servers, mcp_error) = match config.mcp_servers {
        Ok(servers) => (
            servers
                .into_iter()
                .map(|server| McpOption {
                    name: server.name,
                    description: server.description,
                    enabled: server.enabled,
                })
                .collect(),
            None,
        ),
        Err(message) => (Vec::new(), Some(message)),
    };
    AgentEditor {
        draft,
        models: config
            .model_choice_groups
            .into_iter()
            .map(|group| ModelGroup {
                models: group
                    .models
                    .iter()
                    .map(|choice| ModelOption {
                        provider: choice.provider.clone(),
                        id: choice.id.clone(),
                        label: choice.label(),
                    })
                    .collect(),
                provider: group.provider,
                provider_name: group.provider_name,
            })
            .collect(),
        plugins: config
            .plugins
            .into_iter()
            .map(|plugin| PluginOption {
                id: plugin.id,
                kind: match plugin.kind {
                    PiPluginKind::Skill => PluginKind::Skill,
                    PiPluginKind::Extension => PluginKind::Extension,
                },
                name: plugin.name,
                description: plugin.description,
                broken: plugin.broken,
            })
            .collect(),
        mcp_servers,
        choices_error: mcp_error,
    }
}

fn memory_file(agent_id: &str, layer: MemoryLayer, file_name: &str) -> AgentMemoryFileParams {
    AgentMemoryFileParams {
        agent_id: agent_id.to_string(),
        layer: match layer {
            MemoryLayer::Agent => AgentMemoryLayer::Agent,
            MemoryLayer::User => AgentMemoryLayer::User,
        },
        file_name: file_name.to_string(),
    }
}

fn memories_view(memories: model_memory::AgentMemories) -> AgentMemories {
    let agent = memory_shelf(memories.agent);
    let user = memory_shelf(memories.user);
    let pending = agent
        .items
        .iter()
        .chain(&user.items)
        .filter(|item| item.pending_reason.is_some())
        .count();
    AgentMemories {
        agent,
        user,
        pending: u32::try_from(pending).unwrap_or(u32::MAX),
    }
}

/// 和桌面记忆页同序：待确认的要人处理，排最前；其余按最近用过排，没用过的垫底，同档按名字。
fn memory_shelf(listing: MemoryListing) -> MemoryShelf {
    let mut entries = listing.entries;
    entries.sort_by(|a, b| {
        b.pending_reason
            .is_some()
            .cmp(&a.pending_reason.is_some())
            .then_with(|| b.last_used.cmp(&a.last_used))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    MemoryShelf {
        items: entries.into_iter().map(memory_item).collect(),
        core_over_budget: listing.core_over_budget,
    }
}

fn memory_item(entry: MemoryEntry) -> MemoryItem {
    MemoryItem {
        rule: model_memory::is_rule_memory_kind(entry.kind.as_deref()),
        last_used_ms: entry
            .last_used
            .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
            .and_then(|since| i64::try_from(since.as_millis()).ok()),
        file_name: entry.file_name,
        name: entry.name,
        description: entry.description,
        pending_reason: entry.pending_reason,
        stale: entry.stale,
        read_count: entry.read_count,
        body: entry.body,
    }
}
