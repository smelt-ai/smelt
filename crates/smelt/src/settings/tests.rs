use super::*;

#[cfg(test)]
mod appearance_tests {
    use super::{Appearance, ThemeMode, liquid_glass};
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

    /// 外观存进客户端库再读回来，枚举字段按名字往返不丢。
    #[test]
    fn appearance_survives_the_snapshot_round_trip() {
        let appearance = Appearance {
            glass_style: liquid_glass::GlassStyle::Clear,
            theme_mode: ThemeMode::Light,
            theme_name: "Ayu Light".to_string(),
            ui_font_px: 18,
            bg_image: Some("/tmp/bg.png".to_string()),
            ..Appearance::default()
        };
        let snapshot = super::snapshot_from_appearance(&appearance);
        assert_eq!(snapshot.glass_style, "clear");
        assert_eq!(snapshot.theme_mode, "light");
        let back = super::appearance_from_snapshot(snapshot);
        assert_eq!(back.glass_style, liquid_glass::GlassStyle::Clear);
        assert_eq!(back.theme_mode, ThemeMode::Light);
        assert_eq!(back.theme_name, "Ayu Light");
        assert_eq!(back.ui_font_px, 18);
        assert_eq!(back.bg_image.as_deref(), Some("/tmp/bg.png"));
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
mod pairing_qr_tests {
    use super::qr_png_for_url;

    #[test]
    fn pairing_uri_renders_to_a_qr() {
        // endpoint id 有 64 个十六进制字符，钉住「配对串确实能编成二维码」：
        // 超长内容会让 QrCode::new 失败。
        let uri = smelt_pairing::pairing_uri(
            &"a".repeat(64),
            "ABCD-EFGH",
            Some("https://relay.example.test"),
        );
        let png = qr_png_for_url(&uri).expect("配对串必须能生成二维码");
        assert!(!png.is_empty());
    }
}

#[cfg(test)]
mod daemon_info_tests {
    use super::{acp_cmd_setting_value, daemon_info_line, fmt_uptime};
    use crate::terminal::DaemonInfo;
    use smelt_model::agent_kind::ConversationAgentKind;

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
}

#[cfg(test)]
mod antigravity_terminal_tests {
    use super::{LaunchConfig, LaunchEntry, default_launch_entries, remove_launch_entry_at};

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
}

/// 下发给移动端的配色快照必须跟 PC 实际渲染色同源——这里锁住两件事：
/// 快照读的就是当前主题色（切浅色能跟着变、用户自选底色能盖掉主题色），以及
/// `smelt-core` 侧那份「守护读不到快照文件时」的默认值没跟深色主题漂移。
#[cfg(test)]
mod terminal_theme_tests {
    use super::current_terminal_theme;
    use crate::terminal;
    use smelt_model::terminal_theme::TerminalThemeSnapshot;

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
