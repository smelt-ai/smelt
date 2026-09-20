use super::*;

#[cfg(test)]
mod appearance_tests {
    use super::Appearance;
    use gpui::WindowBackgroundAppearance;

    #[test]
    fn window_opacity_clamps_invalid_persisted_values() {
        let mut appearance = Appearance::default();

        assert_eq!(appearance.window_opacity(), 0.95);

        appearance.opacity = 0.2;
        assert_eq!(appearance.window_opacity(), 0.3);

        appearance.opacity = 0.6;
        assert_eq!(appearance.window_opacity(), 0.6);

        appearance.opacity = 1.2;
        assert_eq!(appearance.window_opacity(), 1.0);

        appearance.opacity = f32::NAN;
        assert_eq!(appearance.window_opacity(), 1.0);
    }

    #[test]
    fn appearance_deserialization_defaults() {
        // 旧版本 appearance.json 没有 ui_font_px / ui_font_family 字段时回退默认
        let json = r#"{"bg_color":1710886,"opacity":0.95,"blur":true,"font_px":13}"#;
        let ap: Appearance = serde_json::from_str(json).unwrap();
        assert_eq!(ap.ui_font_px, super::DEFAULT_UI_FONT_PX);
        assert_eq!(ap.ui_font_family, "");
        assert_eq!(ap.font_px, 13);
        assert_eq!(
            super::resolved_ui_font_family(&ap).as_ref(),
            ".SystemUIFont"
        );
    }

    #[test]
    fn liquid_glass_keeps_a_stable_blurred_window_backdrop() {
        assert_eq!(
            Appearance::default().window_bg(),
            WindowBackgroundAppearance::Blurred
        );
    }

    #[test]
    fn settings_window_stays_opaque_for_readability() {
        assert_eq!(super::SETTINGS_WINDOW_OPACITY, 1.0);
    }
}

#[cfg(test)]
mod update_settings_tests {
    use super::UpdateSettings;
    use crate::updater::UpdateChannel;

    /// 开关是后加的，已有 update-settings.json 里没有这个键。缺省必须补成 true，
    /// 否则老用户升上来会静默失去自动更新——这是最不该出现的回归。
    #[test]
    fn auto_install_defaults_to_enabled_for_old_config() {
        let settings: UpdateSettings = serde_json::from_str("{}").unwrap();
        assert!(settings.auto_install);

        let settings: UpdateSettings = serde_json::from_str(r#"{"channel":"dev"}"#).unwrap();
        assert!(settings.auto_install);
        assert_eq!(settings.channel, UpdateChannel::Dev);

        assert!(UpdateSettings::default().auto_install);
    }

    /// 显式关掉要能存下来也读得回来，别被 default 覆盖。
    #[test]
    fn auto_install_roundtrips_when_disabled() {
        let settings = UpdateSettings {
            channel: UpdateChannel::Dev,
            auto_install: false,
        };
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: UpdateSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, settings);
        assert!(!parsed.auto_install);
    }
}

#[cfg(test)]
mod iroh_pairing_tests {
    use super::{RemoteConfig, qr_png_for_url};
    use smelt_pairing::iroh_pairing_uri;

    #[test]
    fn old_config_with_retired_remote_fields_still_loads() {
        // serde 默认忽略旧版的多通路开关；升级后 enabled 是唯一远程开关。
        let json = r#"{"enabled":true,"iroh_enabled":true,"tunnel_enabled":false,"webrtc_enabled":true,
                       "signal_http":"https://s.example.com","write_enabled":true}"#;
        let c: RemoteConfig = serde_json::from_str(json).expect("旧配置必须还能解析");
        assert!(c.enabled);
        assert!(c.write_enabled);
    }

    #[test]
    fn pairing_uri_renders_to_a_qr() {
        // 配对码比 http 链接长（endpoint_id 是 64 个十六进制字符），
        // 这里钉住「它确实能编成二维码」——超长内容会让 QrCode::new 失败。
        let uri = iroh_pairing_uri(
            &"a".repeat(64),
            &"b".repeat(32),
            "https://relay.example.test",
        );
        let png = qr_png_for_url(&uri).expect("配对码必须能生成二维码");
        assert!(!png.is_empty());
        assert_eq!(&png[1..4], b"PNG", "应当是 PNG 字节流");
    }
}

