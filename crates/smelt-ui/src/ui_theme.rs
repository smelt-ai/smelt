//! 语义色板（深色 / 浅色两套）——全局 UI 颜色的唯一出处。
//!
//! 视觉语言整体跟本机 **Grok Bot**（`/Applications/Grok Bot.app`）的 `sand`：
//! 色值、实色表面、近白/近黑主按钮、胶囊 composer。窗口底和舞台底 `#070707` /
//! 会话列表 `#111111` / 卡片 `#181818`，交互强调是它的链接蓝 `#1084fe`；
//! 主操作（发送钮）走近白/近黑实心，不走强调蓝。浅色对应 `sand` light
//! （舞台底 `#fcfcfc`）。
//!
//! 上一版色值来自 Discord，材质一度走过 Telegram 毛玻璃。换成 Grok Bot
//! 是整体换语言，不是调参：**深色表面层级的方向反过来了**（见 DARK 的注释），
//! 内容区用实底，不要再给卡片叠半透明。
//!
//! 布局各层（项目栏 / 会话栏 / 舞台 / 工具栏 / 底栏 / 状态栏 / diff）
//! 统一从这里取色，别再在各处写裸 `rgb(0x...)`——写死的深色在浅色模式下会花。
//!
//! 用法：全是**函数**不是常量（`ui_theme::bg_rail()`），因为色值要跟着
//! `set_light()` 在运行时切换。切换点见 settings 模块的「外观」设置开关和 main() 初始化，
//! 切完必须 `cx.refresh_windows()` 才会重绘。

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Anchor, Pixels, Rgba, px, rgb, rgba, transparent_black};

use smelt_core::agent_status::AgentStatus;

/// 一套完整语义色板。字段即语义位，深浅两套必须一一对应填满。
pub struct Palette {
    // ---- 底色（表面层级） ----
    /// 项目 rail / Tool Panel 图标条（深色下最深，浅色下最灰）。
    pub bg_rail: u32,
    /// 舞台底：中间会话区（终端 / ACP 对话）。
    pub bg_stage: u32,
    /// 左右栏底：会话栏和工具栏共用的次表面。
    pub bg_column: u32,
    /// 标题栏底。
    pub bg_bar: u32,
    /// 卡片 / 胶囊底。
    pub bg_card: u32,
    /// hover / 输入胶囊底。
    pub bg_hover: u32,
    /// **列表行**专用 hover 底：必须明显弱于 `bg_selected`，否则「鼠标划过的行」
    /// 和「当前选中的行」同时是两块灰，一眼分不出哪个才是当前。
    /// 不能直接复用 `bg_hover`——那个还给按钮 / 输入胶囊用，压淡了控件就没手感。
    pub bg_row_hover: u32,
    /// 选中行底（会话行选中、文件行选中）。
    pub bg_selected: u32,
    /// 状态栏 / 终端底条。
    pub bg_status: u32,

    // ---- 边色（对比由弱到强） ----
    /// 大区块分界线（列与列之间）。
    pub border_dim: u32,
    /// 标题栏下沿。
    pub border: u32,
    /// 卡片 / 胶囊描边。
    pub border_mid: u32,
    /// 输入框 / 虚线块描边。
    pub border_loud: u32,
    /// hover / 焦点描边。
    pub border_focus: u32,
    // ---- 文字（由强到弱） ----
    /// 标题 / 强调正文。
    pub text_bright: u32,
    /// 正文。
    pub text: u32,
    /// 次级正文 / 按钮文字。
    pub text_mid: u32,
    /// 弱化说明 / mono 副标题。
    pub text_muted: u32,
    /// 最弱（占位、时间戳、快捷键提示）。
    pub text_faint: u32,

    // ---- 语义色 ----
    /// 交互强调（选中描边 / 焦点 / 链接）。Grok Bot 的 `fill/accent`，不是主按钮填色。
    pub accent: u32,
    /// 绿：运行正常 / 通过 / diff 新增侧。
    pub green: u32,
    /// 黄：警告 / 额度将满，不是 agent 状态。
    pub yellow: u32,
    /// 蓝：链接 / 读类工具 / queued。
    pub blue: u32,
    /// 紫：agent 会话标识 / 模型胶囊。
    pub purple: u32,
    /// 红：要你 / 拒绝 / diff 删除侧。
    pub red: u32,
    /// diff 新增行的文字色（深色下比 green 亮一档用于深绿底；浅色下反之压深）。
    pub diff_add_text: u32,
    /// 实心彩色按钮（橙/绿/黄底）上的反色文字。
    pub on_accent: u32,

