//! 终端配色快照：PC 端当前**真正生效**的终端颜色，落到主库 `terminal_theme`
//! 作用域，给守护/网关（进程外，读不到 GUI 的全局态）转发给移动端。
//!
//! 为什么要有这层：TUI（Claude Code、bat、delta…）会用 OSC 11 查终端背景色来决定
//! 自己用哪档灰。这个查询由守护进程在收到 PTY 输出时立即应答，回的是 PC 当前主题色；
//! 新版 GUI 从握手能力位得知后不再重复应答，旧守护仍由 GUI 兼容兜底。移动端渲染的是
//! 同一份 PTY 字节流，若用自己写死的底色，
//! TUI 以为的底色和实际底色就对不上——直接是对比度问题。
//!
//! 所以颜色真源只有一个：PC 端主题（深浅色模式 + 用户在设置里自选的终端底色）。
//! 这里只负责把它序列化出来。PC 端在主题/外观变化时调 [`publish`]，网关在
//! `terminalReady` 里把 [`TerminalThemeSnapshot::to_wire`] 的结果发给手机。
//!
//! 一部手机可以连多台设备，每条终端连接各自带着**那台机器**的配色回来，所以移动端
//! 不能有任何全局写死的色板。

pub use smelt_model::terminal_theme::*;


/// 读当前快照；文档缺失/损坏回退 PC 默认深色。
pub fn load() -> TerminalThemeSnapshot {
    let Ok(store) = crate::sqlite_state::default_sqlite_store() else {
        return TerminalThemeSnapshot::default();
    };
    match store.get_terminal_theme_snapshot() {
        Ok(Some(snapshot)) => theme_from_store(snapshot).normalized(),
        Ok(None) | Err(_) => TerminalThemeSnapshot::default(),
    }
}

/// PC 端在主题模式 / 外观设置变化时调用，把当前生效配色落盘。
pub fn publish(snapshot: &TerminalThemeSnapshot) {
    if let Ok(store) = crate::sqlite_state::default_sqlite_store() {
        let _ = store.put_terminal_theme_snapshot(&theme_to_store(snapshot));
    }
}

fn theme_to_store(snapshot: &TerminalThemeSnapshot) -> smelt_store::TerminalThemeSnapshot {
    smelt_store::TerminalThemeSnapshot {
        version: snapshot.version,
        dark: snapshot.dark,
        background: snapshot.background,
        foreground: snapshot.foreground,
        cursor: snapshot.cursor,
        selection: snapshot.selection,
        palette: snapshot.palette.clone(),
        search_hit: snapshot.search_hit,
        search_hit_current: snapshot.search_hit_current,
    }
}

fn theme_from_store(snapshot: smelt_store::TerminalThemeSnapshot) -> TerminalThemeSnapshot {
    TerminalThemeSnapshot {
        version: snapshot.version,
        dark: snapshot.dark,
        background: snapshot.background,
        foreground: snapshot.foreground,
        cursor: snapshot.cursor,
        selection: snapshot.selection,
        palette: snapshot.palette,
        search_hit: snapshot.search_hit,
        search_hit_current: snapshot.search_hit_current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_uses_hex_strings_and_full_palette() {
        let wire = TerminalThemeSnapshot::default().to_wire();
        assert_eq!(wire["background"], "#0a0a0a");
        assert_eq!(wire["foreground"], "#d8d8d8");
        assert_eq!(wire["palette"].as_array().unwrap().len(), 16);
        assert_eq!(wire["palette"][0], "#15161e");
        assert_eq!(wire["palette"][15], "#ffffff");
        assert_eq!(wire["dark"], true);
    }

    /// 短色板（老版本快照）补齐到 16 色，渲染侧永远能按 index 取到颜色。
    #[test]
    fn short_palette_is_padded_with_defaults() {
        let snapshot = TerminalThemeSnapshot {
            palette: vec![0x000000, 0x111111],
            ..Default::default()
        }
        .normalized();
        assert_eq!(snapshot.palette.len(), 16);
        assert_eq!(snapshot.palette[0], 0x000000);
        assert_eq!(
            snapshot.palette[2],
            TerminalThemeSnapshot::default().palette[2]
        );
    }

    #[test]
    fn json_round_trips() {
        let snapshot = TerminalThemeSnapshot {
            dark: false,
            background: 0xffffff,
            ..Default::default()
        };
        let text = serde_json::to_string(&snapshot).unwrap();
        let back: TerminalThemeSnapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(back, snapshot);
    }
}
