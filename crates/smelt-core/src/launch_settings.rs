//! 项目行「+」下拉菜单的快捷启动项：运行时持有，`launch.*` 控制方法背后的实现。
//!
//! 启动项里的命令在运行时那台机器上执行，所以清单跟着运行时走。读取时顺带做出厂
//! 命令迁移、把固定内置项与注册表对齐；客户端只缓存，整表提交时带上它看到的那份，
//! 库里已被别处改过就回冲突，不悄悄覆盖。

pub use smelt_model::launch_settings::*;

use crate::agent_kind::TerminalAgentKind;
pub use crate::new_session::{LaunchEntry, default_launch_entries};

#[derive(Debug, PartialEq, Eq)]
pub enum LaunchError {
    /// 提交时库里的清单已经不是客户端看到的那份。
    Conflict,
    Store(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict => f.write_str("启动项已在别处被修改，已刷新为最新的"),
            Self::Store(message) => f.write_str(message),
        }
    }
}

fn launch_from_snapshot(snapshot: smelt_store::LaunchSnapshot) -> LaunchSettings {
    LaunchSettings {
        version: snapshot.version,
        entries: snapshot
            .entries
            .into_iter()
            .map(|entry| LaunchEntry {
                label: entry.label,
                command: entry.command,
                provider: entry.provider,
            })
            .collect(),
    }
}

pub fn persist_on(store: &smelt_store::Store, settings: &LaunchSettings) {
    let snapshot = smelt_store::LaunchSnapshot {
        version: settings.version,
        entries: settings
            .entries
            .iter()
            .map(|entry| smelt_store::LaunchEntryRecord {
                label: entry.label.clone(),
                command: entry.command.clone(),
                provider: entry.provider.clone(),
            })
            .collect(),
    };
    let _ = store.put_launch_snapshot(&snapshot);
}

pub fn load_on(store: &smelt_store::Store) -> LaunchSettings {
    if let Ok(Some(snapshot)) = store.get_launch_snapshot() {
        let mut config = launch_from_snapshot(snapshot);
        let mut changed = config.version < LAUNCH_CONFIG_VERSION;
        if changed {
            for entry in &mut config.entries {
                if let Some(command) = TerminalAgentKind::ALL
                    .into_iter()
                    .find_map(|agent| agent.upgrade_released_quick_terminal_command(&entry.command))
                {
                    entry.command = command.to_string();
                }
                if entry.label.trim() == "Copilot"
                    && (entry.provider.as_deref() == Some(TerminalAgentKind::Copilot.id())
                        || TerminalAgentKind::from_command_prefix(&entry.command)
                            == Some(TerminalAgentKind::Copilot))
                {
                    entry.label = TerminalAgentKind::Copilot
                        .quick_terminal_label()
                        .to_string();
                }
            }
        }
        changed |= ensure_builtin_launch_entries(&mut config);
        if changed {
            config.version = LAUNCH_CONFIG_VERSION;
            persist_on(store, &config);
        }
        return config;
    }
    LaunchSettings::default()
}

/// 读取启动项（默认库）。
pub fn load() -> Result<LaunchSettings, String> {
    Ok(load_on(&crate::sqlite_state::default_sqlite_store()?))
}

/// 整表替换。`base` 是客户端看到的那份，库里已经不是它时拒绝；内置项是固定集合，
/// 被删掉的会重新补上。返回写入后的清单。
pub fn replace_entries(
    base: &[LaunchEntry],
    entries: Vec<LaunchEntry>,
) -> Result<LaunchSettings, LaunchError> {
    let store = crate::sqlite_state::default_sqlite_store().map_err(LaunchError::Store)?;
    replace_entries_on(&store, base, entries)
}