    // ---- diff 视图（git_panel 并排/统一视图） ----
    /// 新增行：前景 / 整行底 / 左色条 / 行内变化片段的加深底。
    pub diff_add_fg: u32,
    pub diff_add_bg: u32,
    pub diff_add_bar: u32,
    pub diff_add_hl: u32,
    /// 删除行：同上四件套。
    pub diff_del_fg: u32,
    pub diff_del_bg: u32,
    pub diff_del_bar: u32,
    pub diff_del_hl: u32,
    /// 上下文行前景。
    pub diff_ctx_fg: u32,
    /// hunk 头前景。
    pub diff_hunk_fg: u32,
    /// diff 元信息行（文件头等）前景。
    pub diff_meta_fg: u32,
    /// 并排视图里「此侧无对应行」的空白底。
    pub diff_empty_bg: u32,
    /// 并排视图中缝分隔线。
    pub diff_gutter: u32,
}

/// 深色（Grok Bot `sand-dark`）。
///
/// **表面层级跟 Discord 那版相反，别顺手改回去**：Grok Bot 是窗口底和舞台同色近黑，
/// 会话列表和卡片往上抬。上一版 Discord 靠「舞台最亮、边缘压暗」把注意力推向中间；
/// 两种都自洽，混用就既不像 Grok 也不像 Discord。
pub const DARK: Palette = Palette {
    bg_rail: 0x070707,
    bg_stage: 0x070707,
    bg_column: 0x111111,
    bg_bar: 0x111111,
    bg_card: 0x181818,
    bg_hover: 0x262626,
    // 只比 bg_column(#111) 抬一档：划过是「浮起一点」，选中才明显亮起来。
    bg_row_hover: 0x151515,
    bg_selected: 0x262626,
    bg_status: 0x070707,

    border_dim: 0x151515,
    border: 0x181818,
    border_mid: 0x262626,
    border_loud: 0x3d3d3d,
    // Grok Bot `border/focus` 深色：#1c8bfe。
    border_focus: 0x1c8bfe,
    // 选中描边走强调蓝，不用「灰上加灰」。
    text_bright: 0xfcfcfc,
    text: 0xf3f3f3,
    text_mid: 0xb7b7b7,
    text_muted: 0x959595,
    text_faint: 0x777777,

    // 强调蓝是 Grok Bot 的 `fill/accent`（sand blue/9）。主按钮不走它，
    // 见 `action_fill`：深色近白、浅色近黑。
    accent: 0x1084fe,
    // 完成绿要偏黄、少蓝。sand success `#00c972` 的 B=114，是薄荷绿，
    // 小图标上看着跟运行青是同一个色。
    green: 0x22c55e,
    yellow: 0xff9800,
    // 运行必须是「蓝通道压过绿」的蓝。`#459ffe` 的 G=159，15px 空心图标会收成青，
    // 和完成绿撞车。`#3b82f6` 仍跟强调蓝 `#1084fe` 同族，但绿通道更低。
    blue: 0x3b82f6,
    purple: 0x9159fe,
    red: 0xff263c,
    diff_add_text: 0x78e2b4,
    on_accent: 0xfcfcfc,

    diff_add_fg: 0x78e2b4,
    diff_add_bg: 0x001c10,
    diff_add_bar: 0x00c972,
    diff_add_hl: 0x004024,
    diff_del_fg: 0xff8c98,
    diff_del_bg: 0x240508,
    diff_del_bar: 0xff263c,
    diff_del_hl: 0x520c13,
    diff_ctx_fg: 0xb7b7b7,
    diff_hunk_fg: 0x459ffe,
    diff_meta_fg: 0x5a5a5a,
    diff_empty_bg: 0x070707,
    diff_gutter: 0x181818,
};

