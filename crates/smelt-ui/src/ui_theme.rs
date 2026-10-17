//! 语义色板。自绘界面的颜色只从**当前装上的主题**来。
//!
//! gpui-kit 的 `Theme::update` / `apply_config` 管组件库。侧栏、会话行、气泡、
//! Git 大多没有 `&App`，不能在绘制时读 `cx.theme()`，所以装主题时把语义位
//! 抄进这里，绘制时读 `ui_theme::bg_stage()`。这里没有第二套色值。
//!
//! 默认主题是库自带的 Default Dark / Default Light。换 Ayu、Catppuccin
//! 或其它已注册主题时，下面这一条映射整份重填。主题文件不带产品色块。
//!
//! 内容区用实底，不要再给卡片叠半透明。调用点不写裸 `rgb(0x...)`。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use gpui::{FontFeatures, Hsla, Pixels, Rgba, px, rgb, rgba};

use smelt_core::agent_status::AgentStatus;

/// 一套完整语义色板。字段即语义位，深浅两套必须一一对应填满。
#[derive(Clone, Copy)]
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
    /// 卡片 / 胶囊底。读主题的 `popover`，和菜单同一层。
    pub bg_card: u32,
    /// 控件 hover。`list_hover` 压在卡片上。
    pub bg_hover: u32,
    /// 列表行 hover。`list_hover` 压在侧栏上，不跟控件 hover 共用一个底。
    pub bg_row_hover: u32,
    /// 选中行底。侧栏选中色和列表选中色里，离栏底更远的那一个。
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
    /// 次级正文。正文和 `muted_foreground` 的中间。
    pub text_mid: u32,
    /// 弱化说明。主题的 `muted_foreground`。
    pub text_muted: u32,
    /// 最弱（占位、时间戳、快捷键提示）。从弱字再往窗口底走一截。
    pub text_faint: u32,

    // ---- 语义色 ----
    /// 强调色。`link` 能从正文里认出来就用它，否则用列表选中边，再否则用蓝。
    pub accent: u32,
    /// 绿：运行正常 / 通过 / diff 新增侧。
    pub green: u32,
    /// 黄：警告 / 额度将满，不是 agent 状态。
    pub yellow: u32,
    /// 蓝：运行中、读类工具。来自主题的 `base.blue`，不是强调色。
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

    /// 主操作填充，读主题的 `primary`。
    pub action_fill: u32,
    /// 压在 `action_fill` 上的字和图标。
    pub action_on: u32,
}

/// 当前是不是浅色。进程级全局态：色板要在任意渲染函数里同步读到，
/// 走 GPUI 的 Global 就得把 `&App` 一路传进每个取色点，代价不成比例
/// （跟 terminal.rs 的 `DARK_MODE` 是同一路数）。
static LIGHT_MODE: AtomicBool = AtomicBool::new(false);

static ACTIVE: RwLock<Option<Palette>> = RwLock::new(None);

/// 主题还没装上时的明暗。测试和 `apply` 失败时走这里。
/// 正式切换走 `adopt_active_theme`，整份换成当前主题。
pub fn set_light(light: bool) {
    install_palette(builtin_fallback(light), light);
}

pub fn is_light() -> bool {
    LIGHT_MODE.load(Ordering::Relaxed)
}

/// 主题装上之后，按语义位重填产品色板。
pub fn adopt_active_theme(cx: &gpui::App) {
    use gpui_component::Theme;

    if !cx.has_global::<Theme>() {
        return;
    }
    let theme = Theme::global(cx);
    install_palette(palette_from_theme(theme), !theme.is_dark());
}

fn install_palette(palette: Palette, light: bool) {
    LIGHT_MODE.store(light, Ordering::Relaxed);
    *ACTIVE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(palette);
}

struct ThemeRoles {
    background: Hsla,
    foreground: Hsla,
    muted_foreground: Hsla,
    sidebar: Hsla,
    popover: Hsla,
    list_hover: Hsla,
    list_active: Hsla,
    sidebar_accent: Hsla,
    list_active_border: Hsla,
    title_bar: Hsla,
    status_bar: Hsla,
    sidebar_border: Hsla,
    border: Hsla,
    input: Hsla,
    ring: Hsla,
    link: Hsla,
    success: Hsla,
    danger: Hsla,
    warning: Hsla,
    blue: Hsla,
    magenta: Hsla,
    primary: Hsla,
    primary_foreground: Hsla,
}

