//! 运行时那份智能体宿主配置：各家 CLI 的启动命令、环境变量、配置记忆、workspace
//! profile、hook 与跨会话消息开关、智能体定义。`agent.host.*` 控制方法背后的实现。
//!
//! 这些都跟着运行时那台机器走（CLI 装在哪、profile 目录在哪），所以由运行时持有；
//! 客户端只缓存一份，修改以「这次改了什么」提交，由这里合并到库里最新的那份上。
//! 通知开关属于客户端，不在这里。

pub use smelt_model::agent_host::*;

use std::collections::BTreeMap;

use crate::agent_kind::{AcpProfile, CONVERSATION_AGENTS, ConversationAgentKind};

fn from_snapshot(snapshot: smelt_store::AgentUiSnapshot) -> AgentHostSettings {
    let mut commands = BTreeMap::new();
    for command in snapshot.commands {
        if let Some(descriptor) = CONVERSATION_AGENTS
            .iter()
            .find(|descriptor| descriptor.config_key == command.command_key)
        {
            commands.insert(descriptor.id.to_string(), command.command);
        }
    }
    let mut env: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for record in snapshot.env {
        env.entry(record.engine_kind_id)
            .or_default()
            .insert(record.name, record.value);
    }
    let mut config_memory: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for record in snapshot.config_memory {
        config_memory
            .entry(record.engine_kind_id)
            .or_default()
            .push((record.config_id, record.value_id));
    }
    AgentHostSettings {
        hooks_enabled: snapshot.agent_hooks_enabled,
        cross_agent_enabled: snapshot.cross_agent_enabled,
        commands,
        env,
        config_memory,
        profiles: snapshot
            .profiles
            .into_iter()
            .map(|profile| AcpProfile {
                native_launch: None,
                id: profile.id,
                kind_id: profile.kind_id,
                label: profile.label,
                workspace_dir: profile.workspace_dir,
            })
            .collect(),
        native_dsh_profiles: Vec::new(),
        agents: Vec::new(),
        default_conversation_agent: None,
    }
}

/// 偏好表的整份内容。智能体定义另走字段级合并，不在这里写。
fn prefs_snapshot(settings: &AgentHostSettings) -> smelt_store::AgentUiSnapshot {
    smelt_store::AgentUiSnapshot {
        agent_hooks_enabled: settings.hooks_enabled,
        cross_agent_enabled: settings.cross_agent_enabled,
        commands: CONVERSATION_AGENTS
            .iter()
            .filter_map(|descriptor| {
                settings.commands.get(descriptor.id).map(|command| {
                    smelt_store::AgentConversationCommandRecord {
                        command_key: descriptor.config_key.to_string(),
                        command: command.clone(),
                    }
                })
            })
            .collect(),
        env: settings
            .env
            .iter()
            .flat_map(|(engine_kind_id, vars)| {
                vars.iter()
                    .map(|(name, value)| smelt_store::AgentAcpEnvRecord {
                        engine_kind_id: engine_kind_id.clone(),
                        name: name.clone(),
                        value: value.clone(),
                    })
            })
            .collect(),
        config_memory: settings
            .config_memory
            .iter()
            .flat_map(|(engine_kind_id, pairs)| {
                pairs.iter().map(
                    |(config_id, value_id)| smelt_store::AgentAcpConfigMemoryRecord {
                        engine_kind_id: engine_kind_id.clone(),
                        config_id: config_id.clone(),
                        value_id: value_id.clone(),
                    },
                )
            })
            .collect(),
        agents: Vec::new(),
        profiles: settings
            .profiles
            .iter()
            .map(|profile| smelt_store::AgentProfileRecord {
                id: profile.id.clone(),
                kind_id: profile.kind_id.clone(),
                label: profile.label.clone(),
                workspace_dir: profile.workspace_dir.clone(),
            })
            .collect(),
    }
}

/// 只迁移之前随应用发出的默认命令；用户自定义的命令不动。
fn migrate_released_adapter_defaults(settings: &mut AgentHostSettings) -> bool {
    let mut migrated = false;
    for agent in ConversationAgentKind::ALL {
        let current = settings
            .commands
            .get(agent.id())
            .cloned()
            .unwrap_or_else(|| agent.default_cmd());
        if let Some(upgraded) = agent.upgrade_released_default_command(&current) {
            settings.commands.insert(agent.id().to_string(), upgraded);
            migrated = true;
        }
    }
    migrated
}