/// 浅色（Grok Bot `sand` light）。
///
/// 两条别顺手改平：
/// - 浅色舞台底是近白 `#fcfcfc`（最亮），会话列表和卡片略压暗才能浮起来。
/// - 强调蓝深浅两套用同一个值 `#1084fe`：它是品牌交互色，跟着模式变色就不是
///   那个牌子了；其余语义色（绿/黄/红/青/紫）照例压深一档，否则近白底上对比不足。
pub const LIGHT: Palette = Palette {
    bg_rail: 0xeeeeee,
    bg_stage: 0xfcfcfc,
    bg_column: 0xf7f7f7,
    bg_bar: 0xf7f7f7,
    bg_card: 0xf3f3f3,
    bg_hover: 0xe8e8e8,
    // 浅色下同理：划过只比 bg_column 压深一点，选中才明显。
    bg_row_hover: 0xeeeeee,
    bg_selected: 0xe8e8e8,
    bg_status: 0xeeeeee,

    border_dim: 0xe8e8e8,
    border: 0xe8e8e8,
    border_mid: 0xd5d5d5,
    border_loud: 0xb7b7b7,
    border_focus: 0x0c64c1,

    text_bright: 0x141414,
    text: 0x141414,
    text_mid: 0x3d3d3d,
    text_muted: 0x5a5a5a,
    text_faint: 0x777777,

    accent: 0x1084fe,
    green: 0x16a34a,
    yellow: 0xc27400,
    blue: 0x2563eb,
    purple: 0x6e44c1,
    red: 0xc21d2e,
    diff_add_text: 0x00673a,
    on_accent: 0xfcfcfc,

    diff_add_fg: 0x00673a,
    diff_add_bg: 0xe8faf2,
    diff_add_bar: 0x00c972,
    diff_add_hl: 0xb0eed3,
    diff_del_fg: 0xc21d2e,
    diff_del_bg: 0xffebed,
    diff_del_bar: 0xff263c,
    diff_del_hl: 0xffbcc3,
    diff_ctx_fg: 0x3d3d3d,
    diff_hunk_fg: 0x0c64c1,
    diff_meta_fg: 0x959595,
    diff_empty_bg: 0xf7f7f7,
    diff_gutter: 0xe8e8e8,
};

/// 当前是不是浅色。进程级全局态：色板要在任意渲染函数里同步读到，
/// 走 GPUI 的 Global 就得把 `&App` 一路传进每个取色点，代价不成比例
/// （跟 terminal.rs 的 `DARK_MODE` 是同一路数）。
static LIGHT_MODE: AtomicBool = AtomicBool::new(false);

/// 切换色板。调用方切完必须 `cx.refresh_windows()`，否则已绘制的界面不会更新。
pub fn set_light(light: bool) {
    LIGHT_MODE.store(light, Ordering::Relaxed);
}

pub fn is_light() -> bool {
    LIGHT_MODE.load(Ordering::Relaxed)
}