fn palette_from_theme(theme: &gpui_component::Theme) -> Palette {
    palette_from_roles(&ThemeRoles {
        background: theme.background,
        foreground: theme.foreground,
        muted_foreground: theme.muted_foreground,
        sidebar: theme.sidebar,
        popover: theme.popover,
        list_hover: theme.list_hover,
        list_active: theme.list_active,
        sidebar_accent: theme.sidebar_accent,
        list_active_border: theme.list_active_border,
        title_bar: theme.title_bar,
        status_bar: theme.status_bar,
        sidebar_border: theme.sidebar_border,
        border: theme.border,
        input: theme.input,
        ring: theme.ring,
        link: theme.link,
        success: theme.success,
        danger: theme.danger,
        warning: theme.warning,
        blue: theme.blue,
        magenta: theme.magenta,
        primary: theme.primary,
        primary_foreground: theme.primary_foreground,
    })
}

/// 正文和弱字之间的一档。库只有 `muted_foreground` 一种弱字。
const TEXT_MID_MIX: f32 = 0.45;
/// 最弱字再往窗口底走的比例。对比仍够 3:1 才走满。
const TEXT_FAINT_MIX: f32 = 0.40;
/// 弱字自己已经偏暗时只走一小步，避免时间戳掉进底色。
const TEXT_FAINT_SHORT_MIX: f32 = 0.15;
const TEXT_FAINT_MIN_CONTRAST: f32 = 3.0;
/// 和正文或窗口底的平方距离不超过这个值，就还是同一种墨。
/// 纯灰大约 23 级。Default Light 的 link 落到 `#171717`，
/// 正文是 `#0a0a0a`，只差 13 级，不能当强调色。
const EMPHASIS_MIN_DISTANCE2: u32 = 40 * 40;

/// 产品档读当前主题的语义位。库没有的几档才用这份主题自己的颜色推一次：
/// 字的中间档和最弱档、hover 压在各自的底上、选中行取离栏底更远的那个、
/// 强调色在 link 和正文糊在一起时改走列表选中边。diff 底色用 success / danger
/// 和背景混一次。不另写一套色，也不按主题名分支。
fn palette_from_roles(theme: &ThemeRoles) -> Palette {
    let bg = solid(theme.background, theme.background);
    let fg = solid(theme.foreground, theme.background);
    let muted_fg = solid(theme.muted_foreground, theme.background);
    let column = solid(theme.sidebar, theme.background);
    let card = solid(theme.popover, theme.background);
    let row_hover = solid(theme.list_hover, theme.sidebar);
    let control_hover = solid(theme.list_hover, theme.popover);
    let border = solid(theme.border, theme.background);
    let link = solid(theme.link, theme.background);
    let active_border = solid(theme.list_active_border, theme.background);
    let green = solid(theme.success, theme.background);
    let red = solid(theme.danger, theme.background);
    let yellow = solid(theme.warning, theme.background);
    let blue = solid(theme.blue, theme.background);
    let accent = emphasis(link, fg, bg, active_border, blue);
    Palette {
        bg_rail: bg,
        bg_stage: bg,
        bg_column: column,
        bg_bar: solid(theme.title_bar, theme.background),
        bg_card: card,
        bg_hover: control_hover,
        bg_row_hover: row_hover,
        bg_selected: selected_fill(theme.sidebar_accent, theme.list_active, theme.sidebar),
        bg_status: solid(theme.status_bar, theme.background),
        border_dim: solid(theme.sidebar_border, theme.background),
        border,
        border_mid: border,
        border_loud: solid(theme.input, theme.background),
        border_focus: solid(theme.ring, theme.background),
        text_bright: fg,
        text: fg,
        text_mid: mix(fg, muted_fg, TEXT_MID_MIX),
        text_muted: muted_fg,
        text_faint: faint_text(muted_fg, bg),
        accent,
        green,
        yellow,
        blue,
        purple: solid(theme.magenta, theme.background),
        red,
        diff_add_text: green,
        on_accent: solid(theme.primary_foreground, theme.primary),
        diff_add_fg: green,
        diff_add_bg: mix(bg, green, 0.18),
        diff_add_bar: green,
        diff_add_hl: mix(bg, green, 0.35),
        diff_del_fg: red,
        diff_del_bg: mix(bg, red, 0.18),
        diff_del_bar: red,
        diff_del_hl: mix(bg, red, 0.35),
        diff_ctx_fg: muted_fg,
        diff_hunk_fg: accent,
        diff_meta_fg: muted_fg,
        diff_empty_bg: bg,
        diff_gutter: card,
        action_fill: solid(theme.primary, theme.background),
        action_on: solid(theme.primary_foreground, theme.primary),
    }
}

