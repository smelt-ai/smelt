//! 终端配色快照：PC 端当前**真正生效**的终端颜色，落一份到 `~/.smelt/terminal-theme.json`，
//! 给守护/网关（进程外，读不到 GUI 的全局态）转发给移动端。
//!
//! 为什么要有这层：TUI（Claude Code、bat、delta…）会用 OSC 11 查终端背景色来决定
//! 自己用哪档灰。这个查询由 PC 端的终端应答（`smelt::terminal::EventProxy::resolve_color`），
//! 回的是 PC 当前主题色。移动端渲染的是同一份 PTY 字节流，若用自己写死的底色，
//! TUI 以为的底色和实际底色就对不上——直接是对比度问题。
//!
//! 所以颜色真源只有一个：PC 端主题（深浅色模式 + 用户在设置里自选的终端底色）。
//! 这里只负责把它序列化出来。PC 端在主题/外观变化时调 [`publish`]，网关在
//! `terminalReady` 里把 [`TerminalThemeSnapshot::to_wire`] 的结果发给手机。
//!
//! 一部手机可以连多台设备，每条终端连接各自带着**那台机器**的配色回来，所以移动端
//! 不能有任何全局写死的色板。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 线协议版本：字段只增不改，移动端按缺字段回退即可，这里主要用于排查。
pub const TERMINAL_THEME_VERSION: u32 = 1;

/// 一套终端配色（0xRRGGBB）。字段与 `smelt::terminal` / `smelt::terminal_view`
/// 里实际渲染用的色位一一对应。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalThemeSnapshot {
    pub version: u32,
    /// 深色模式；移动端拿它决定周边 chrome（状态栏图标等）的明暗。
    pub dark: bool,
    /// 默认背景色，也是 OSC 11 查询的应答值（含用户自选底色）。
    pub background: u32,
    /// 默认前景色（OSC 10）。
    pub foreground: u32,
    pub cursor: u32,
    pub selection: u32,
    /// ANSI 16 色。
    pub palette: Vec<u32>,
    pub search_hit: u32,
    pub search_hit_current: u32,
}

impl Default for TerminalThemeSnapshot {
    /// PC 深色主题的出厂值。快照文件缺失（守护先于 GUI 起、或从没开过 GUI）时用它，
    /// 与 PC 默认外观一致——`smelt` 侧有单测锁住这份默认值不跟实际渲染色漂移。
    fn default() -> Self {
        Self {
            version: TERMINAL_THEME_VERSION,
            dark: true,
            background: 0x313338,
            foreground: 0xd8d8d8,
            cursor: 0xd8d8d8,
            selection: 0x334a6a,
            palette: vec![
                0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc7c7c7,
                0x2c3149, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xffffff,
            ],
            search_hit: 0x7a5c20,
            search_hit_current: 0xd4a017,
        }
    }
}

impl TerminalThemeSnapshot {
    /// 发给移动端的 JSON：颜色写成 `#rrggbb` 字符串，客户端不必关心整数字节序。
    pub fn to_wire(&self) -> serde_json::Value {
        let hex = |c: u32| format!("#{:06x}", c & 0x00ff_ffff);
        serde_json::json!({
            "version": self.version,
            "dark": self.dark,
            "background": hex(self.background),
            "foreground": hex(self.foreground),
            "cursor": hex(self.cursor),
            "selection": hex(self.selection),
            "palette": self.palette.iter().copied().map(hex).collect::<Vec<_>>(),
            "searchHit": hex(self.search_hit),
            "searchHitCurrent": hex(self.search_hit_current),
        })
    }

    /// 色板短了就用默认色补齐：老版本快照 / 手写坏文件都不该让渲染缺色。
    fn normalized(mut self) -> Self {
        let fallback = Self::default().palette;
        if self.palette.len() < fallback.len() {
            self.palette
                .extend_from_slice(&fallback[self.palette.len()..]);
        }
        self.palette.truncate(fallback.len());
        self
    }
}

fn theme_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".smelt").join("terminal-theme.json"))
}

/// 读当前快照；文件缺失/损坏回退 PC 默认深色。
pub fn load() -> TerminalThemeSnapshot {
    crate::json_store::load_json::<TerminalThemeSnapshot>(theme_path()).normalized()
}

/// PC 端在主题模式 / 外观设置变化时调用，把当前生效配色落盘。
pub fn publish(snapshot: &TerminalThemeSnapshot) {
    crate::json_store::save_json(theme_path(), snapshot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_uses_hex_strings_and_full_palette() {
        let wire = TerminalThemeSnapshot::default().to_wire();
        assert_eq!(wire["background"], "#313338");
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