/// 把当前色板按语义位灌进 gpui-component 的主题，让组件（Input / Button /
/// Menu / 设置页 / 表格…）跟自绘部分同色。
///
/// 背景：全库有一百多处直接读 `cx.theme().border` 这类组件库色位，和自绘部分读
/// 的 `ui_theme::*` 是两套独立色值，同屏挨着就差一档。与其改掉那一百多处调用，
/// 不如在这里把组件库主题按语义对齐——色真源仍然只有本文件一个。
///
/// 只覆写「表面 / 边 / 文字 / 主按钮」这些会跟自绘部分并排出现的位；组件内部
/// 的细分态（各种 button_success_hover 之类）交给组件库自己推导，别越俎代庖。
pub fn apply_to_component_theme(cx: &mut gpui::App) {
    use gpui_component::{ActiveTheme as _, Theme};

    if !cx.has_global::<Theme>() {
        return;
    }
    let p = palette();
    let is_dark = cx.theme().mode.is_dark();
    let c = &mut cx.global_mut::<Theme>().colors;

    c.background = rgb(p.bg_stage).into();
    c.foreground = rgb(p.text).into();
    // Markdown 表格/分割线、ACP 卡片描边都读 `theme.border`。写成透明后这些
    // 声明全部失效，表格会糊成两列飘字。跟 `border_mid()` 对齐。
    c.border = rgb(p.border_mid).into();
    c.muted = rgb(p.bg_card).into();
    c.muted_foreground = rgb(p.text_muted).into();
    c.popover = rgb(p.bg_card).into();
    c.popover_foreground = rgb(p.text).into();
    c.input = rgb(p.border_loud).into();
    c.ring = rgb(p.accent).into();
    c.selection = rgba((p.accent << 8) | 0x55).into();
    c.drop_target = rgba((p.accent << 8) | 0x33).into();

    // 主按钮走 Grok Bot：深色近白底 + 深字，浅色近黑底 + 浅字。强调蓝只留给
    // 选中/链接，不再当实心 CTA，否则和对话里的白圆钮打架。
    c.primary = rgb(p.text_bright).into();
    c.primary_hover = rgb(shade(p.text_bright, if is_dark { 0.9 } else { 1.18 })).into();
    c.primary_active = rgb(shade(p.text_bright, if is_dark { 0.82 } else { 0.7 })).into();
    c.primary_foreground = rgb(p.bg_stage).into();
    // Slider：tokens 派生时 slider_bar/slider_thumb 没配置会 fallback 到
    // primary——拖动 active 时整条轨道铺成主色，全屏下非常刺眼。
    // 收敛成中性选中表面色 + 亮手柄，跟拖拽调整布局那条 drag_border 一个思路：
    // 拖拽反馈用主题色，不用组件默认的亮强调色。
    c.slider_bar = rgb(p.bg_selected).into();
    c.slider_thumb = rgb(p.text_bright).into();
    c.secondary = rgb(p.bg_card).into();
    c.secondary_hover = rgb(p.bg_hover).into();
    c.secondary_active = rgb(p.bg_selected).into();
    c.secondary_foreground = rgb(p.text).into();
    // 行内 code 的底色改在 markdown_view 里用 bg_selected 实色设，不再借 accent。
    // accent 仍给菜单/列表 hover：半透明选中表面，避免整块发灰。
    c.accent = rgba((p.bg_selected << 8) | 0xaa).into();
    c.accent_foreground = rgb(p.text_bright).into();
    c.danger = rgb(p.red).into();
    c.danger_foreground = rgb(p.on_accent).into();
    // Markdown 链接读 `theme.link*`，不覆写的话深色默认是近白，和正文分不开。
    c.link = rgb(p.blue).into();
    c.link_hover = rgb(shade(p.blue, if is_dark { 1.14 } else { 0.88 })).into();
    c.link_active = rgb(shade(p.blue, if is_dark { 0.88 } else { 0.78 })).into();
    c.warning = rgb(p.yellow).into();
    c.warning_foreground = rgb(p.yellow).into();
    c.info = rgb(p.blue).into();
    c.info_foreground = rgb(p.blue).into();
    // Tab（tool_panel.rs 的 Files / Git / History 与插件 tab 用的 gpui_component::tab::TabBar）：
    // 库自带的 tab_foreground/tab_active_foreground 是内置深色主题的固定值，
    // 不跟着上面这些 palette 覆写走，跟其余全用 ui_theme 色板的界面挨在一起
    // 会有点"格格不入"（灰阶、字重都对不上）。这里收敛成跟舞台头标题同一套
    // text_muted/text_bright。
    c.tab_foreground = rgb(p.text_muted).into();
    c.tab_active_foreground = rgb(p.text_bright).into();
    c.tab_bar = c.background;
    c.tab = transparent_black();
    c.tab_active = transparent_black();

    // 列表 / 侧栏：会话列表与文件树都在这一层，必须跟自绘的行底同色。
    c.list = rgb(p.bg_column).into();
    c.list_hover = rgb(p.bg_hover).into();
    c.list_active = rgb(p.bg_selected).into();
    c.list_active_border = rgb(p.accent).into();
    c.list_even = rgb(p.bg_column).into();
    c.list_head = rgb(p.bg_bar).into();
    c.sidebar = rgb(p.bg_column).into();
    // 设置页侧栏的 active 行读的是 sidebar_accent（不是 list_active）。
    // 如果只覆写 sidebar，默认深色主题的 #262626 会和 bg_column 几乎融在一起，
    // 看起来就像当前页没有选中。
    c.sidebar_accent = rgb(p.bg_selected).into();
    c.sidebar_accent_foreground = rgb(p.text_bright).into();
    c.sidebar_border = rgb(p.border_dim).into();
    c.sidebar_foreground = rgb(p.text).into();
    // 滚动条：组件库缺省 thumb 会落到 accent（Grok 强调蓝 `#1084fe`），
    // 侧栏/历史这种长列表里整条亮蓝非常扎眼。轨道保持透明，thumb 用中性描边灰。
    c.scrollbar = transparent_black();
    c.scrollbar_thumb = rgb(p.border_loud).into();
    c.scrollbar_thumb_hover = rgb(p.text_faint).into();

    // colors 改完必须同步重算 tokens：组件内部有些位读的是 `tokens.*` 而不是
    // `colors.*`（如 checkbox 勾选态的方块填充用 tokens.primary）。不同步的话
    // 填充还是组件库默认的浅色，和被我们改成白的对勾（primary_foreground）撞成
    // 「白底 + 白对勾」——勾选了却看不见勾，就是这个回归。
    let theme = cx.global_mut::<Theme>();
    theme.tokens = (&theme.colors).into();
    // 应用内通知固定在右下角，避开左侧高频操作的会话列表；主题切换会重置通知配置，
    // 所以必须在每次 Theme::change 之后和色板一起重新覆盖。
    theme.notification.placement = Anchor::BottomRight;
    let radius = theme.radius;

    // 分栏拖拽条读的是 gpui-base 的 `resizable.handle`，不是 `Theme.colors.border`。
    // Theme::change 会把手柄设成当时的 `theme.border`；我们随后覆写 colors.border
    // 却不同步这一位的话，手柄就停在组件库默认——Default 是透明，1px 缝里露出
    // 壳底 `#070707`，看起来像一条黑槽。发丝画在手柄上，不要靠缝透底。
    // 滚动条同理：Theme::change 已经把旧 thumb 色烤进 gpui-base，只改 colors
    // 不够，必须把中性灰重新投影上去。
    if cx.has_global::<gpui_base::Theme>() {
        let hairline: gpui::Hsla = overlay(0x22).into();
        let active: gpui::Hsla = rgb(p.border_focus).into();
        let track: gpui::Hsla = transparent_black();
        let thumb: gpui::Hsla = rgb(p.border_loud).into();
        let thumb_hover: gpui::Hsla = rgb(p.text_faint).into();
        let border: gpui::Hsla = rgb(p.border_mid).into();
        let base = cx.global_mut::<gpui_base::Theme>();
        base.resizable.handle = hairline;
        base.resizable.active_handle = active;
        let mode = base.scrollbar.mode();
        let motion = base.scrollbar.motion();
        base.scrollbar = gpui_base::ScrollbarTheme::new()
            .with_mode(mode)
            .with_motion(motion)
            .with_styles(
                gpui_base::ScrollbarStyles::default()
                    .track(|style| style.bg(track))
                    .track_hover(|style| style.bg(track))
                    .track_active(|style| style.bg(track).border_color(border))
                    .thumb(|style| style.bg(thumb).radius(radius))
                    .thumb_hover(|style| style.bg(thumb_hover).radius(radius))
                    .thumb_active(|style| style.bg(thumb_hover).radius(radius)),
            );
    }
}