/// 侧栏行的选中底。主题给侧栏准备的选中色离栏底更远，就用它。
/// 否则用列表选中色。Ayu 没写侧栏，回落的 accent 几乎贴着栏底，
/// 真正的选中是 `list.active`。
fn selected_fill(sidebar_accent: Hsla, list_active: Hsla, column: Hsla) -> u32 {
    let column_rgb = solid(column, column);
    let accent_fill = solid(sidebar_accent, column);
    let active_fill = solid(list_active, column);
    if distance2(accent_fill, column_rgb) >= distance2(active_fill, column_rgb) {
        accent_fill
    } else {
        active_fill
    }
}

fn emphasis(link: u32, foreground: u32, background: u32, active_border: u32, blue: u32) -> u32 {
    if reads_as_emphasis(link, foreground, background) {
        link
    } else if reads_as_emphasis(active_border, foreground, background) {
        active_border
    } else {
        blue
    }
}

fn reads_as_emphasis(color: u32, foreground: u32, background: u32) -> bool {
    distance2(color, foreground) > EMPHASIS_MIN_DISTANCE2
        && distance2(color, background) > EMPHASIS_MIN_DISTANCE2
}

fn faint_text(muted: u32, background: u32) -> u32 {
    let full = mix(muted, background, TEXT_FAINT_MIX);
    if contrast_ratio(muted, background) >= TEXT_FAINT_MIN_CONTRAST
        && contrast_ratio(full, background) >= TEXT_FAINT_MIN_CONTRAST
    {
        full
    } else {
        mix(muted, background, TEXT_FAINT_SHORT_MIX)
    }
}

/// 主题还没装上时的底。只对齐库 Default 的窗口底：深色 `neutral-950`，浅色白。
/// 强调色用一档能看见的蓝，不跟正文糊在一起。状态色彼此不同，
/// 好让首帧的状态点分得开。正式颜色由 `adopt_active_theme` 换上。
fn builtin_fallback(light: bool) -> Palette {
    let bg = if light { 0xffffff } else { 0x0a0a0a };
    let fg = if light { 0x0a0a0a } else { 0xfafafa };
    let muted = if light { 0x737373 } else { 0xa3a3a3 };
    let border = if light { 0xe5e5e5 } else { 0x262626 };
    let green = 0x16a34a;
    let red = 0xdc2626;
    let yellow = 0xca8a04;
    let blue = 0x2563eb;
    let purple = 0x9333ea;
    Palette {
        bg_rail: bg,
        bg_stage: bg,
        bg_column: bg,
        bg_bar: bg,
        bg_card: bg,
        bg_hover: bg,
        bg_row_hover: bg,
        bg_selected: bg,
        bg_status: bg,
        border_dim: border,
        border,
        border_mid: border,
        border_loud: border,
        border_focus: border,
        text_bright: fg,
        text: fg,
        text_mid: mix(fg, muted, TEXT_MID_MIX),
        text_muted: muted,
        text_faint: faint_text(muted, bg),
        accent: blue,
        green,
        yellow,
        blue,
        purple,
        red,
        diff_add_text: green,
        on_accent: bg,
        diff_add_fg: green,
        diff_add_bg: mix(bg, green, 0.18),
        diff_add_bar: green,
        diff_add_hl: mix(bg, green, 0.35),
        diff_del_fg: red,
        diff_del_bg: mix(bg, red, 0.18),
        diff_del_bar: red,
        diff_del_hl: mix(bg, red, 0.35),
        diff_ctx_fg: muted,
        diff_hunk_fg: blue,
        diff_meta_fg: muted,
        diff_empty_bg: bg,
        diff_gutter: bg,
        action_fill: fg,
        action_on: bg,
    }
}

