//! 语义色板。自绘界面的颜色只从**当前装上的主题**来。
//!
//! gpui-kit 的 `Theme::update` / `apply_config` 管组件库。侧栏、会话行、气泡、
//! Git 大多没有 `&App`，不能在绘制时读 `cx.theme()`，所以装主题时把语义位
//! 抄进这里，绘制时读 `ui_theme::bg()`。这里没有第二套色值。
//!
//! 语义位和读的主题位一一对应，见 `docs/ui-design-language.md` 第 2 节。默认主题是
//! 组件库自带的 smelt Light / smelt Dark（`theme_catalog`），换别的主题时整份重填。
//!
//! 界面本身是黑白灰：颜色只表示状态（`run` / `need` / `ok` / `bad`）和主操作
//! （`ink`）。调用点不写裸 `rgb(0x...)`。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use gpui::{BoxShadow, FontFeatures, Hsla, Pixels, Rgba, px, rgb, rgba};

use smelt_model::agent_status::AgentStatus;

/// 一套完整语义色板。字段即语义位，深浅两套必须一一对应填满。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    // ---- 底色（由浅到深一档一档往下压） ----
    /// 舞台：对话、终端所在的主区。主题的 `background`。
    pub bg: u32,
    /// 侧栏、页签栏。主题的 `sidebar.background`。
    pub side: u32,
    /// 菜单、对话框、卡片、输入框、composer。主题的 `popover.background`。
    pub surface: u32,
    /// 次要按钮、行悬停、标签。主题的 `secondary.background`。
    pub soft: u32,
    /// 次要按钮悬停、行选中。主题的 `secondary.hover.background`。
    pub soft_hover: u32,
    /// 次要按钮按下、开关关闭底。主题的 `secondary.active.background`。
    pub soft_press: u32,

    // ---- 线（带透明度，压在哪层底上都是同一个深浅） ----
    /// 分隔线、卡片外沿。主题的 `border`。
    pub hair: Rgba,
    /// 输入框、复选框外沿。主题的 `input.border`。
    pub hair_strong: Rgba,

    // ---- 文字（由强到弱） ----
    /// 正文、标题。主题的 `foreground`。
    pub text: u32,
    /// 说明、次级信息、图标。主题的 `muted.foreground`。
    pub muted: u32,
    /// 占位、时间、快捷键。从 `muted` 往 `bg` 混出来，和底至少 3:1。
    pub faint: u32,

    // ---- 主操作 ----
    /// 主操作底、选中的复选框和开关。主题的 `primary.background`。
    pub ink: u32,
    pub ink_hover: u32,
    pub ink_press: u32,
    /// 压在 `ink` 上的字和图标。主题的 `primary.foreground`。
    pub on_ink: u32,

    /// 键盘焦点环。主题的 `ring`。
    pub focus: Rgba,

    // ---- 状态色：只用在状态上 ----
    /// 在跑。主题的 `base.blue`。
    pub run: u32,
    /// 要你处理、需要留意的提醒。主题的 `warning.background`。
    pub need: u32,
    /// 完成、通过、diff 新增。主题的 `success.background`。
    pub ok: u32,
    /// 出错、危险操作、diff 删除。主题的 `danger.background`。
    pub bad: u32,
    /// 压在 `bad` 实底上的字。主题的 `danger.foreground`。
    pub on_bad: u32,

    /// 对话框后的遮罩。主题的 `overlay`。
    pub scrim: Rgba,
    /// 分类色（用量分项、轨迹分道）：没有状态含义的并列类别。主题的 `chart.1`–`chart.5`。
    pub chart: [u32; 5],
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
    sidebar: Hsla,
    popover: Hsla,
    secondary: Hsla,
    secondary_hover: Hsla,
    secondary_active: Hsla,
    border: Hsla,
    input: Hsla,
    foreground: Hsla,
    muted_foreground: Hsla,
    primary: Hsla,
    primary_hover: Hsla,
    primary_active: Hsla,
    primary_foreground: Hsla,
    ring: Hsla,
    blue: Hsla,
    warning: Hsla,
    success: Hsla,
    danger: Hsla,
    danger_foreground: Hsla,
    overlay: Hsla,
    chart: [Hsla; 5],
}