/// 把一个 RGB 按比例调亮（factor > 1）或压暗（< 1），逐通道饱和截断。
/// 只给上面的 hover / active 态推导用——手写十几个近似色不值当。
fn shade(color: u32, factor: f32) -> u32 {
    let ch = |shift: u32| {
        let v = ((color >> shift) & 0xff) as f32 * factor;
        (v.clamp(0.0, 255.0) as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

/// 当前色板。
pub fn palette() -> &'static Palette {
    if is_light() { &LIGHT } else { &DARK }
}

/// 给每个语义位生成一个取当前色板的读取函数——调用点写 `ui_theme::bg_rail()`。
macro_rules! slots {
    ($($name:ident),* $(,)?) => {
        $(
            #[inline]
            pub fn $name() -> u32 {
                palette().$name
            }
        )*
    };
}

slots!(
    bg_rail,
    bg_stage,
    bg_column,
    bg_bar,
    bg_card,
    bg_hover,
    bg_row_hover,
    bg_selected,
    bg_status,
    border_dim,
    border,
    border_mid,
    border_loud,
    border_focus,
    text_bright,
    text,
    text_mid,
    text_muted,
    text_faint,
    accent,
    green,
    yellow,
    blue,
    purple,
    red,
    diff_add_text,
    on_accent,
    diff_add_fg,
    diff_add_bg,
    diff_add_bar,
    diff_add_hl,
    diff_del_fg,
    diff_del_bg,
    diff_del_bar,
    diff_del_hl,
    diff_ctx_fg,
    diff_hunk_fg,
    diff_meta_fg,
    diff_empty_bg,
    diff_gutter,
);

/// 给纯色叠低透明度，用于角标底、激活态背景等衍生色。
/// `alpha` 0–255；`tint(accent(), 0x22)` 给强调蓝叠一层淡底。
pub fn tint(color: u32, alpha: u8) -> Rgba {
    rgba((color << 8) | alpha as u32)
}

/// 浮层材质：菜单、补全、对话框。Grok Bot 是实色抬起，不是磨砂。
/// 函数名 `glass_*` 是历史残留，实现必须不透明。
pub fn glass_floating() -> Rgba {
    rgb(bg_card())
}

/// 内容卡片材质：工具调用、附件、任务卡。实色 `bg_card`，比舞台抬一档。
pub fn glass_card() -> Rgba {
    rgb(bg_card())
}

/// 内容卡片圆角（工具卡、弹层、对话附件）。窗口分栏本身不圆角。
pub fn card_radius() -> Pixels {
    px(12.)
}

/// 卡片内边距统一出处，配合 `card_radius` 一起用，让重复出现的卡片手感一致。
pub fn card_padding() -> Pixels {
    px(12.)
}

/// 列表行 / 页签 / 侧栏按钮的圆角。比卡片小一档，避免行和外壳撞成同一半径。
pub fn row_radius() -> Pixels {
    px(8.)
}

/// 栏与栏之间的输入安全槽。
///
/// GPUI 的拖拽手柄以边界为中心、左右各约 4px；插件 WebView 却住在更高层级的
/// 原生 child window 中。栏缝为 0 时，用户沿着可见发丝按下会先命中 WebView，
/// GPUI 根本收不到 drag start。6px 让 WebView 从手柄之后开始，同时仍保持紧凑。
pub fn chrome_gap() -> Pixels {
    px(6.)
}

/// 窗口最外圈内边距。Grok Bot 栏贴边，所以是 0；分栏矩形直接顶到窗口。
pub fn shell_padding() -> Pixels {
    px(0.)
}

/// 对话列最大宽度。Grok 那种居中阅读栏，不是 IDE 拉满的 1040。
pub fn conversation_max_width() -> Pixels {
    px(768.)
}

/// 输入条圆角。Grok 的 composer 是胶囊，不是 12px 卡片。
pub fn composer_radius() -> Pixels {
    px(24.)
}

/// 主操作填充：深色近白、浅色近黑。Grok 发送钮同一套，不用品牌紫。
pub fn action_fill() -> u32 {
    text_bright()
}

/// 主操作上的字/图标，压在 `action_fill` 上。
pub fn action_on() -> u32 {
    bg_stage()
}

/// 弱分隔：半透明 overlay，不是实色 `border_dim`。
/// 用来切开区块，又不会把界面画成一堆 1px 表格线。
pub fn hairline() -> Rgba {
    overlay(0x14)
}

/// 卡片轮廓：半透明 overlay，Grok Bot 的 `sand-border-default` 同思路，不走硬灰边。
pub fn card_stroke() -> Rgba {
    overlay(0x22)
}

/// composer 输入区：实色抬起，压在舞台底上。
pub fn glass_input() -> Rgba {
    rgb(bg_card())
}

/// 浮层背后的压暗层。Grok Bot 弹层背后还能认出来，不要压成一块黑。
/// `heavy` 只比轻遮罩深一档，仍透得出舞台。
pub fn glass_scrim(heavy: bool) -> Rgba {
    rgba(if heavy { 0x0000004d } else { 0x00000020 })
}

/// 「在底色上压一层薄纱」——深色下是白纱，浅色下是黑纱。
/// 收编原先散落的 `rgba(0xffffff0d)` 之类：那些在浅色底上是隐形的。
pub fn overlay(alpha: u8) -> Rgba {
    let base: u32 = if is_light() { 0x000000 } else { 0xffffff };
    rgba((base << 8) | alpha as u32)
}

/// Agent 三态色：要你红 > 运行蓝 > 空闲灰。
pub fn agent_status_color(status: AgentStatus) -> Rgba {
    rgb(agent_status_u32(status))
}

fn agent_status_u32(status: AgentStatus) -> u32 {
    match status {
        AgentStatus::NeedsYou => red(),
        AgentStatus::Running => blue(),
        AgentStatus::Idle => text_faint(),
    }
}

/// 同一套状态色的 (r, g, b) 形态，给 mac 菜单栏（status_item）用。
pub fn agent_status_rgb8(status: AgentStatus) -> (u8, u8, u8) {
    let c = agent_status_u32(status);
    ((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// 左侧会话 / pane 状态色。空闲是灰，不另画点。
pub fn session_dot_color(status: AgentStatus) -> Rgba {
    agent_status_color(status)
}

const RUNNING_GLOW_DARK_LIGHTNESS_GAIN: f32 = 0.08;
const RUNNING_GLOW_LIGHT_LIGHTNESS_GAIN: f32 = 0.05;

/// 运行中 agent 图标进入状态时的一次性提亮色。
///
/// 桌面侧栏会话行用 `session_dot_color` 给 agent 图标上色：Running 是静态蓝。进入
/// 状态时的短暂提亮只改变 HSL 亮度，不按比例放大 RGB，避免蓝通道先饱和后把图标
/// 推成青色。主题判断也收在这里，调用方不再传容易反转的 light/dark 布尔值。
///
/// `phase` 是 0.0..=1.0 的过渡进度；起点和终点都是基准蓝，中点最亮。浅色底的
/// 增幅更小，避免在近白背景上失去对比度。终点回到静态蓝后不再预约动画帧。
pub fn running_glow_color(phase: f32) -> Rgba {
    let lightness_gain = if is_light() {
        RUNNING_GLOW_LIGHT_LIGHTNESS_GAIN
    } else {
        RUNNING_GLOW_DARK_LIGHTNESS_GAIN
    };
    running_glow_color_for(blue(), phase, lightness_gain)
}

fn running_glow_color_for(base: u32, phase: f32, lightness_gain: f32) -> Rgba {
    // 半个正弦周期保证起点/终点都回到基准色，不给稳态留下动画依赖。
    let wave = (phase.clamp(0.0, 1.0) * std::f32::consts::PI)
        .sin()
        .clamp(0.0, 1.0);
    let mut color: gpui::Hsla = rgb(base).into();
    color.l = (color.l + wave * lightness_gain).clamp(0.0, 1.0);
    color.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_tokens_keep_a_visible_hierarchy() {
        assert!(card_radius() > row_radius());
        assert_eq!(card_radius(), px(12.));
        assert_eq!(row_radius(), px(8.));
        assert_eq!(
            chrome_gap(),
            px(6.),
            "栏间要给原生 WebView 留出 GPUI 拖拽手柄的输入安全槽"
        );
        assert!(
            chrome_gap() >= px(4.),
            "输入安全槽不能小于 8px 拖拽命中区落在 WebView 一侧的半宽"
        );
        assert_eq!(
            shell_padding(),
            px(0.),
            "Grok Bot 栏贴边，窗口不要再留浮卡内边距"
        );
        assert_eq!(card_padding(), px(12.));
        assert!(composer_radius() > card_radius());
        assert_eq!(conversation_max_width(), px(768.));
        assert_eq!(action_fill(), text_bright());
        assert_eq!(action_on(), bg_stage());
        assert_ne!(
            action_fill(),
            accent(),
            "主操作不能走强调蓝，否则和 Grok Bot 发送钮两套语言"
        );
        assert_eq!(DARK.accent, 0x1084fe, "强调色必须是 Grok Bot fill/accent");
        assert_ne!(
            DARK.border_loud, DARK.accent,
            "滚动条 thumb 用 border_loud，不能跟强调蓝撞成一条亮蓝"
        );
        assert_eq!(LIGHT.accent, 0x1084fe);
        assert_ne!(DARK.accent, 0x5865f2, "Discord blurple 必须从色板里清掉");
        const {
            // 深色：舞台最暗，会话列表/卡片往上抬。
            assert!(DARK.bg_stage <= DARK.bg_column);
            assert!(DARK.bg_column <= DARK.bg_card);
            assert!(DARK.bg_card < DARK.bg_selected);
            // 浅色：舞台最亮，会话列表/卡片略压暗。
            assert!(LIGHT.bg_stage > LIGHT.bg_column);
            assert!(LIGHT.bg_column >= LIGHT.bg_card);
            assert!(LIGHT.bg_card > LIGHT.bg_selected);
        }
    }

    #[test]
    fn card_stroke_and_hairline_are_translucent() {
        assert!(
            card_stroke().a < 1.0,
            "卡片描边必须半透明，不能再走实色硬边"
        );
        assert!(hairline().a < card_stroke().a);
        assert!(hairline().a > 0.0);
    }

    #[test]
    fn content_surfaces_are_opaque() {
        assert_eq!(
            glass_card().a,
            1.0,
            "内容卡必须是 Grok Bot 实底，不能半透明"
        );
        assert_eq!(glass_floating().a, 1.0, "浮层必须是实底");
        assert_eq!(glass_input().a, 1.0, "composer 必须是实底");
    }

    fn channel(c: u32, shift: u32) -> i32 {
        ((c >> shift) & 0xff) as i32
    }

    /// 15px 空心图标在深色底上，RGB 欧氏距离会骗人：天蓝 `#459ffe`（G=159）
    /// 和薄荷绿 `#00c972`（B=114）数值差得开，看起来仍是两颗青绿。
    /// 运行必须蓝通道压过绿，完成必须绿通道压过蓝。
    fn assert_running_is_blue_and_done_is_green(
        blue: u32,
        green: u32,
        min_blue_lead: i32,
        min_green_lead: i32,
        label: &str,
    ) {
        let run_b = channel(blue, 0);
        let run_g = channel(blue, 8);
        let done_g = channel(green, 8);
        let done_b = channel(green, 0);
        assert!(
            run_b - run_g > min_blue_lead,
            "{label} 运行态不够蓝：B-G={} blue={blue:#08x}（15px 图标会收成青）",
            run_b - run_g
        );
        assert!(
            done_g - done_b > min_green_lead,
            "{label} 完成态不够绿：G-B={} green={green:#08x}（薄荷绿会跟青撞）",
            done_g - done_b
        );
    }

    #[test]
    fn agent_status_uses_three_colors() {
        assert_eq!(agent_status_u32(AgentStatus::NeedsYou), DARK.red);
        assert_eq!(agent_status_u32(AgentStatus::Running), DARK.blue);
        assert_eq!(agent_status_u32(AgentStatus::Idle), DARK.text_faint);
        assert_ne!(DARK.red, DARK.blue);
        assert_ne!(DARK.blue, DARK.text_faint);
    }

    #[test]
    fn running_is_visibly_blue() {
        // 运行中仍用蓝，避免再和别的状态挤在青绿里。
        assert_running_is_blue_and_done_is_green(DARK.blue, DARK.green, 110, 100, "深色");
        assert_running_is_blue_and_done_is_green(LIGHT.blue, LIGHT.green, 110, 85, "浅色");
    }

    #[test]
    fn running_glow_preserves_hue_and_limits_the_light_theme_peak() {
        let dark_base: gpui::Hsla = rgb(DARK.blue).into();
        let dark_start: gpui::Hsla =
            running_glow_color_for(DARK.blue, 0.0, RUNNING_GLOW_DARK_LIGHTNESS_GAIN).into();
        let dark_peak: gpui::Hsla =
            running_glow_color_for(DARK.blue, 0.5, RUNNING_GLOW_DARK_LIGHTNESS_GAIN).into();
        let dark_end: gpui::Hsla =
            running_glow_color_for(DARK.blue, 1.0, RUNNING_GLOW_DARK_LIGHTNESS_GAIN).into();
        let light_base: gpui::Hsla = rgb(LIGHT.blue).into();
        let light_peak: gpui::Hsla =
            running_glow_color_for(LIGHT.blue, 0.5, RUNNING_GLOW_LIGHT_LIGHTNESS_GAIN).into();

        let dark_hue_shift = (dark_peak.h - dark_base.h).abs();
        let light_hue_shift = (light_peak.h - light_base.h).abs();
        let dark_lightness_gain = dark_peak.l - dark_base.l;
        let light_lightness_gain = light_peak.l - light_base.l;
        assert_eq!(dark_start, dark_base, "进入动画必须从静态运行色开始");
        assert_eq!(dark_end, dark_base, "进入动画必须回到静态运行色结束");
        assert!(
            dark_hue_shift < 0.001
                && light_hue_shift < 0.001
                && dark_lightness_gain > light_lightness_gain,
            "状态提亮必须保持蓝色色相，且浅色主题更克制：dark hue={dark_hue_shift}, light hue={light_hue_shift}, dark gain={dark_lightness_gain}, light gain={light_lightness_gain}",
        );
    }
}