#[cfg(test)]
mod daemon_info_tests {
    use super::{acp_cmd_setting_value, command_uses_smelt_notify, daemon_info_line, fmt_uptime};
    use crate::terminal::DaemonInfo;
    use smelt_core::agent_kind::ConversationAgentKind;

    #[test]
    fn builtin_agent_command_is_hidden_in_settings() {
        let agent = ConversationAgentKind::Codex;
        assert!(acp_cmd_setting_value(agent, agent.default_cmd()).is_empty());
    }

    #[test]
    fn custom_agent_command_remains_visible_in_settings() {
        assert_eq!(
            acp_cmd_setting_value(ConversationAgentKind::Codex, "codex-acp --custom".into())
                .as_ref(),
            "codex-acp --custom"
        );
    }

    #[test]
    fn fmt_uptime_picks_two_units() {
        assert_eq!(fmt_uptime(45), "45 秒");
        assert_eq!(fmt_uptime(600), "10 分钟");
        assert_eq!(fmt_uptime(3600 * 3 + 60 * 12), "3 小时 12 分");
        assert_eq!(fmt_uptime(86400 * 2 + 3600 * 5), "2 天 5 小时");
    }

    /// 老守护只回 version/exe_mtime：拿不到的字段整段省掉，不摆「未知」占位。
    #[test]
    fn old_daemon_without_new_fields_shows_only_version() {
        let info = DaemonInfo {
            version: Some("0.5.4".into()),
            ..Default::default()
        };
        assert_eq!(daemon_info_line(&info), "v0.5.4");
    }

    /// 全字段齐活：各段用 · 连起来，PID 和会话数都在。
    #[test]
    fn full_info_joins_all_parts() {
        let info = DaemonInfo {
            version: Some("0.5.4".into()),
            pid: Some(64954),
            started_at: Some(1_000_000),
            session_count: Some(5),
        };
        let line = daemon_info_line(&info);
        assert!(
            line.starts_with("v0.5.4 · PID 64954 · 启动于 "),
            "got {line}"
        );
        assert!(line.contains("已运行 "), "got {line}");
        assert!(line.ends_with("· 5 个会话"), "got {line}");
    }