fn palette_from_theme(theme: &gpui_component::Theme) -> Palette {
    palette_from_roles(&ThemeRoles {
        background: theme.background,
        sidebar: theme.sidebar,
        popover: theme.popover,
        secondary: theme.secondary,
        secondary_hover: theme.secondary_hover,
        secondary_active: theme.secondary_active,
        border: theme.border,
        input: theme.input,
        foreground: theme.foreground,
        muted_foreground: theme.muted_foreground,
        primary: theme.primary,
        primary_hover: theme.primary_hover,
        primary_active: theme.primary_active,
        primary_foreground: theme.primary_foreground,
        ring: theme.ring,
        blue: theme.blue,
        warning: theme.warning,
        success: theme.success,
        danger: theme.danger,
        danger_foreground: theme.danger_foreground,
        overlay: theme.overlay,
        chart: [
            theme.chart_1,
            theme.chart_2,
            theme.chart_3,
            theme.chart_4,
            theme.chart_5,
        ],
    })
}

/// 最弱字再往窗口底走的比例。对比仍够 3:1 才走满。
const TEXT_FAINT_MIX: f32 = 0.40;
/// 弱字自己已经偏暗时只走一小步，避免时间戳掉进底色。
const TEXT_FAINT_SHORT_MIX: f32 = 0.15;
const TEXT_FAINT_MIN_CONTRAST: f32 = 3.0;