fn store() -> Result<smelt_store::Store, String> {
    crate::sqlite_state::default_sqlite_store()
}

/// 读取整份配置。顺带把过时的出厂命令迁到当前版本并写回。
pub fn load() -> Result<AgentHostSettings, String> {
    load_on(&store()?)
}

fn load_on(store: &smelt_store::Store) -> Result<AgentHostSettings, String> {
    let snapshot = store.get_agent_ui_snapshot().map_err(|e| e.to_string())?;
    let mut settings = snapshot
        .map(from_snapshot)
        .unwrap_or_else(AgentHostSettings::fresh);
    if migrate_released_adapter_defaults(&mut settings) {
        store
            .put_agent_ui_prefs(&prefs_snapshot(&settings))
            .map_err(|e| e.to_string())?;
    }
    settings.agents = crate::agent_definition_store::list_agent_definitions_on(store)
        .map_err(|e| e.to_string())?;
    settings.native_dsh_profiles = crate::agent_kind::native_dsh_profiles();
    settings.default_conversation_agent = crate::default_conversation::load_on(store);
    Ok(settings)
}

/// 两个 map 之间改了的键，写到 `current` 上；没改的键保留 `current` 里的值。
fn merge_map<V: Clone + PartialEq>(
    current: &mut BTreeMap<String, V>,
    before: &BTreeMap<String, V>,
    after: &BTreeMap<String, V>,
) {
    for key in before.keys().chain(after.keys()) {
        if before.get(key) == after.get(key) {
            continue;
        }
        match after.get(key) {
            Some(value) => {
                current.insert(key.clone(), value.clone());
            }
            None => {
                current.remove(key);
            }
        }
    }
}

/// profile 按 id 合并：改了的就地替换，新增的追加，删掉的移除。
fn merge_profiles(current: &mut Vec<AcpProfile>, before: &[AcpProfile], after: &[AcpProfile]) {
    let find = |list: &[AcpProfile], id: &str| list.iter().find(|p| p.id == id).cloned();
    let ids: Vec<String> = before.iter().chain(after).map(|p| p.id.clone()).collect();
    for id in ids {
        let (old, new) = (find(before, &id), find(after, &id));
        if old == new {
            continue;
        }
        match new {
            Some(profile) => match current.iter_mut().find(|p| p.id == id) {
                Some(slot) => *slot = profile,
                None => current.push(profile),
            },
            None => current.retain(|p| p.id != id),
        }
    }
}

fn merge_flag(current: &mut bool, before: bool, after: bool) {
    if before != after {
        *current = after;
    }
}

/// 把一次修改合并到库里最新的那份上，返回合并后的整份配置。只写客户端真正改了的
/// 字段：别的客户端、CLI、Control API 同期的修改不会被这次覆盖。
pub fn apply_edit(edit: &AgentHostEdit) -> Result<AgentHostSettings, String> {
    apply_edit_on(&store()?, edit)
}

