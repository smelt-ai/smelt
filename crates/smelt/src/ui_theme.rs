//! 设计稿语义色板（仅深色）——全局 UI 颜色的唯一出处。
//!
//! 色值来自 claude.ai/design「桌面开发者客户端设计」的 Desktop Client 定稿。
//! 布局各层（项目 rail / 会话列表 / 会话舞台 / inspector / 状态栏 / toast）统一从
//! 这里取色，别再在各处写裸 `rgb(0x...)`。浅色方案等有对应设计稿再补，
//! 本版全局强制深色（见 main() 初始化处）。

// 改版分阶段落地，常量与 helper 会陆续被各阶段启用；收尾阶段拿掉这行。
#![allow(dead_code)]

use std::hash::{Hash, Hasher};

use gpui::{rgb, rgba, Rgba};

use crate::AgentStatus;

// ---- 底色（由深到浅的表面层级） ----
/// 项目 rail / inspector 图标条（最深）。
pub const BG_RAIL: u32 = 0x0a0b0e;
/// 会话舞台底。
pub const BG_PANEL: u32 = 0x0f1116;
/// 会话列表 / inspector 面板底。
pub const BG_ELEV: u32 = 0x12141a;
/// 标题栏底。
pub const BG_BAR: u32 = 0x14161b;
/// 卡片 / 胶囊底。
pub const BG_CARD: u32 = 0x171a20;
/// hover / toast / 输入胶囊底。
pub const BG_HOVER: u32 = 0x1a1d24;
/// 选中行底（会话行选中、文件行选中）。
pub const BG_SELECTED: u32 = 0x20232b;
/// 状态栏 / 终端底条。
pub const BG_STATUS: u32 = 0x0c0d11;

// ---- 边色（由暗到亮） ----
/// 大区块分界线（列与列之间）。
pub const BORDER_DIM: u32 = 0x1c1f27;
/// 标题栏下沿。
pub const BORDER: u32 = 0x23262e;
/// 卡片 / 胶囊描边。
pub const BORDER_MID: u32 = 0x262a33;
/// 输入框 / 虚线块描边。
pub const BORDER_LOUD: u32 = 0x2a2e37;
/// hover / 焦点描边。
pub const BORDER_FOCUS: u32 = 0x3a3f4a;
/// 选中行描边。
pub const BORDER_SELECTED: u32 = 0x33373f;

// ---- 文字（由亮到暗） ----
/// 标题 / 强调正文。
pub const TEXT_BRIGHT: u32 = 0xe6e8ec;
/// 正文。
pub const TEXT: u32 = 0xd7dae0;
/// 次级正文 / 按钮文字。
pub const TEXT_MID: u32 = 0xc2c6cf;
/// 弱化说明 / mono 副标题。
pub const TEXT_MUTED: u32 = 0x8b909c;
/// 最弱（占位、时间戳、快捷键提示）。
pub const TEXT_FAINT: u32 = 0x5a606c;

// ---- 语义色 ----
/// 主强调橙（品牌色：进行中、主按钮、激活态）。
pub const ACCENT: u32 = 0xd98a4f;
/// 绿：运行正常 / 通过 / diff 新增侧。
pub const GREEN: u32 = 0x66bb8a;
/// 黄：阻塞 / 等审批。
pub const YELLOW: u32 = 0xfebc2e;
/// 蓝：链接 / 读类工具 / queued。
pub const BLUE: u32 = 0x6ea8fe;
/// 紫：agent 会话标识 / 模型胶囊。
pub const PURPLE: u32 = 0xb98be0;
/// 红：删除侧 / 拒绝 / 等审批（最高优先级状态）。
pub const RED: u32 = 0xe0736e;
/// diff 新增行的文字色（比 GREEN 更亮一档，用于深绿底上）。
pub const DIFF_ADD_TEXT: u32 = 0x8fd6ac;

/// 深色底上的按钮反色文字（橙/绿/黄实心按钮上的深字）。
pub const ON_ACCENT: u32 = 0x12141a;

/// 给纯色叠低透明度，用于角标底、激活态背景等衍生色。
/// `alpha` 0–255；`tint(ACCENT, 0x22)` ≈ 设计稿的 rgba(217,138,79,.13)。
pub fn tint(color: u32, alpha: u8) -> Rgba {
    rgba((color << 8) | alpha as u32)
}

/// Agent 五态状态色（等审批红 > 需处理黄 > 运行蓝 > 完成绿 > 空闲灰）。
/// 收敛自旧版散落的 0xef4444/0xf59e0b/0x4a9eff/0x22c55e 硬编码。
pub fn agent_status_color(status: AgentStatus) -> Rgba {
    match status {
        AgentStatus::WaitingApproval => rgb(RED),
        AgentStatus::NeedsAttention => rgb(YELLOW),
        AgentStatus::Running => rgb(BLUE),
        AgentStatus::Done => rgb(GREEN),
        AgentStatus::Idle => rgb(TEXT_FAINT),
    }
}

/// 同一套状态色的 (r, g, b) 形态，给 mac 菜单栏（status_item）用。
pub fn agent_status_rgb8(status: AgentStatus) -> (u8, u8, u8) {
    let c = match status {
        AgentStatus::WaitingApproval => RED,
        AgentStatus::NeedsAttention => YELLOW,
        AgentStatus::Running => BLUE,
        AgentStatus::Done => GREEN,
        AgentStatus::Idle => TEXT_FAINT,
    };
    ((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// 设计稿三态点（项目 rail / 会话行）：跑着 = 绿、阻塞 = 黄、空闲 = 灰。
/// 与五态 `agent_status_color` 是两套语义，别硬凑：这里「Running/Done」都算
/// 「活着且正常」（绿），两种等待都算「阻塞」（黄）。
pub fn session_dot_color(status: AgentStatus) -> Rgba {
    match status {
        AgentStatus::WaitingApproval | AgentStatus::NeedsAttention => rgb(YELLOW),
        AgentStatus::Running | AgentStatus::Done => rgb(GREEN),
        AgentStatus::Idle => rgb(TEXT_FAINT),
    }
}

/// 项目名 → 稳定颜色：hash 到 6 色环，跟会话顺序无关。
pub fn project_color(name: &str) -> Rgba {
    const RING: [u32; 6] = [ACCENT, GREEN, BLUE, PURPLE, YELLOW, RED];
    let mut h = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut h);
    rgb(RING[(h.finish() % RING.len() as u64) as usize])
}