/// 每个语义位读一个主题位，不按主题名分支。只有 `faint` 是算出来的：库只有一档弱字。
/// 实色位压在各自的底上取实色；线、焦点环、遮罩保留透明度。
fn palette_from_roles(theme: &ThemeRoles) -> Palette {
    let bg = solid(theme.background, theme.background);
    let muted = solid(theme.muted_foreground, theme.background);
    let on = |color: Hsla| solid(color, theme.background);
    Palette {
        bg,
        side: on(theme.sidebar),
        surface: on(theme.popover),
        soft: solid(theme.secondary, theme.sidebar),
        soft_hover: solid(theme.secondary_hover, theme.sidebar),
        soft_press: solid(theme.secondary_active, theme.sidebar),
        hair: theme.border.to_rgb(),
        hair_strong: theme.input.to_rgb(),
        text: on(theme.foreground),
        muted,
        faint: faint_text(muted, bg),
        ink: on(theme.primary),
        ink_hover: on(theme.primary_hover),
        ink_press: on(theme.primary_active),
        on_ink: solid(theme.primary_foreground, theme.primary),
        focus: theme.ring.to_rgb(),
        run: on(theme.blue),
        need: on(theme.warning),
        ok: on(theme.success),
        bad: on(theme.danger),
        on_bad: solid(theme.danger_foreground, theme.danger),
        scrim: theme.overlay.to_rgb(),
        chart: theme.chart.map(on),
    }
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

/// 主题还没装上时的色板：和 smelt Light / smelt Dark 主题文件同一组色值
/// （测试 `fallback_matches_the_smelt_themes` 锁住）。正式颜色由 `adopt_active_theme` 换上。
pub(crate) fn builtin_fallback(light: bool) -> Palette {
    if light {
        let bg = 0xffffff;
        let muted = 0x6b6a65;
        Palette {
            bg,
            side: 0xf7f7f5,
            surface: 0xffffff,
            soft: 0xf1f0ec,
            soft_hover: 0xe9e8e3,
            soft_press: 0xe1e0da,
            hair: rgba(0x1a1a1817),
            hair_strong: rgba(0x1a1a1829),
            text: 0x1a1a18,
            muted,
            faint: faint_text(muted, bg),
            ink: 0x1a1a18,
            ink_hover: 0x33332f,
            ink_press: 0x000000,
            on_ink: 0xffffff,
            focus: rgba(0x3a6ee173),
            run: 0x3a6ee1,
            need: 0xd2602a,
            ok: 0x2e7d4f,
            bad: 0xc8372d,
            on_bad: 0xffffff,
            scrim: rgba(0x1a1a1838),
            chart: [0x4f7cd9, 0x3f9a6a, 0xc98a2b, 0x8b63c7, 0xc2506f],
        }
    } else {
        let bg = 0x1e1e1d;
        let muted = 0xa3a29c;
        Palette {
            bg,
            side: 0x171716,
            surface: 0x262625,
            soft: 0x2a2a28,
            soft_hover: 0x323230,
            soft_press: 0x3a3a37,
            hair: rgba(0xfffffa14),
            hair_strong: rgba(0xfffffa26),
            text: 0xecece8,
            muted,
            faint: faint_text(muted, bg),
            ink: 0xecece8,
            ink_hover: 0xffffff,
            ink_press: 0xd6d6d0,
            on_ink: 0x1a1a18,
            focus: rgba(0x6ea0ff8c),
            run: 0x6ea0ff,
            need: 0xf08a55,
            ok: 0x6bc28e,
            bad: 0xf0776d,
            on_bad: 0x1a1a18,
            scrim: rgba(0x00000073),
            chart: [0x7fa3f0, 0x6cc294, 0xe0ad5c, 0xb293e6, 0xe07a97],
        }
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

/// 半透明色压在实底上取实色，给只认实色的地方（SVG 配色）用。
pub(crate) fn flatten(color: Rgba, behind: u32) -> u32 {
    pack(rgb(behind).blend(color))
}

fn pack(color: Rgba) -> u32 {
    let q = |channel: f32| ((channel.clamp(0.0, 1.0) * 255.0).round() as u32) & 0xff;
    (q(color.r) << 16) | (q(color.g) << 8) | q(color.b)
}

pub(crate) fn mix(from: u32, to: u32, t: f32) -> u32 {
    let channel = |shift: u32| {
        let start = ((from >> shift) & 0xff) as f32;
        let end = ((to >> shift) & 0xff) as f32;
        let value = start + (end - start) * t;
        (value.clamp(0.0, 255.0).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
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

pub(crate) fn contrast_ratio(a: u32, b: u32) -> f32 {
    let l1 = relative_luminance(a);
    let l2 = relative_luminance(b);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

/// 当前色板。渲染里同步读，不碰数据库。还没装主题时是 smelt 主题的色值。
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

/// 给每个语义位生成一个取当前色板的读取函数——调用点写 `ui_theme::bg()`。
macro_rules! slots {
    ($ty:ty => $($name:ident),* $(,)?) => {
        $(
            #[inline]
            pub fn $name() -> $ty {
                palette().$name
            }
        )*
    };
}

slots!(u32 =>
    bg,
    side,
    surface,
    soft,
    soft_hover,
    soft_press,
    text,
    muted,
    faint,
    ink,
    ink_hover,
    ink_press,
    on_ink,
    run,
    need,
    ok,
    bad,
    on_bad,
);

slots!(Rgba => hair, hair_strong, focus, scrim);

/// 第 `index` 个分类色，超出五个时循环。
pub fn chart(index: usize) -> u32 {
    let chart = palette().chart;
    chart[index % chart.len()]
}

/// 给纯色叠低透明度，用于状态色的浅底（悬停 8%–12%）、标记底等衍生色。
/// `alpha` 0–255；`tint(need(), 0x1f)` 是「要你处理」的浅底。
pub fn tint(color: u32, alpha: u8) -> Rgba {
    rgba((color << 8) | alpha as u32)
}

/// 浮起：真正浮在内容上的东西（菜单、对话框、浮动按钮）。两层投影加 1px `hair` 细圈；
/// 深色下投影加深，细圈本来就是浅色的 `hair`。
pub fn float_shadow() -> Vec<BoxShadow> {
    let (far, near) = if is_light() {
        (0.12, 0.06)
    } else {
        (0.45, 0.30)
    };
    let black = |alpha: f32| Hsla::from(rgb(0x000000)).alpha(alpha);
    vec![
        BoxShadow::new(px(0.), px(10.), black(far)).blur_radius(px(30.)),
        BoxShadow::new(px(0.), px(2.), black(near)).blur_radius(px(6.)),
        BoxShadow::new(px(0.), px(0.), hair().into()).spread_radius(px(1.)),
    ]
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

/// 窗口最外圈内边距。分栏贴边，所以是 0；分栏矩形直接顶到窗口。
pub fn shell_padding() -> Pixels {
    px(0.)
}

/// 对话列最大宽度。居中阅读栏，不是 IDE 拉满的 1040。
pub fn conversation_max_width() -> Pixels {
    px(768.)
}

/// 输入条圆角。composer 是大圆角，不是 12px 卡片。
pub fn composer_radius() -> Pixels {
    px(24.)
}

/// 用户消息气泡圆角。比内容卡片大，比输入条胶囊小。
pub fn bubble_radius() -> Pixels {
    px(20.)
}

/// 行、输入框、页签条的高度阶梯。同一档的东西并排放时顶边、底边才对得齐。
///
/// 按钮不要用这里的高度去压 `smelt_ui::Button`，按钮自己有 28 / 32 / 36 三档。
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

    /// 短输入框、小表头。
    pub fn sm() -> Pixels {
        px(28.)
    }

    /// 列表行：侧栏、智能体对话列表、用量明细。
    pub fn md() -> Pixels {
        px(32.)
    }

    /// 面板标题条和页签条。
    pub fn lg() -> Pixels {
        px(36.)
    }

    /// 表单行、检查器标题栏。
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

/// 「在底色上压一层薄纱」——深色下是白纱，浅色下是黑纱。
/// 收编原先散落的 `rgba(0xffffff0d)` 之类：那些在浅色底上是隐形的。
pub fn overlay(alpha: u8) -> Rgba {
    let base: u32 = if is_light() { 0x000000 } else { 0xffffff };
    rgba((base << 8) | alpha as u32)
}

/// Agent 三态色：要你橙 > 运行蓝 > 空闲灰。
pub fn agent_status_color(status: AgentStatus) -> Rgba {
    rgb(agent_status_u32(status))
}

/// 同一套状态色的 `0xRRGGBB` 形态，给菜单栏这类不走 gpui 的地方用。
pub fn agent_status_u32(status: AgentStatus) -> u32 {
    role_for_status(&palette(), status)
}

fn role_for_status(palette: &Palette, status: AgentStatus) -> u32 {
    match status {
        AgentStatus::NeedsYou => palette.need,
        AgentStatus::Running => palette.run,
        AgentStatus::Idle => palette.faint,
    }
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
    running_glow_color_for(run(), phase, lightness_gain)
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
        assert_eq!(shell_padding(), px(0.), "分栏贴边，窗口不留外圈内边距");
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
            sidebar: opaque(0x0c0c0c),
            popover: opaque(0x181818),
            secondary: opaque(0x202020),
            secondary_hover: opaque(0x282828),
            secondary_active: opaque(0x303030),
            border: rgba(0xffffff14).into(),
            input: rgba(0xffffff26).into(),
            foreground: opaque(0xfafafa),
            muted_foreground: opaque(0xa3a3a3),
            primary: opaque(0xfafafa),
            primary_hover: opaque(0xffffff),
            primary_active: opaque(0xe0e0e0),
            primary_foreground: opaque(0x171717),
            ring: rgba(0x6ea0ff8c).into(),
            blue: opaque(0x3b82f6),
            warning: opaque(0xf08a55),
            success: opaque(0x16a34a),
            danger: opaque(0xdc2626),
            danger_foreground: opaque(0xffffff),
            overlay: rgba(0x00000073).into(),
            chart: [
                opaque(0x000001),
                opaque(0x000002),
                opaque(0x000003),
                opaque(0x000004),
                opaque(0x000005),
            ],
        }
    }

    /// `Rgba` 量化回 `0xRRGGBBAA`，比较时不受 Hsla 来回换算的浮点误差影响。
    fn rgba_hex(color: Rgba) -> u32 {
        let q = |channel: f32| (channel.clamp(0.0, 1.0) * 255.0).round() as u32;
        (q(color.r) << 24) | (q(color.g) << 16) | (q(color.b) << 8) | q(color.a)
    }

    #[test]
    fn each_slot_reads_its_theme_role() {
        let palette = palette_from_roles(&sample_roles());
        assert_eq!(palette.bg, 0x111111);
        assert_eq!(palette.side, 0x0c0c0c);
        assert_eq!(palette.surface, 0x181818);
        assert_eq!(palette.soft, 0x202020);
        assert_eq!(palette.soft_hover, 0x282828);
        assert_eq!(palette.soft_press, 0x303030);
        assert_eq!(rgba_hex(palette.hair), 0xffffff14, "线保留透明度");
        assert_eq!(rgba_hex(palette.hair_strong), 0xffffff26);
        assert_eq!(palette.text, 0xfafafa);
        assert_eq!(palette.muted, 0xa3a3a3);
        assert_eq!(palette.faint, faint_text(palette.muted, palette.bg));
        assert_eq!(palette.ink, 0xfafafa);
        assert_eq!(palette.ink_hover, 0xffffff);
        assert_eq!(palette.ink_press, 0xe0e0e0);
        assert_eq!(palette.on_ink, 0x171717);
        assert_eq!(rgba_hex(palette.focus), 0x6ea0ff8c);
        assert_eq!(palette.run, 0x3b82f6);
        assert_eq!(palette.need, 0xf08a55);
        assert_eq!(palette.ok, 0x16a34a);
        assert_eq!(palette.bad, 0xdc2626);
        assert_eq!(palette.on_bad, 0xffffff);
        assert_eq!(rgba_hex(palette.scrim), 0x00000073);
        assert_eq!(palette.chart, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn translucent_fills_are_flattened_onto_their_layer() {
        // 行悬停、次要按钮落在侧栏上：主题给半透明时压在侧栏上取实色。
        let mut roles = sample_roles();
        roles.secondary = rgba(0xffffff20).into();
        let palette = palette_from_roles(&roles);
        assert_eq!(palette.soft, solid(roles.secondary, roles.sidebar));
        assert_ne!(palette.soft, palette.side);
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
    fn fallback_text_steps_stay_ordered_and_readable() {
        for light in [false, true] {
            let palette = builtin_fallback(light);
            let lum = relative_luminance;
            let ordered = if light {
                lum(palette.text) < lum(palette.muted)
                    && lum(palette.muted) < lum(palette.faint)
                    && lum(palette.faint) < lum(palette.bg)
            } else {
                lum(palette.text) > lum(palette.muted)
                    && lum(palette.muted) > lum(palette.faint)
                    && lum(palette.faint) > lum(palette.bg)
            };
            assert!(ordered, "light={light}: 正文 > 次级 > 最弱 > 底");
            assert!(contrast_ratio(palette.faint, palette.bg) >= TEXT_FAINT_MIN_CONTRAST);
            assert!(contrast_ratio(palette.on_ink, palette.ink) >= 4.5);
            assert!(contrast_ratio(palette.on_bad, palette.bad) >= 4.5);
            let status = [palette.run, palette.need, palette.ok, palette.bad];
            for (i, a) in status.iter().enumerate() {
                for b in &status[i + 1..] {
                    assert_ne!(a, b, "四个状态色要分得开");
                }
            }
        }
    }

    /// 首帧色板和 smelt 主题文件是同一组色值：主题文件改了，这里跟着改。
    #[gpui::test]
    fn fallback_matches_the_smelt_themes(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            for (name, light) in [
                (smelt_model::theme_index::DEFAULT_LIGHT, true),
                (smelt_model::theme_index::DEFAULT_DARK, false),
            ] {
                assert!(crate::theme_catalog::apply(name, cx), "{name} 装不上");
                let theme = gpui_component::Theme::global(cx);
                let applied = palette_from_theme(theme);
                let fallback = builtin_fallback(light);
                assert_eq!(theme.is_dark(), !light, "{name}");
                let solids = |p: &Palette| {
                    [
                        p.bg,
                        p.side,
                        p.surface,
                        p.soft,
                        p.soft_hover,
                        p.soft_press,
                        p.text,
                        p.muted,
                        p.faint,
                        p.ink,
                        p.ink_hover,
                        p.ink_press,
                        p.on_ink,
                        p.run,
                        p.need,
                        p.ok,
                        p.bad,
                        p.on_bad,
                    ]
                };
                let lines = |p: &Palette| [p.hair, p.hair_strong, p.focus, p.scrim].map(rgba_hex);
                assert_eq!(solids(&applied), solids(&fallback), "{name}");
                assert_eq!(lines(&applied), lines(&fallback), "{name}");
                assert_eq!(applied.chart, fallback.chart, "{name}");
            }
        });
    }

    #[test]
    fn agent_status_uses_three_colors() {
        let palette = builtin_fallback(false);
        assert_eq!(
            role_for_status(&palette, AgentStatus::NeedsYou),
            palette.need
        );
        assert_eq!(role_for_status(&palette, AgentStatus::Running), palette.run);
        assert_eq!(role_for_status(&palette, AgentStatus::Idle), palette.faint);
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

    /// Ayu Light / Catppuccin Latte 的主按钮字色曾经低于 4.5:1。深色主题本来就够，锁住别回退。
    #[test]
    fn primary_button_text_meets_contrast() {
        const MIN: f32 = 4.5;
        let watched = [
            "smelt Light",
            "smelt Dark",
            "Ayu Light",
            "Ayu Dark",
            "Catppuccin Latte",
            "Catppuccin Mocha",
        ];
        for file in [
            include_str!("../themes/smelt.json"),
            include_str!("../../plugins/themes/themes/ayu.json"),
            include_str!("../../plugins/themes/themes/catppuccin.json"),
        ] {
            let doc: serde_json::Value = serde_json::from_str(file).expect("theme json");
            let Some(themes) = doc["themes"].as_array() else {
                continue;
            };
            for theme in themes {
                let name = theme["name"].as_str().unwrap_or("");
                if !watched.contains(&name) {
                    continue;
                }
                let colors = &theme["colors"];
                let fg = parse_theme_hex(
                    colors["primary.foreground"]
                        .as_str()
                        .unwrap_or_else(|| panic!("{name} missing primary.foreground")),
                );
                let background = colors["primary.background"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{name} missing primary.background"));
                let slots = [
                    ("primary.background", background),
                    (
                        "primary.hover.background",
                        colors["primary.hover.background"]
                            .as_str()
                            .unwrap_or(background),
                    ),
                    (
                        "primary.active.background",
                        colors["primary.active.background"]
                            .as_str()
                            .unwrap_or(background),
                    ),
                ];
                for (slot, hex) in slots {
                    let ratio = contrast_ratio(fg, parse_theme_hex(hex));
                    assert!(
                        ratio >= MIN,
                        "{name} primary.foreground on {slot} ({hex}) is {ratio:.2}, need {MIN}"
                    );
                }
            }
        }
    }

    fn parse_theme_hex(hex: &str) -> u32 {
        let hex = hex.trim_start_matches('#');
        let rgb = &hex[..hex.len().min(6)];
        u32::from_str_radix(rgb, 16).unwrap_or_else(|_| panic!("bad color {hex}"))
    }
}