fn replace_entries_on(
    store: &smelt_store::Store,
    base: &[LaunchEntry],
    entries: Vec<LaunchEntry>,
) -> Result<LaunchSettings, LaunchError> {
    if load_on(store).entries != base {
        return Err(LaunchError::Conflict);
    }
    let mut settings = LaunchSettings {
        version: LAUNCH_CONFIG_VERSION,
        entries,
    };
    ensure_builtin_launch_entries(&mut settings);
    persist_on(store, &settings);
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_launch(dir: &std::path::Path, value: serde_json::Value) -> std::path::PathBuf {
        let database = dir.join(smelt_store::DATABASE_FILE_NAME);
        let store = smelt_store::Store::open_or_create(&database).unwrap();
        store
            .put("json", "launch.json", &serde_json::to_vec(&value).unwrap())
            .unwrap();
        database
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn load_from(path: &std::path::Path) -> LaunchSettings {
        load_on(&smelt_store::Store::open_or_create(path).unwrap())
    }

    fn persist_to(path: &std::path::Path, settings: &LaunchSettings) {
        persist_on(&smelt_store::Store::open_or_create(path).unwrap(), settings);
    }

    /// 另一台设备在我编辑期间改过清单：整表提交必须被拒绝，不能覆盖它。
    #[test]
    fn replacing_entries_against_a_stale_base_is_a_conflict() {
        let dir = temp_dir("launch-conflict");
        let store =
            smelt_store::Store::open_or_create(dir.join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        let seen = load_on(&store).entries;
        let mut theirs = seen.clone();
        theirs.push(LaunchEntry {
            label: "Theirs".into(),
            command: "zsh".into(),
            provider: None,
        });
        replace_entries_on(&store, &seen, theirs.clone()).unwrap();

        let mut mine = seen.clone();
        mine.push(LaunchEntry {
            label: "Mine".into(),
            command: "bash".into(),
            provider: None,
        });
        assert_eq!(
            replace_entries_on(&store, &seen, mine),
            Err(LaunchError::Conflict)
        );
        assert_eq!(load_on(&store).entries, theirs);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn current_launch_config_gains_the_registered_crush_entry() {
        let dir = temp_dir("launch-crush-registration");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": LAUNCH_CONFIG_VERSION,
                "entries": [
                    {"label":"My shell","command":"zsh","provider":null}
                ]
            }),
        );

        let config = load_from(&path);
        let crush = config
            .entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("crush"))
            .expect("当前版本的已有配置也应从注册表补入 Crush");
        assert_eq!(crush.command, "crush --yolo");

        let store = smelt_store::Store::open_or_create(&path).unwrap();
        let snapshot = store.get_launch_snapshot().unwrap().unwrap();
        let persisted = serde_json::json!({
            "version": snapshot.version,
            "entries": snapshot.entries.iter().map(|entry| serde_json::json!({
                "label": entry.label,
                "command": entry.command,
                "provider": entry.provider,
            })).collect::<Vec<_>>(),
        });
        assert!(persisted["entries"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["provider"] == "crush" && entry["command"] == "crush --yolo")
        }));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn released_pi_command_upgrades_to_full_project_trust() {
        let dir = temp_dir("launch-pi-full-access-migration");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 13,
                "entries": [
                    {"label":"Pi","command":"pi","provider":"pi"},
                    {"label":"My Pi","command":"pi --model custom","provider":null}
                ]
            }),
        );

        let config = load_from(&path);
        let builtin = config
            .entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("pi"))
            .expect("迁移后内置 Pi 项应保留");
        assert_eq!(builtin.command, "pi --approve");
        assert!(
            config
                .entries
                .iter()
                .any(|entry| { entry.label == "My Pi" && entry.command == "pi --model custom" })
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn released_kiro_v2_default_moves_to_v3_without_touching_custom_agents() {
        let dir = temp_dir("launch-kiro-v3-migration");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 10,
                "entries": [
                    {
                        "label": "Kiro",
                        "command": "kiro-cli chat --trust-all-tools",
                        "provider": "kiro"
                    },
                    {
                        "label": "My Kiro",
                        "command": "kiro-cli chat --agent personal",
                        "provider": "kiro"
                    }
                ]
            }),
        );

        let config = load_from(&path);
        assert_eq!(config.version, LAUNCH_CONFIG_VERSION);
        assert_eq!(
            config.entries[0].command,
            "kiro-cli chat --v3 --trust-all-tools"
        );
        assert_eq!(
            config.entries[1].command, "kiro-cli chat --agent personal",
            "用户自定义 Kiro 命令不能被出厂迁移覆盖"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_default_launch_config_keeps_one_fixed_antigravity_entry() {
        let dir = temp_dir("launch-antigravity-migration");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "entries": [
                    {"label":"Claude Code","command":"claude --dangerously-skip-permissions"},
                    {"label":"Copilot","command":"copilot --allow-all"},
                    {"label":"Codex","command":"codex --dangerously-bypass-approvals-and-sandbox"},
                    {"label":"Grok","command":"grok"}
                ]
            }),
        );

        let mut config = load_from(&path);
        assert_eq!(config.version, LAUNCH_CONFIG_VERSION);
        assert_eq!(
            config
                .entries
                .iter()
                .filter(|entry| entry.provider.as_deref() == Some("antigravity"))
                .count(),
            1
        );
        config
            .entries
            .retain(|entry| entry.provider.as_deref() != Some("antigravity"));
        persist_to(&path, &config);
        let reloaded = load_from(&path);
        assert_eq!(
            reloaded
                .entries
                .iter()
                .filter(|entry| entry.provider.as_deref() == Some("antigravity"))
                .count(),
            1,
            "固定的 Antigravity 启动项缺失时应自动恢复"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_antigravity_default_command_is_upgraded_to_full_permissions() {
        let dir = temp_dir("launch-antigravity-permission-migration");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 2,
                "entries": [
                    {"label":"Claude Code","command":"claude --dangerously-skip-permissions"},
                    {"label":"Copilot","command":"copilot --allow-all"},
                    {"label":"Codex","command":"codex --dangerously-bypass-approvals-and-sandbox"},
                    {"label":"Grok","command":"grok"},
                    {"label":"Antigravity","command":"agy","provider":"antigravity"}
                ]
            }),
        );

        let config = load_from(&path);
        assert_eq!(config.version, LAUNCH_CONFIG_VERSION);
        let antigravity = config
            .entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("antigravity"))
            .expect("迁移后 Antigravity 条目应保留");
        assert_eq!(
            antigravity.command, "agy --dangerously-skip-permissions",
            "旧出厂值 `agy` 应升级为全权限启动"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn customized_antigravity_command_survives_migration() {
        let dir = temp_dir("launch-antigravity-custom-command");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 2,
                "entries": [
                    {"label":"Antigravity","command":"agy --mode plan","provider":"antigravity"}
                ]
            }),
        );

        let config = load_from(&path);
        let antigravity = config
            .entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("antigravity"))
            .expect("Antigravity 条目应保留");
        assert_eq!(
            antigravity.command, "agy --mode plan",
            "用户自定义过的命令不能被迁移覆盖"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_grok_default_command_is_upgraded_to_full_permissions() {
        let dir = temp_dir("launch-grok-permission-migration");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 2,
                "entries": [
                    {"label":"Grok","command":"grok"}
                ]
            }),
        );

        let config = load_from(&path);
        assert_eq!(config.version, LAUNCH_CONFIG_VERSION);
        let grok = config
            .entries
            .iter()
            .find(|entry| entry.command.starts_with("grok"))
            .expect("迁移后 Grok 条目应保留");
        assert_eq!(
            grok.command, "grok --always-approve",
            "旧出厂值 `grok` 应升级为全权限启动"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forced_codex_thread_title_command_returns_to_the_cli_default() {
        let dir = temp_dir("launch-codex-terminal-title-migration");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 12,
                "entries": [
                    {
                        "label": "Codex",
                        "command": concat!(
                            "codex --dangerously-bypass-approvals-and-sandbox ",
                            "-c 'tui.terminal_title=[\"thread-title\"]'"
                        ),
                        "provider": "codex"
                    },
                    {
                        "label": "My Codex",
                        "command": "codex --model custom",
                        "provider": null
                    }
                ]
            }),
        );

        let config = load_from(&path);
        let builtin = config
            .entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("codex"))
            .expect("迁移后内置 Codex 项应保留");
        assert_eq!(
            builtin.command,
            "codex --dangerously-bypass-approvals-and-sandbox"
        );
        assert!(
            config.entries.iter().any(|entry| {
                entry.label == "My Codex" && entry.command == "codex --model custom"
            })
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_launch_config_keeps_one_fixed_cursor_entry() {
        let dir = temp_dir("launch-cursor-migration");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 4,
                "entries": [
                    {"label":"Claude Code","command":"claude --dangerously-skip-permissions"},
                    {"label":"Copilot","command":"copilot --allow-all"},
                    {"label":"Codex","command":"codex --dangerously-bypass-approvals-and-sandbox"},
                    {"label":"Grok","command":"grok --always-approve"}
                ]
            }),
        );

        let mut config = load_from(&path);
        assert_eq!(config.version, LAUNCH_CONFIG_VERSION);
        assert_eq!(
            config
                .entries
                .iter()
                .find(|entry| entry.command.contains("copilot"))
                .map(|entry| entry.label.as_str()),
            Some("GitHub Copilot"),
            "终端菜单出厂名应与对话菜单一致"
        );
        assert_eq!(
            config
                .entries
                .iter()
                .filter(|entry| entry.provider.as_deref() == Some("cursor")
                    || entry.command.contains("cursor-agent"))
                .count(),
            1
        );
        config
            .entries
            .retain(|entry| entry.provider.as_deref() != Some("cursor"));
        persist_to(&path, &config);
        let reloaded = load_from(&path);
        assert_eq!(
            reloaded
                .entries
                .iter()
                .filter(|entry| entry.provider.as_deref() == Some("cursor"))
                .count(),
            1,
            "固定的 Cursor 启动项缺失时应自动恢复"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_launch_config_keeps_one_fixed_opencode_entry() {
        let dir = temp_dir("launch-opencode-migration");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 6,
                "entries": [
                    {"label":"Claude Code","command":"claude --dangerously-skip-permissions"},
                    {"label":"GitHub Copilot","command":"copilot --allow-all"},
                    {"label":"Codex","command":"codex --dangerously-bypass-approvals-and-sandbox"},
                    {"label":"Grok","command":"grok --always-approve"},
                    {"label":"Antigravity","command":"agy --dangerously-skip-permissions","provider":"antigravity"},
                    {"label":"Cursor Agent","command":"cursor-agent --force","provider":"cursor"}
                ]
            }),
        );

        let mut config = load_from(&path);
        assert_eq!(config.version, LAUNCH_CONFIG_VERSION);
        assert_eq!(
            config
                .entries
                .iter()
                .filter(|entry| entry.provider.as_deref() == Some("opencode")
                    || entry.command.contains("opencode"))
                .count(),
            1
        );
        config
            .entries
            .retain(|entry| entry.provider.as_deref() != Some("opencode"));
        persist_to(&path, &config);
        let reloaded = load_from(&path);
        assert_eq!(
            reloaded
                .entries
                .iter()
                .filter(|entry| entry.provider.as_deref() == Some("opencode"))
                .count(),
            1,
            "固定的 OpenCode 启动项缺失时应自动恢复"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_launch_config_keeps_one_fixed_kiro_entry() {
        let dir = temp_dir("launch-kiro-migration");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 7,
                "entries": [
                    {"label":"Claude Code","command":"claude --dangerously-skip-permissions"},
                    {"label":"GitHub Copilot","command":"copilot --allow-all"},
                    {"label":"Codex","command":"codex --dangerously-bypass-approvals-and-sandbox"},
                    {"label":"Grok","command":"grok --always-approve"},
                    {"label":"Antigravity","command":"agy --dangerously-skip-permissions","provider":"antigravity"},
                    {"label":"Cursor Agent","command":"cursor-agent --force","provider":"cursor"},
                    {"label":"OpenCode","command":"opencode --auto","provider":"opencode"}
                ]
            }),
        );

        let mut config = load_from(&path);
        assert_eq!(config.version, LAUNCH_CONFIG_VERSION);
        assert_eq!(
            config
                .entries
                .iter()
                .filter(|entry| entry.provider.as_deref() == Some("kiro")
                    || entry.command.contains("kiro-cli"))
                .count(),
            1
        );
        config
            .entries
            .retain(|entry| entry.provider.as_deref() != Some("kiro"));
        persist_to(&path, &config);
        let reloaded = load_from(&path);
        assert_eq!(
            reloaded
                .entries
                .iter()
                .filter(|entry| entry.provider.as_deref() == Some("kiro"))
                .count(),
            1,
            "固定的 Kiro 启动项缺失时应自动恢复"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn customized_copilot_label_survives_rename_migration() {
        let dir = temp_dir("launch-copilot-custom-label");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 5,
                "entries": [
                    {"label":"家里的 Copilot","command":"copilot --allow-all","provider":"copilot"}
                ]
            }),
        );

        let config = load_from(&path);
        let copilot = config
            .entries
            .iter()
            .find(|entry| entry.command.contains("copilot"))
            .expect("Copilot 条目应保留");
        assert_eq!(copilot.label, "家里的 Copilot");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn customized_grok_command_survives_migration() {
        let dir = temp_dir("launch-grok-custom-command");
        let path = seed_launch(
            &dir,
            serde_json::json!({
                "version": 2,
                "entries": [
                    {"label":"Grok","command":"grok --model xyz"}
                ]
            }),
        );

        let config = load_from(&path);
        let grok = config
            .entries
            .iter()
            .find(|entry| entry.command.starts_with("grok"))
            .expect("Grok 条目应保留");
        assert_eq!(
            grok.command, "grok --model xyz",
            "用户自定义过的命令不能被迁移覆盖"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