    /// 守护时钟比 GUI 快时不能算出天文数字（saturating_sub 兜底）。
    #[test]
    fn future_started_at_does_not_underflow() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 9999;
        let info = DaemonInfo {
            started_at: Some(future),
            ..Default::default()
        };
        assert!(daemon_info_line(&info).contains("已运行 0 秒"));
    }

    #[test]
    fn hook_ownership_requires_smelt_notify_as_the_executable() {
        assert!(command_uses_smelt_notify(
            "SMELT_HOOK_PROVIDER=copilot /Users/test/.smelt/bin/smelt-notify"
        ));
        assert!(command_uses_smelt_notify(
            "'/Applications/Smelt App/smelt-notify'"
        ));
        assert!(!command_uses_smelt_notify("echo smelt-notify"));
        assert!(!command_uses_smelt_notify(
            "if test -x /tmp/smelt-notify; then /tmp/other-hook; fi"
        ));
    }

    #[test]
    fn removing_smelt_hook_preserves_third_party_handlers() {
        let dir = std::env::temp_dir().join(format!(
            "smelt-hook-remove-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "hooks": {
                    "PreToolUse": [{
                        "matcher": "*",
                        "hooks": [
                            {"type":"command","command":"/tmp/smelt-notify"},
                            {"type":"command","command":"/tmp/orca-hook"}
                        ]
                    }]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        super::uninstall_hook_file(Some(path.clone()), &["PreToolUse"]).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let handlers = value["hooks"]["PreToolUse"][0]["hooks"].as_array().unwrap();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0]["command"], "/tmp/orca-hook");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_json_write_keeps_dotfile_symlink() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "smelt-hook-symlink-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("managed-settings.json");
        let link = dir.join("settings.json");
        std::fs::write(&target, "{}").unwrap();
        symlink(&target, &link).unwrap();

        super::write_json_atomic(&link, &serde_json::json!({"hooks":{"Stop":[]}})).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(value["hooks"]["Stop"], serde_json::json!([]));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod antigravity_terminal_tests {
    use super::{
        ANTIGRAVITY_HOOK_EVENTS, ANTIGRAVITY_HOOK_NAME, LAUNCH_CONFIG_VERSION, LaunchConfig,
        LaunchEntry, antigravity_event_installed, command_uses_smelt_notify,
        default_launch_entries, load_launch_config_from_path, merge_antigravity_hooks,
        remove_launch_entry_at, uninstall_antigravity_hooks_at, write_json_atomic,
    };

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

    fn smelt_handler_count(definition: &serde_json::Value, event: &str) -> usize {
        let Some(handlers) = definition.get(event).and_then(|value| value.as_array()) else {
            return 0;
        };
        if event == "PostToolUse" {
            handlers
                .iter()
                .filter_map(|group| group.get("hooks").and_then(|value| value.as_array()))
                .flatten()
                .filter(|handler| {
                    handler
                        .get("command")
                        .and_then(|value| value.as_str())
                        .is_some_and(command_uses_smelt_notify)
                })
                .count()
        } else {
            handlers
                .iter()
                .filter(|handler| {
                    handler
                        .get("command")
                        .and_then(|value| value.as_str())
                        .is_some_and(command_uses_smelt_notify)
                })
                .count()
        }
    }

    #[test]
    fn default_terminal_entries_include_registered_full_permission_launches() {
        let entries = default_launch_entries();
        let antigravity = entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("antigravity"))
            .expect("出厂快捷项应包含 Antigravity");
        assert_eq!(antigravity.label, "Antigravity");
        assert_eq!(antigravity.command, "agy --dangerously-skip-permissions");
        let grok = entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("grok"))
            .expect("出厂快捷项应包含 Grok");
        assert_eq!(grok.command, "grok --always-approve");
        let cursor = entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("cursor"))
            .expect("出厂快捷项应包含 Cursor Agent");
        assert_eq!(cursor.label, "Cursor Agent");
        assert_eq!(cursor.command, "cursor-agent --force");
        let opencode = entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("opencode"))
            .expect("出厂快捷项应包含 OpenCode");
        assert_eq!(opencode.label, "OpenCode");
        assert_eq!(opencode.command, "opencode --auto");
        let kiro = entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("kiro"))
            .expect("出厂快捷项应包含 Kiro");
        assert_eq!(kiro.label, "Kiro");
        assert_eq!(kiro.command, "kiro-cli chat --v3 --trust-all-tools");
        let pi = entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("pi"))
            .expect("出厂快捷项应包含 Pi");
        assert_eq!(pi.label, "Pi");
        assert_eq!(pi.command, "pi --approve");
        let crush = entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("crush"))
            .expect("出厂快捷项应包含 Crush");
        assert_eq!(crush.label, "Crush");
        assert_eq!(crush.command, "crush --yolo");
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

        let config = load_launch_config_from_path(&path);
        let crush = config
            .entries
            .iter()
            .find(|entry| entry.provider.as_deref() == Some("crush"))
            .expect("当前版本的已有配置也应从注册表补入 Crush");
        assert_eq!(crush.command, "crush --yolo");

        let store = smelt_core::sqlite_state::open_sqlite_store(&path).unwrap();
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

        let config = load_launch_config_from_path(&path);
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
    fn builtin_launch_entries_are_fixed_but_custom_entries_can_be_removed() {
        let mut config = LaunchConfig::default();
        let builtin_count = config.entries.len();
        config.entries.push(LaunchEntry {
            label: "My command".into(),
            // 即使命令与内置项相同，provider=None 也明确表示这是用户自定义项。
            command: "claude --dangerously-skip-permissions".into(),
            provider: None,
        });

        assert!(!remove_launch_entry_at(&mut config, 0));
        assert_eq!(config.entries.len(), builtin_count + 1);
        assert!(remove_launch_entry_at(&mut config, builtin_count));
        assert_eq!(config.entries.len(), builtin_count);
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

        let config = load_launch_config_from_path(&path);
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

        let mut config = load_launch_config_from_path(&path);
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
        super::persist_launch_config_at(&path, &config);
        let reloaded = load_launch_config_from_path(&path);
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

        let config = load_launch_config_from_path(&path);
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

        let config = load_launch_config_from_path(&path);
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

        let config = load_launch_config_from_path(&path);
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

        let config = load_launch_config_from_path(&path);
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

        let mut config = load_launch_config_from_path(&path);
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
        super::persist_launch_config_at(&path, &config);
        let reloaded = load_launch_config_from_path(&path);
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

        let mut config = load_launch_config_from_path(&path);
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
        super::persist_launch_config_at(&path, &config);
        let reloaded = load_launch_config_from_path(&path);
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

        let mut config = load_launch_config_from_path(&path);
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
        super::persist_launch_config_at(&path, &config);
        let reloaded = load_launch_config_from_path(&path);
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

        let config = load_launch_config_from_path(&path);
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

        let config = load_launch_config_from_path(&path);
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

    #[test]
    fn antigravity_hook_merge_is_idempotent_and_preserves_other_hooks() {
        let mut root = serde_json::json!({
            "third-party": {
                "Stop": [{"type":"command","command":"/tmp/other-stop-hook"}]
            },
            "smelt-status-notifications": {
                "PostInvocation": [
                    {"type":"command","command":"/tmp/other-invocation-hook"}
                ]
            }
        });
        merge_antigravity_hooks(&mut root).unwrap();
        merge_antigravity_hooks(&mut root).unwrap();

        let definition = &root[ANTIGRAVITY_HOOK_NAME];
        for event in ANTIGRAVITY_HOOK_EVENTS {
            assert!(antigravity_event_installed(definition, event));
            assert_eq!(smelt_handler_count(definition, event), 1);
        }
        assert_eq!(
            definition["PostInvocation"][0]["command"],
            "/tmp/other-invocation-hook"
        );
        assert_eq!(
            root["third-party"]["Stop"][0]["command"],
            "/tmp/other-stop-hook"
        );
    }

    #[test]
    fn antigravity_hook_uninstall_removes_only_smelt_handlers() {
        let dir = temp_dir("antigravity-hook-uninstall");
        let path = dir.join("hooks.json");
        let mut root = serde_json::json!({
            "third-party": {
                "Stop": [{"type":"command","command":"/tmp/other-stop-hook"}]
            },
            "smelt-status-notifications": {
                "PostInvocation": [
                    {"type":"command","command":"/tmp/other-invocation-hook"}
                ]
            }
        });
        merge_antigravity_hooks(&mut root).unwrap();
        write_json_atomic(&path, &root).unwrap();

        uninstall_antigravity_hooks_at(&path).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let definition = &value[ANTIGRAVITY_HOOK_NAME];
        for event in ANTIGRAVITY_HOOK_EVENTS {
            assert_eq!(smelt_handler_count(definition, event), 0);
        }
        assert_eq!(
            definition["PostInvocation"][0]["command"],
            "/tmp/other-invocation-hook"
        );
        assert_eq!(
            value["third-party"]["Stop"][0]["command"],
            "/tmp/other-stop-hook"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// 下发给移动端的配色快照必须跟 PC 实际渲染色同源——这里锁住两件事：
/// 快照读的就是当前主题色（切浅色能跟着变、用户自选底色能盖掉主题色），以及
/// `smelt-core` 侧那份「守护读不到快照文件时」的默认值没跟深色主题漂移。
#[cfg(test)]
mod terminal_theme_tests {
    use super::current_terminal_theme;
    use crate::terminal;
    use smelt_core::terminal_theme::TerminalThemeSnapshot;

    #[test]
    fn dark_snapshot_matches_core_fallback() {
        let _guard = terminal::lock_theme_globals();
        terminal::set_dark_mode(true);
        crate::ui_theme::set_light(false);
        terminal::set_bg_override(None);
        assert_eq!(
            current_terminal_theme(),
            TerminalThemeSnapshot::default(),
            "smelt-core 的默认快照要跟 PC 深色主题实际色值一致：守护先于 GUI 起来时用的就是它"
        );
    }

    #[test]
    fn snapshot_follows_theme_mode_and_user_background() {
        let _guard = terminal::lock_theme_globals();
        terminal::set_dark_mode(false);
        crate::ui_theme::set_light(true);
        terminal::set_bg_override(None);
        let light = current_terminal_theme();
        assert!(!light.dark);
        assert_eq!(light.background, crate::ui_theme::bg_stage());
        assert_ne!(
            light.palette,
            TerminalThemeSnapshot::default().palette,
            "浅色主题要发浅色板，不能原样发深色板"
        );

        terminal::set_bg_override(Some(0x00123456));
        assert_eq!(
            current_terminal_theme().background,
            0x00123456,
            "用户自选底色要盖过主题色"
        );

        terminal::set_bg_override(None);
        terminal::set_dark_mode(true);
        crate::ui_theme::set_light(false);
    }
}

#[cfg(test)]
mod native_dsh_model_settings_tests {
    use super::NativeDshModelSettings;

    /// `dsh-model-settings.mjs --action read` 的真实输出。dsh 的 settings.yaml
    /// 用 `baseURL`，而 serde 的 camelCase 规则会算出 `baseUrl`——键名对不上时
    /// 整个模型页会静默退回兜底文案，「配置」「添加」也只会弹解析错误。
    const READ_OUTPUT: &str = r#"{
        "settingsPath": "/home/u/.dsh/settings.yaml",
        "defaultModel": {
            "provider": "deepseek-official",
            "model": "deepseek-v4-flash",
            "reasoningEffort": "max"
        },
        "deepseek": {
            "baseURL": "",
            "apiKeyEnv": "DEEPSEEK_API_KEY",
            "reasoningEffort": "max"
        },
        "credentialConfigured": true,
        "customProviders": [
            {
                "id": "acme",
                "displayName": "Acme",
                "api": "openai-completions",
                "baseURL": "https://gateway.acme/v1",
                "apiKeyEnv": "ACME_API_KEY",
                "credentialConfigured": false,
                "models": [{"id": "m1", "name": "M1", "contextWindow": 128000, "maxTokens": 4096}]
            }
        ]
    }"#;

    #[test]
    fn reads_the_helper_payload() {
        let settings: NativeDshModelSettings = serde_json::from_str(READ_OUTPUT).expect("parse");
        assert_eq!(settings.default_model.provider, "deepseek-official");
        assert_eq!(settings.deepseek.api_key_env, "DEEPSEEK_API_KEY");
        assert_eq!(settings.deepseek.base_url, "");
        assert!(settings.credential_configured);
        let provider = settings.custom_providers.first().expect("custom provider");
        assert_eq!(provider.base_url, "https://gateway.acme/v1");
        assert_eq!(
            provider.models.first().expect("model").context_window,
            Some(128000)
        );
    }
}

#[cfg(test)]
mod pi_model_settings_integration_tests {
    use smelt_core::pi_model_settings::{
        PiCustomModel, PiCustomProvider, PiDefaultModelConfig, load_pi_model_settings_at,
        pi_settings_summary_at, remove_pi_custom_provider_at, save_pi_custom_provider_at,
        save_pi_default_model_at,
    };

    #[test]
    fn pi_model_settings_lifecycle_in_isolated_dir() {
        let temp_dir = std::env::temp_dir().join(format!("smelt-pi-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let temp = temp_dir.as_path();
        let initial = load_pi_model_settings_at(temp).expect("load initial");
        assert_eq!(initial.default_model.provider, "deepseek");
        assert_eq!(initial.default_model.model, "deepseek-chat");
        assert_eq!(initial.default_model.thinking_level, "medium");
        let environment_credential_configured =
            std::env::var("DEEPSEEK_API_KEY").is_ok_and(|key| !key.trim().is_empty());
        assert_eq!(
            initial.credential_configured,
            environment_credential_configured
        );
        assert!(initial.custom_providers.is_empty());

        let default_config = PiDefaultModelConfig {
            provider: "deepseek".to_string(),
            model: "deepseek-chat".to_string(),
            base_url: "https://api.deepseek.com".to_string(),
            thinking_level: "low".to_string(),
        };
        save_pi_default_model_at(temp, &default_config, Some("sk-official-key"))
            .expect("save default");

        let custom_provider = PiCustomProvider {
            id: "local-vllm".to_string(),
            display_name: "Local vLLM".to_string(),
            api: "openai-completions".to_string(),
            base_url: "http://localhost:8000/v1".to_string(),
            credential_configured: false,
            models: vec![PiCustomModel {
                id: "qwen-2.5-coder".to_string(),
                name: "Qwen 2.5 Coder 32B".to_string(),
                context_window: Some(65536),
                max_tokens: Some(4096),
            }],
        };
        save_pi_custom_provider_at(temp, &custom_provider, None, false).expect("save custom");

        let reloaded = load_pi_model_settings_at(temp).expect("reload");
        assert_eq!(reloaded.default_model.provider, "deepseek");
        assert_eq!(reloaded.default_model.base_url, "https://api.deepseek.com");
        assert_eq!(reloaded.default_model.thinking_level, "low");
        assert!(reloaded.credential_configured);
        assert_eq!(reloaded.custom_providers.len(), 1);
        assert_eq!(reloaded.custom_providers[0].id, "local-vllm");
        assert_eq!(reloaded.custom_providers[0].models[0].id, "qwen-2.5-coder");

        let summary = pi_settings_summary_at(temp).expect("summary");
        let dm = summary.default_model.expect("default model in summary");
        assert_eq!(dm.provider.as_deref(), Some("deepseek"));
        assert_eq!(dm.model.as_deref(), Some("deepseek-chat"));
        assert_eq!(dm.thinking_level.as_deref(), Some("low"));

        remove_pi_custom_provider_at(temp, "local-vllm").expect("remove custom");
        let after_removal = load_pi_model_settings_at(temp).expect("after remove");
        assert!(after_removal.custom_providers.is_empty());
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

/// 插件面板「更新」按钮拼出来的 spec。
///
/// 这里守的是一个静默失败：光传包名时 pnpm 认定"范围已满足"，退 0 却不升级，
/// 界面还照报成功。所以 spec 必须带上范围，包名的解析也不能被 scope 的 `@` 骗到。
#[cfg(test)]
mod plugin_spec_tests {
    use crate::settings::Workspace;

    #[test]
    fn strips_the_range_without_tripping_over_the_scope() {
        assert_eq!(
            Workspace::plugin_name_of("@smelt-ai/dsh-acp-rich@^0.1.7"),
            "@smelt-ai/dsh-acp-rich"
        );
        assert_eq!(
            Workspace::plugin_name_of("@smelt-ai/dsh-acp-rich"),
            "@smelt-ai/dsh-acp-rich"
        );
        assert_eq!(Workspace::plugin_name_of("yaml@latest"), "yaml");
    }
}

/// 自定义 provider 的凭据引用名。
///
/// 引用是"这把 key 存在哪"，不是配置项——两个 provider 填出同一个名字，后保存的
/// 那个就会把先保存的 key 顶掉。所以名字由 id 派生，不给人填。
#[cfg(test)]
mod credential_ref_tests {
    use crate::settings::Workspace;

    #[test]
    fn derives_a_name_that_says_which_provider_it_belongs_to() {
        assert_eq!(
            Workspace::custom_provider_credential_ref("", "acme-gateway", true),
            "ACME_GATEWAY_API_KEY"
        );
        assert_eq!(
            Workspace::custom_provider_credential_ref("", "modelsight", true),
            "MODELSIGHT_API_KEY"
        );
    }

    /// 已经配好的那条沿用原名：它下面存着真的 key，而 Smelt 读不回明文，改名就是
    /// 让用户重输一次。
    #[test]
    fn keeps_the_name_a_configured_provider_already_uses() {
        assert_eq!(
            Workspace::custom_provider_credential_ref("OPEN_API_KEY", "modelsight", true),
            "OPEN_API_KEY"
        );
        assert_eq!(
            Workspace::custom_provider_credential_ref("OPEN_API_KEY", "renamed", false),
            "OPEN_API_KEY"
        );
    }

    /// 无鉴权的端点不该在凭据表里占个空位。
    #[test]
    fn leaves_an_unauthenticated_endpoint_without_a_reference() {
        assert_eq!(
            Workspace::custom_provider_credential_ref("", "local-llama", false),
            ""
        );
    }
}
