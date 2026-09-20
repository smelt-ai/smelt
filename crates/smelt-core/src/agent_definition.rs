use serde::{Deserialize, Serialize};

use crate::agent_kind::ConversationAgentKind;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub id: String,
    pub name: String,
    /// 旧版「简介」字段，仅兼容已有存档，界面不再展示或编辑。
    #[serde(default)]
    pub description: String,
    /// 执行该产品智能体的引擎类型。旧配置曾将它写成含义不清的 `agent_id`。
    #[serde(rename = "engine_kind", alias = "agent_id")]
    pub engine_kind_id: String,
    /// 长期工作方式，启动时写入引擎 system prompt。
    pub prompt: String,
    /// 这个智能体要加载的 Pi 插件 id（`skill:<名字>` / `extension:<名字>`）。
    /// 空表示不加载任何插件：会话启动时会关掉 Pi 的自动发现，不做「空则全量」
    /// 的回退，否则用户勾掉最后一个反而会拿回全量。
    #[serde(default)]
    pub plugins: Vec<String>,
    /// 绑定到这个智能体的本地目录。会话的工作目录是智能体自己的 space，业务
    /// 素材靠这里挂进来：Pi 只接受单个 cwd，没有 `--add-dir`，所以只能把绝对
    /// 路径写进 system prompt，让它用 read/bash 去访问。
    #[serde(default)]
    pub context_folders: Vec<String>,
    /// 绑定到这个智能体的参考链接，同样写进 system prompt。
    #[serde(default)]
    pub context_links: Vec<String>,
    /// 这个智能体新对话和自动化用的 Pi provider。空 = 跟随 Pi 全局默认。
    /// 对话里仍可再换模型，只影响这一段会话。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model_provider: String,
    /// 对应的模型 ID。必须和 `model_provider` 成对出现，否则视为未设置。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model_id: String,
}

impl AgentDefinition {
    /// 有稳定 id 和可识别引擎即可使用。工作方式可空，启动时不会写入 system prompt。
    pub fn is_ready(&self) -> bool {
        !self.id.trim().is_empty() && self.engine_kind().is_some()
    }

    pub fn engine_kind(&self) -> Option<ConversationAgentKind> {
        ConversationAgentKind::from_id(&self.engine_kind_id)
    }

    /// 能不能真的用这个定义开一场对话：引擎认得出，而且已注册为产品级引擎。
    /// 桌面新建菜单、移动端选项、自动化都以这一条为准，别各判各的。
    pub fn is_conversation_ready(&self) -> bool {
        self.is_ready()
            && self
                .engine_kind()
                .is_some_and(|kind| agent_engine_kinds().contains(&kind))
    }

    /// 定义里绑了完整的 provider + 模型才算选过。只填一半当作没选，避免发出
    /// `Token-X/` 这种 Pi 认不出的值。
    pub fn model_selection(&self) -> Option<(&str, &str)> {
        let provider = self.model_provider.trim();
        let model = self.model_id.trim();
        if provider.is_empty() || model.is_empty() {
            None
        } else {
            Some((provider, model))
        }
    }

    /// ACP / Pi 会话配置用的 `provider/id`。
    pub fn model_config_value(&self) -> Option<String> {
        self.model_selection()
            .map(|(provider, model)| format!("{provider}/{model}"))
    }
}

/// 产品智能体的执行引擎注册点。第一阶段只开放 Pi；以后某个引擎达到产品级
/// 生命周期与权限语义后，在这里显式注册，不能因为它能开普通对话就自动暴露。
const BUILTIN_AGENT_ENGINES: &[ConversationAgentKind] = &[ConversationAgentKind::Pi];

pub fn agent_engine_kinds() -> Vec<ConversationAgentKind> {
    BUILTIN_AGENT_ENGINES.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(engine: &str) -> AgentDefinition {
        AgentDefinition {
            id: "writer".to_string(),
            name: "写作助手".to_string(),
            engine_kind_id: engine.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn only_registered_engines_can_start_a_conversation() {
        assert!(definition(ConversationAgentKind::Pi.id()).is_conversation_ready());

        // 认得出引擎不等于能开对话：没进注册表的引擎只是普通 CLI。
        let claude = definition(ConversationAgentKind::Claude.id());
        assert!(claude.is_ready());
        assert!(!claude.is_conversation_ready());

        let unknown = definition("nope");
        assert!(!unknown.is_ready());
        assert!(!unknown.is_conversation_ready());
    }

    #[test]
    fn model_selection_requires_both_provider_and_id() {
        let mut agent = definition(ConversationAgentKind::Pi.id());
        assert_eq!(agent.model_selection(), None);
        agent.model_provider = "Token-X".into();
        assert_eq!(agent.model_selection(), None);
        agent.model_id = "Claude-Opus-5".into();
        assert_eq!(agent.model_selection(), Some(("Token-X", "Claude-Opus-5")));
        assert_eq!(
            agent.model_config_value().as_deref(),
            Some("Token-X/Claude-Opus-5")
        );
    }
}