fn apply_edit_on(
    store: &smelt_store::Store,
    edit: &AgentHostEdit,
) -> Result<AgentHostSettings, String> {
    let (before, after) = (&edit.before, &edit.after);
    let snapshot = store.get_agent_ui_snapshot().map_err(|e| e.to_string())?;
    let mut current = snapshot
        .map(from_snapshot)
        .unwrap_or_else(AgentHostSettings::fresh);
    merge_flag(
        &mut current.hooks_enabled,
        before.hooks_enabled,
        after.hooks_enabled,
    );
    merge_flag(
        &mut current.cross_agent_enabled,
        before.cross_agent_enabled,
        after.cross_agent_enabled,
    );
    merge_map(&mut current.commands, &before.commands, &after.commands);
    merge_map(&mut current.env, &before.env, &after.env);
    merge_map(
        &mut current.config_memory,
        &before.config_memory,
        &after.config_memory,
    );
    merge_profiles(&mut current.profiles, &before.profiles, &after.profiles);
    store
        .put_agent_ui_prefs(&prefs_snapshot(&current))
        .map_err(|e| e.to_string())?;
    crate::agent_definition_store::sync_agent_definitions_on(store, &before.agents, &after.agents)
        .map_err(|e| e.to_string())?;
    if before.default_conversation_agent != after.default_conversation_agent {
        crate::default_conversation::store_on(store, after.default_conversation_agent.as_deref())?;
    }
    load_on(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(settings: &AgentHostSettings, agent: ConversationAgentKind) -> String {
        settings
            .commands
            .get(agent.id())
            .cloned()
            .unwrap_or_else(|| agent.default_cmd())
    }

    fn set_cmd(settings: &mut AgentHostSettings, agent: ConversationAgentKind, command: String) {
        settings.commands.insert(agent.id().to_string(), command);
    }

    fn temp_store() -> (smelt_store::Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "smelt-agent-host-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        (store, dir)
    }

    /// 端到端：客户端改一条命令，库里只变这一条，别的偏好原样留着。
    #[test]
    fn apply_edit_persists_only_the_changed_field() {
        let (store, dir) = temp_store();
        let mut seeded = prefs_snapshot(&AgentHostSettings::fresh());
        seeded.cross_agent_enabled = false;
        store.put_agent_ui_prefs(&seeded).unwrap();

        let before = load_on(&store).unwrap();
        let mut after = before.clone();
        after
            .commands
            .insert("claude".into(), "claude --edited".into());
        let merged = apply_edit_on(&store, &AgentHostEdit { before, after }).unwrap();

        assert_eq!(merged.commands["claude"], "claude --edited");
        let stored = store.get_agent_ui_snapshot().unwrap().unwrap();
        assert!(!stored.cross_agent_enabled, "没改的开关不能被这次提交改掉");
        assert_eq!(load_on(&store).unwrap(), merged);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn with_cmd(agent: ConversationAgentKind, command: String) -> AgentHostSettings {
        let mut settings = AgentHostSettings::fresh();
        set_cmd(&mut settings, agent, command);
        settings
    }

    fn profile(id: &str, label: &str) -> AcpProfile {
        AcpProfile {
            native_launch: None,
            id: id.into(),
            kind_id: "claude".into(),
            label: label.into(),
            workspace_dir: format!("~/.claude-{id}"),
        }
    }

    /// 别的客户端改了 A，我改了 B：合并后两处都在，谁也没覆盖谁。
    #[test]
    fn edits_only_write_the_fields_this_client_changed() {
        let before = AgentHostSettings::fresh();
        let mut current = before.clone();
        current
            .commands
            .insert("codex".into(), "codex --theirs".into());
        current.cross_agent_enabled = false;

        let mut after = before.clone();
        after
            .commands
            .insert("claude".into(), "claude --mine".into());
        after.hooks_enabled = false;

        merge_flag(
            &mut current.hooks_enabled,
            before.hooks_enabled,
            after.hooks_enabled,
        );
        merge_flag(
            &mut current.cross_agent_enabled,
            before.cross_agent_enabled,
            after.cross_agent_enabled,
        );
        merge_map(&mut current.commands, &before.commands, &after.commands);

        assert_eq!(current.commands["codex"], "codex --theirs");
        assert_eq!(current.commands["claude"], "claude --mine");
        assert!(!current.hooks_enabled);
        assert!(!current.cross_agent_enabled, "没改的开关保留别人的值");
    }

    #[test]
    fn removing_a_key_in_the_edit_removes_it() {
        let mut before = AgentHostSettings::fresh();
        before
            .env
            .insert("claude".into(), BTreeMap::from([("A".into(), "1".into())]));
        let mut current = before.clone();
        let mut after = before.clone();
        after.env.remove("claude");

        merge_map(&mut current.env, &before.env, &after.env);

        assert!(!current.env.contains_key("claude"));
    }

    #[test]
    fn profiles_merge_by_id() {
        let before = vec![profile("a", "A"), profile("b", "B")];
        let mut current = vec![profile("a", "A"), profile("b", "B"), profile("c", "theirs")];
        let after = vec![profile("a", "A renamed"), profile("d", "D")];

        merge_profiles(&mut current, &before, &after);

        let labels: Vec<_> = current
            .iter()
            .map(|p| (p.id.as_str(), p.label.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![("a", "A renamed"), ("c", "theirs"), ("d", "D")],
            "改名就地替换，删掉的移除，别人加的保留，新加的追加"
        );
    }

    #[test]
    fn released_default_commands_are_migrated_but_custom_ones_are_kept() {
        let mut settings = AgentHostSettings::fresh();
        settings
            .commands
            .insert("claude".into(), "my-claude --flag".into());
        let custom_before = settings.commands["claude"].clone();

        migrate_released_adapter_defaults(&mut settings);

        assert_eq!(settings.commands["claude"], custom_before);
    }

    #[test]
    fn legacy_codex_default_is_replaced_by_current_adapter() {
        let mut config = AgentHostSettings::fresh();
        set_cmd(
            &mut config,
            ConversationAgentKind::Codex,
            "bunx --bun @zed-industries/codex-acp@0.16.0".into(),
        );

        assert!(migrate_released_adapter_defaults(&mut config));

        assert_eq!(
            cmd(&config, ConversationAgentKind::Codex),
            ConversationAgentKind::Codex.default_cmd()
        );
    }

    #[test]
    fn interim_app_server_default_is_replaced_by_current_adapter() {
        let mut config = AgentHostSettings::fresh();
        set_cmd(
            &mut config,
            ConversationAgentKind::Codex,
            "codex app-server".into(),
        );

        assert!(migrate_released_adapter_defaults(&mut config));
        assert_eq!(
            cmd(&config, ConversationAgentKind::Codex),
            ConversationAgentKind::Codex.default_cmd()
        );
    }

    #[test]
    fn previous_official_adapter_defaults_are_replaced_by_current_versions() {
        let mut config = AgentHostSettings::fresh();
        set_cmd(
            &mut config,
            ConversationAgentKind::Claude,
            "bunx --bun @agentclientprotocol/claude-agent-acp@0.59.0".into(),
        );
        set_cmd(
            &mut config,
            ConversationAgentKind::Codex,
            "bunx --bun @agentclientprotocol/codex-acp@1.1.7".into(),
        );

        assert!(migrate_released_adapter_defaults(&mut config));
        assert_eq!(
            cmd(&config, ConversationAgentKind::Claude),
            "bunx --bun @agentclientprotocol/claude-agent-acp@0.78.0"
        );
        assert_eq!(
            cmd(&config, ConversationAgentKind::Codex),
            "bunx --bun @agentclientprotocol/codex-acp@1.12.0"
        );
    }

    #[test]
    fn unsupported_kiro_v3_trust_default_is_migrated_without_touching_custom_commands() {
        let mut config = AgentHostSettings::fresh();
        set_cmd(
            &mut config,
            ConversationAgentKind::Kiro,
            "kiro-cli acp --agent-engine v3 --auth-method cli --trust-all-tools".into(),
        );

        assert!(migrate_released_adapter_defaults(&mut config));
        assert_eq!(
            cmd(&config, ConversationAgentKind::Kiro),
            "kiro-cli acp --agent-engine v3 --auth-method cli"
        );

        let custom =
            "kiro-cli acp --agent-engine v3 --auth-method cli --agent personal".to_string();
        let mut config = with_cmd(ConversationAgentKind::Kiro, custom.clone());

        assert!(!migrate_released_adapter_defaults(&mut config));
        assert_eq!(cmd(&config, ConversationAgentKind::Kiro), custom);
    }

    #[test]
    fn customized_previous_adapter_command_is_not_replaced() {
        let custom = "NO_BROWSER=1 bunx --bun @agentclientprotocol/codex-acp@1.1.7".to_string();
        let mut config = with_cmd(ConversationAgentKind::Codex, custom.clone());

        assert!(!migrate_released_adapter_defaults(&mut config));
        assert_eq!(cmd(&config, ConversationAgentKind::Codex), custom);
    }

    #[test]
    fn current_adapter_defaults_do_not_repeat_migration() {
        let mut config = AgentHostSettings::fresh();

        assert!(!migrate_released_adapter_defaults(&mut config));
    }

    #[test]
    fn custom_codex_adapter_is_not_migrated() {
        let mut config = AgentHostSettings::fresh();
        set_cmd(
            &mut config,
            ConversationAgentKind::Codex,
            "codex-acp --custom".into(),
        );

        assert!(!migrate_released_adapter_defaults(&mut config));
        assert_eq!(
            cmd(&config, ConversationAgentKind::Codex),
            "codex-acp --custom"
        );
    }
}