fn solid(color: Hsla, behind: Hsla) -> u32 {
    let over = color.to_rgb();
    if over.a >= 0.98 {
        pack(over)
    } else {
        pack(behind.to_rgb().blend(over))
    }
}

fn pack(color: Rgba) -> u32 {
    let q = |channel: f32| ((channel.clamp(0.0, 1.0) * 255.0).round() as u32) & 0xff;
    (q(color.r) << 16) | (q(color.g) << 8) | q(color.b)
}

fn mix(from: u32, to: u32, t: f32) -> u32 {
    let channel = |shift: u32| {
        let start = ((from >> shift) & 0xff) as f32;
        let end = ((to >> shift) & 0xff) as f32;
        let value = start + (end - start) * t;
        (value.clamp(0.0, 255.0).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

fn distance2(a: u32, b: u32) -> u32 {
    let channel = |shift: u32| {
        let delta = ((a >> shift) & 0xff).abs_diff((b >> shift) & 0xff);
        delta * delta
    };
    channel(16) + channel(8) + channel(0)
}

fn channel_lin(channel: u32) -> f32 {
    let c = (channel as f32) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn relative_luminance(color: u32) -> f32 {
    0.2126 * channel_lin((color >> 16) & 0xff)
        + 0.7152 * channel_lin((color >> 8) & 0xff)
        + 0.0722 * channel_lin(color & 0xff)
}

fn contrast_ratio(a: u32, b: u32) -> f32 {
    let l1 = relative_luminance(a);
    let l2 = relative_luminance(b);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

/// 当前色板。渲染里同步读，不碰数据库。还没装主题时是库 Default 的底色。
pub fn palette() -> Palette {
    let guard = ACTIVE
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(palette) = *guard {
        return palette;
    }
    drop(guard);
    let palette = builtin_fallback(is_light());
    *ACTIVE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(palette);
    palette
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
/// `alpha` 0–255；`tint(accent(), 0x22)` 给强调色叠一层淡底。
pub fn tint(color: u32, alpha: u8) -> Rgba {
    rgba((color << 8) | alpha as u32)
}

/// 浮层材质：菜单、补全、对话框。Grok Bot 是实色抬起，不是磨砂。
/// 函数名 `glass_*` 是历史残留，实现必须不透明。
pub fn glass_floating() -> Rgba {
    rgb(bg_card())
}

/// 内容卡片材质：工具调用、附件、任务卡。
/// 实色 `bg_card`，跟主题的 popover 同一层。
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

/// 用户消息气泡圆角。比内容卡片大，比输入条胶囊小。
pub fn bubble_radius() -> Pixels {
    px(20.)
}

/// 控件和行的高度阶梯。同一档的东西并排放时顶边、底边才对得齐；新控件先挑一档，
/// 别再写裸 `px`。数值就是现有界面在用的六档，换成这组名字不改任何观感。
///
/// 圆角和内边距不跟着绑死：同一档里主操作和次操作的内边距本来就有意不同。
pub mod height {
    use gpui::{Pixels, px};

    /// 行内状态行（「正在想 · 12s」）、小状态胶囊。
    pub fn xxs() -> Pixels {
        px(22.)
    }

    /// 快捷键键帽、输入框里的小胶囊。
    pub fn xs() -> Pixels {
        px(24.)
    }

    /// 紧凑按钮、短输入框、小表头。
    pub fn sm() -> Pixels {
        px(28.)
    }

    /// 列表行：侧栏、智能体对话列表、用量明细。
    pub fn md() -> Pixels {
        px(32.)
    }

    /// 常规按钮、主操作胶囊、面板标题条和页签条。
    pub fn lg() -> Pixels {
        px(36.)
    }

    /// 页面级大按钮、表单行、检查器标题栏。
    pub fn xl() -> Pixels {
        px(44.)
    }
}

/// 等宽数字（OpenType `tnum`）。逐秒跳动的计时、计数挂上它：系统字体默认是比例数字，
/// `1` 比 `0` 窄，读数一变整行宽度就跟着变，右边的内容会抖。只管数字字形，别的字不受影响。
pub fn tabular_figures() -> FontFeatures {
    static TNUM: LazyLock<FontFeatures> =
        LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".into(), 1)])));
    TNUM.clone()
}

/// 标题胶囊离开窗口顶边的距离。
pub const CHROME_ISLAND_TOP_PX: f32 = 16.;
/// 标题胶囊高度。
pub const CHROME_ISLAND_HEIGHT_PX: f32 = 28.;
/// 标题胶囊占掉的顶带：顶距 + 高度。终端舞台整体从这条带下面开始，终端自己
/// 还有 8px 内边距，所以字落在和对话一样的 [`CHROME_ISLAND_CLEARANCE_PX`]。
pub const CHROME_ISLAND_BAND_PX: f32 = CHROME_ISLAND_TOP_PX + CHROME_ISLAND_HEIGHT_PX;
/// 穿过舞台正中的内容要让开胶囊：顶带 + 8px 缝。
///
/// 侧栏和工具页签走交通灯那条 `TOP_BAR_HEIGHT`，不用它。
pub const CHROME_ISLAND_CLEARANCE_PX: f32 = CHROME_ISLAND_BAND_PX + 8.;

/// 主操作填充，读当前主题的 `primary`。
pub fn action_fill() -> u32 {
    palette().action_fill
}

/// 主操作上的字/图标，压在 `action_fill` 上。
pub fn action_on() -> u32 {
    palette().action_on
}

/// 弱分隔：半透明 overlay，不是实色 `border_dim`。
/// 用来切开区块，又不会把界面画成一堆 1px 表格线。
pub fn hairline() -> Rgba {
    overlay(0x14)
}

/// 卡片轮廓：半透明 overlay，不走硬灰边。
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
    role_for_status(&palette(), status)
}

fn role_for_status(palette: &Palette, status: AgentStatus) -> u32 {
    match status {
        AgentStatus::NeedsYou => palette.red,
        AgentStatus::Running => palette.blue,
        AgentStatus::Idle => palette.text_faint,
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
        assert!(bubble_radius() > card_radius());
        assert!(composer_radius() > bubble_radius());
        assert_eq!(bubble_radius(), px(20.));
        assert_eq!(CHROME_ISLAND_TOP_PX, 16.);
        assert_eq!(CHROME_ISLAND_HEIGHT_PX, 28.);
        assert_eq!(
            CHROME_ISLAND_BAND_PX,
            CHROME_ISLAND_TOP_PX + CHROME_ISLAND_HEIGHT_PX
        );
        assert_eq!(
            CHROME_ISLAND_CLEARANCE_PX,
            CHROME_ISLAND_BAND_PX + 8.,
            "正中内容的净空必须由胶囊顶带和缝加出来"
        );
        assert_eq!(conversation_max_width(), px(768.));
    }

    fn opaque(hex: u32) -> Hsla {
        rgb(hex).into()
    }

    fn sample_roles() -> ThemeRoles {
        ThemeRoles {
            background: opaque(0x111111),
            foreground: opaque(0xfafafa),
            muted_foreground: opaque(0xa3a3a3),
            sidebar: opaque(0x222222),
            popover: opaque(0x181818),
            list_hover: rgba(0xffffff40).into(),
            list_active: rgba(0x1e40af33).into(),
            sidebar_accent: opaque(0x3a3a3a),
            list_active_border: opaque(0x1d4ed8),
            title_bar: opaque(0x171717),
            status_bar: opaque(0x0a0a0a),
            sidebar_border: opaque(0x262626),
            border: opaque(0x2a2a2a),
            input: opaque(0x3a3a3a),
            ring: opaque(0x737373),
            link: opaque(0x2563eb),
            success: opaque(0x16a34a),
            danger: opaque(0xdc2626),
            warning: opaque(0xca8a04),
            blue: opaque(0x3b82f6),
            magenta: opaque(0x9333ea),
            primary: opaque(0xfafafa),
            primary_foreground: opaque(0x171717),
        }
    }

    #[test]
    fn product_slots_read_theme_roles() {
        let roles = sample_roles();
        let palette = palette_from_roles(&roles);
        assert_eq!(palette.bg_stage, 0x111111);
        assert_eq!(palette.bg_rail, palette.bg_stage);
        assert_eq!(palette.bg_column, 0x222222);
        assert_eq!(palette.bg_card, 0x181818);
        assert_ne!(palette.bg_card, palette.border);
        assert_eq!(palette.bg_bar, 0x171717);
        assert_eq!(palette.bg_status, 0x0a0a0a);
        assert_eq!(palette.bg_row_hover, solid(roles.list_hover, roles.sidebar));
        assert_eq!(palette.bg_hover, solid(roles.list_hover, roles.popover));
        assert_ne!(palette.bg_hover, palette.bg_row_hover);
        assert_eq!(palette.bg_selected, 0x3a3a3a);
        assert_ne!(palette.bg_selected, solid(roles.list_active, roles.sidebar));
        assert_eq!(palette.border_dim, 0x262626);
        assert_eq!(palette.border, 0x2a2a2a);
        assert_eq!(palette.border_mid, palette.border);
        assert_eq!(palette.border_loud, 0x3a3a3a);
        assert_eq!(palette.border_focus, 0x737373);
        assert_eq!(palette.text, 0xfafafa);
        assert_eq!(palette.text_bright, palette.text);
        assert_eq!(palette.text_muted, 0xa3a3a3);
        assert_eq!(
            palette.text_mid,
            mix(palette.text, palette.text_muted, TEXT_MID_MIX)
        );
        assert_ne!(palette.text_mid, palette.text);
        assert_ne!(palette.text_mid, palette.text_muted);
        assert_eq!(
            palette.text_faint,
            faint_text(palette.text_muted, palette.bg_stage)
        );
        assert_ne!(palette.text_faint, palette.text_muted);
        assert!(contrast_ratio(palette.text_faint, palette.bg_stage) >= TEXT_FAINT_MIN_CONTRAST);
        assert_eq!(palette.accent, 0x2563eb);
        assert_eq!(palette.green, 0x16a34a);
        assert_eq!(palette.red, 0xdc2626);
        assert_eq!(palette.yellow, 0xca8a04);
        assert_eq!(palette.blue, 0x3b82f6);
        assert_eq!(palette.purple, 0x9333ea);
        assert_eq!(palette.action_fill, 0xfafafa);
        assert_eq!(
            palette.action_on,
            solid(roles.primary_foreground, roles.primary)
        );
        assert_eq!(palette.diff_add_fg, palette.green);
        assert_eq!(palette.diff_add_bar, palette.green);
        assert_eq!(
            palette.diff_add_bg,
            mix(palette.bg_stage, palette.green, 0.18)
        );
        assert_eq!(
            palette.diff_add_hl,
            mix(palette.bg_stage, palette.green, 0.35)
        );
        assert_eq!(palette.diff_del_fg, palette.red);
        assert_eq!(
            palette.diff_del_bg,
            mix(palette.bg_stage, palette.red, 0.18)
        );
        assert_eq!(palette.diff_hunk_fg, palette.accent);
        assert_eq!(palette.diff_ctx_fg, palette.text_muted);
        assert_eq!(palette.diff_empty_bg, palette.bg_stage);
        assert_eq!(palette.diff_gutter, palette.bg_card);
    }

    #[test]
    fn selected_row_keeps_list_active_when_sidebar_accent_is_flat() {
        let mut roles = sample_roles();
        roles.sidebar_accent = opaque(0x282828);
        roles.list_active = rgba(0x36a3d980).into();
        let palette = palette_from_roles(&roles);
        assert_eq!(palette.bg_selected, solid(roles.list_active, roles.sidebar));
        assert_ne!(
            palette.bg_selected,
            solid(roles.sidebar_accent, roles.sidebar)
        );
    }

    #[test]
    fn link_that_matches_body_ink_uses_the_active_border() {
        let mut roles = sample_roles();
        roles.link = roles.foreground;
        let palette = palette_from_roles(&roles);
        assert_eq!(palette.accent, 0x1d4ed8);
        assert_ne!(palette.accent, palette.text);
        assert_eq!(palette.diff_hunk_fg, palette.accent);

        roles.foreground = opaque(0x0a0a0a);
        roles.background = opaque(0xffffff);
        roles.link = opaque(0x171717);
        roles.list_active_border = opaque(0x60a5fa);
        let palette = palette_from_roles(&roles);
        assert_eq!(palette.accent, 0x60a5fa);
    }

    #[test]
    fn emphasis_uses_blue_when_link_and_border_are_body_ink() {
        let mut roles = sample_roles();
        roles.link = roles.foreground;
        roles.list_active_border = roles.foreground;
        let palette = palette_from_roles(&roles);
        assert_eq!(palette.accent, palette.blue);
    }

    #[test]
    fn faint_text_takes_a_short_step_when_muted_is_already_dim() {
        let muted = 0x52514f;
        let background = 0x0d1016;
        assert!(contrast_ratio(muted, background) < TEXT_FAINT_MIN_CONTRAST);
        assert_eq!(
            faint_text(muted, background),
            mix(muted, background, TEXT_FAINT_SHORT_MIX)
        );
        assert_ne!(faint_text(muted, background), muted);
    }

    #[test]
    fn contrast_ratio_matches_black_on_white() {
        let ratio = contrast_ratio(0xffffff, 0x000000);
        assert!((ratio - 21.0).abs() < 0.05, "{ratio}");
    }

    #[test]
    fn fallback_matches_builtin_default_background() {
        let dark = builtin_fallback(false);
        let light = builtin_fallback(true);
        assert_eq!(dark.bg_stage, 0x0a0a0a);
        assert_eq!(dark.bg_rail, dark.bg_stage);
        assert_eq!(dark.bg_column, dark.bg_stage);
        assert_eq!(light.bg_stage, 0xffffff);
        assert_eq!(dark.action_fill, dark.text);
        assert_eq!(dark.action_on, dark.bg_stage);
        assert_eq!(dark.accent, dark.blue);
        assert_ne!(dark.accent, dark.text);
        assert_ne!(light.accent, light.text);
        assert!(relative_luminance(dark.text) > relative_luminance(dark.text_mid));
        assert!(relative_luminance(dark.text_mid) > relative_luminance(dark.text_muted));
        assert!(relative_luminance(dark.text_muted) > relative_luminance(dark.text_faint));
        assert!(relative_luminance(dark.text_faint) > relative_luminance(dark.bg_stage));
        assert!(relative_luminance(light.text) < relative_luminance(light.text_mid));
        assert!(relative_luminance(light.text_mid) < relative_luminance(light.text_muted));
        assert!(relative_luminance(light.text_muted) < relative_luminance(light.text_faint));
        assert!(relative_luminance(light.text_faint) < relative_luminance(light.bg_stage));
        assert!(contrast_ratio(dark.text_faint, dark.bg_stage) >= TEXT_FAINT_MIN_CONTRAST);
        assert!(contrast_ratio(light.text_faint, light.bg_stage) >= TEXT_FAINT_MIN_CONTRAST);
        assert_ne!(dark.red, dark.blue);
        assert_ne!(dark.blue, dark.text_faint);
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

    #[test]
    fn agent_status_uses_three_colors() {
        let palette = builtin_fallback(false);
        assert_eq!(
            role_for_status(&palette, AgentStatus::NeedsYou),
            palette.red
        );
        assert_eq!(
            role_for_status(&palette, AgentStatus::Running),
            palette.blue
        );
        assert_eq!(
            role_for_status(&palette, AgentStatus::Idle),
            palette.text_faint
        );
        assert_ne!(palette.red, palette.blue);
        assert_ne!(palette.blue, palette.text_faint);
    }

    #[test]
    fn running_glow_preserves_hue_and_limits_the_light_theme_peak() {
        let blue = 0x3b82f6;
        let dark_base: gpui::Hsla = rgb(blue).into();
        let dark_start: gpui::Hsla =
            running_glow_color_for(blue, 0.0, RUNNING_GLOW_DARK_LIGHTNESS_GAIN).into();
        let dark_peak: gpui::Hsla =
            running_glow_color_for(blue, 0.5, RUNNING_GLOW_DARK_LIGHTNESS_GAIN).into();
        let dark_end: gpui::Hsla =
            running_glow_color_for(blue, 1.0, RUNNING_GLOW_DARK_LIGHTNESS_GAIN).into();
        let light_base: gpui::Hsla = rgb(blue).into();
        let light_peak: gpui::Hsla =
            running_glow_color_for(blue, 0.5, RUNNING_GLOW_LIGHT_LIGHTNESS_GAIN).into();

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
