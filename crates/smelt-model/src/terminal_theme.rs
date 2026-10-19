//! 桌面发布给移动端的终端配色快照。存取在运行时。

use serde::{Deserialize, Serialize};

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
            background: 0x0a0a0a,
            foreground: 0xd8d8d8,
            cursor: 0xd8d8d8,
            selection: 0x0c3d7a,
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
    pub fn normalized(mut self) -> Self {
        let fallback = Self::default().palette;
        if self.palette.len() < fallback.len() {
            self.palette
                .extend_from_slice(&fallback[self.palette.len()..]);
        }
        self.palette.truncate(fallback.len());
        self
    }
}
