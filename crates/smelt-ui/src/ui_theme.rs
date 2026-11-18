//! 语义色板。自绘界面的颜色只从**当前装上的主题**来。
//!
//! 主题由 `theme_catalog::apply` 装上：主题文件求值成语义色（`theme_config::ThemeColors`），
//! 在这里按窗口材质配好字色，再投影成界面常用的色板 [`Palette`]。侧栏、会话行、气泡、Git
//! 大多没有 `&App`，绘制时读 `ui_theme::bg()` 这类函数，不读全局；要色板以外的语义色读
//! [`colors`]，投影读 [`elevation`]。这里没有第二套色值：还没装主题时用的也是 smelt 主题文件。
//!
//! 色板位和读的语义色一一对应，见 `docs/ui-design-language.md` 第 2 节。默认主题是
//! smelt-ui 自带的 smelt Light / smelt Dark（`theme_catalog`），换别的主题时整份重填。
//!
//! 界面本身是黑白灰：颜色只表示状态（`run` / `need` / `ok` / `bad`）和主操作
//! （`ink`）。调用点不写裸 `rgb(0x...)`。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use gpui::{FontFeatures, Pixels, Rgba, px, rgb, rgba};

use smelt_model::agent_status::AgentStatus;

use crate::theme_catalog;
use crate::theme_config::{Elevation, ThemeColors, ThemeMode};

/// 一套完整语义色板。字段即语义位，深浅两套必须一一对应填满。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    // ---- 底色（由浅到深一档一档往下压） ----
    /// 舞台：对话、终端所在的主区。`bg_base`。
    pub bg: u32,
    /// 侧栏、页签栏。`page`。
    pub side: u32,
    /// 菜单、对话框、卡片、输入框、composer。`surface_raised`。
    pub surface: u32,
    /// 次要按钮、行悬停、标签。`layer_01`，压在 `side` 上。
    pub soft: u32,
    /// 次要按钮悬停、行选中。`layer_02`，同上。
    pub soft_hover: u32,
    /// 次要按钮按下、开关关闭底。`layer_03`，同上。
    pub soft_press: u32,
    /// 列表行悬停：正文色叠一层透明度，底下透出窗口材质时跟着透。`hover` 压在 `bg` 上
    /// 那一档，压在字底那层窗口底上至少差 [`ROW_HOVER_MIN_LIGHTNESS`] 的明度。
    pub row_hover: Rgba,
    /// 列表行选中，同上。`pressed` 那一档，至少差 [`ROW_SELECTED_MIN_LIGHTNESS`]。
    pub row_selected: Rgba,

    // ---- 线（带透明度，压在哪层底上都是同一个深浅） ----
    /// 分隔线、卡片外沿。`border_base`。
    pub hair: Rgba,
    /// 输入框、复选框外沿。`border_strong`。
    pub hair_strong: Rgba,

    // ---- 文字（由强到弱） ----
    /// 正文、标题。`text_base`。
    pub text: u32,
    /// 说明、次级信息、图标。`text_muted`。
    pub muted: u32,
    /// 占位、时间、快捷键。`text_faint`，压在字底下那层窗口底（毛玻璃时是材质，不然是
    /// `bg`）上至少 3:1。
    pub faint: u32,
    /// 压在选中行底上的 `muted`：选中底和窗口底差得出来，原样的次要字压上去就不够对比，
    /// 按选中底再配一次（[`row_muted`]）。
    pub muted_selected: u32,
    /// 压在选中行底上的 `faint`，从 `muted_selected` 往选中底混（[`row_faint`]）。
    pub faint_selected: u32,

    // ---- 主操作 ----
    /// 主操作底、选中的复选框。`bg_contrast`；悬停、按下叠 `contrast_hover` / `contrast_pressed`。
    pub ink: u32,
    pub ink_hover: u32,
    pub ink_press: u32,
    /// 压在 `ink` 三档上的字和图标。`text_contrast`，三档上都配够 4.5:1。
    pub on_ink: u32,

    /// 主按钮三档（常态、悬停、按下），同 `ink`。次要按钮三档是 `bg_button_neutral` 叠
    /// `hover` / `pressed`，字是 `text`。
    pub button_primary: [Rgba; 3],
    pub button_primary_foreground: u32,
    /// 只有图标的主按钮（发送、提交）三档：`bg_icon_button_contrast` 叠 `submit_hover` /
    /// `contrast_pressed`。图标是 `submit_icon`，三档上都配够 4.5:1。
    pub button_submit: [Rgba; 3],
    pub button_submit_foreground: u32,
    pub button_secondary: [Rgba; 3],
    pub button_secondary_foreground: u32,

    /// 键盘焦点环。`border_focus`。
    pub focus: Rgba,
    /// 文字选区（输入框、Markdown）。`selection`，带透明度。
    pub selection: Rgba,
    /// 正文里的链接。`text_accent`。
    pub link: u32,

    // ---- 状态色：只用在状态上 ----
    /// 在跑。`info.fg`（蓝只表示在跑；信息图标色可以另配色相）。
    pub run: u32,
    /// 要你处理、需要留意的提醒。`warning.icon`。
    pub need: u32,
    /// 完成、通过、diff 新增。`success.icon`。
    pub ok: u32,
    /// 出错、危险操作、diff 删除。`danger.icon`。
    pub bad: u32,
    /// 压在 `bad` 实底上的字：`text_contrast` 和 `text_base` 里对比高的那个，配够 4.5:1。
    pub on_bad: u32,

    /// 弹层（菜单、提示）里的正文。`text_base`，可能带透明度，
    /// 用 [`popover_foreground`] 压到实际弹层底上取实色。
    pub popover_text: Rgba,

    /// 对话框后的遮罩。`scrim`。
    pub scrim: Rgba,
    /// 分类色（用量分项、轨迹分道）：没有状态含义的并列类别。`chart.1`–`chart.5`。
    pub chart: [u32; 5],
}

/// 当前是不是浅色。进程级全局态：色板要在任意渲染函数里同步读到，
/// 走 GPUI 的 Global 就得把 `&App` 一路传进每个取色点，代价不成比例
/// （跟 terminal.rs 的 `DARK_MODE` 是同一路数）。
static LIGHT_MODE: AtomicBool = AtomicBool::new(false);

/// 装上的主题：投影好的色板，加上它来自的那份语义色和投影。
#[derive(Clone)]
struct Installed {
    palette: Palette,
    colors: ThemeColors,
    elevation: Arc<Elevation>,
    /// 窗口底叠出来的颜色，见 [`backdrop`]。
    backdrop: u32,
}

static ACTIVE: RwLock<Option<Installed>> = RwLock::new(None);

/// 窗口底下那块系统材质（毛玻璃）在深浅两种外观下的代表色（0xRRGGBB）。
///
/// 材质的颜色跟着后面的桌面变，程序读不到，只能由宿主按材质种类给一个代表值，往不利的
/// 那头留余量：深色往亮、浅色往暗。字在代表色上对比够，在常见的桌面上就都够。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaterialTone {
    pub dark: u32,
    pub light: u32,
}

impl MaterialTone {
    fn pack(self) -> u64 {
        (u64::from(self.dark & 0xffffff) << 32) | u64::from(self.light & 0xffffff)
    }

    fn unpack(packed: u64) -> Self {
        Self {
            dark: (packed >> 32) as u32,
            light: packed as u32,
        }
    }

    fn pick(self, dark: bool) -> u32 {
        if dark { self.dark } else { self.light }
    }
}

/// 「窗口底下没有材质」：两个色各只占低 24 位，打包后不可能撞上。
const NO_MATERIAL: u64 = u64::MAX;

/// 窗口底下垫的材质（打包的 [`MaterialTone`]）。
static WINDOW_MATERIAL: AtomicU64 = AtomicU64::new(NO_MATERIAL);
/// 有材质时主题底色铺在材质上的不透明度（`f32` 的位）。
static BG_OPACITY: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// 宿主告诉色板窗口底透的地方是怎么叠的：底下垫什么材质（`None` 是没有，窗口不透明；几种
/// 材质时宿主给最不利的那种），主题底色以多大的不透明度（0–1）铺在材质上。界面上的字压在两者叠出来的颜色上（[`text_backdrop`]），
/// 之后装的每份主题都按它配色，所以这两样变了要重装一次主题。
pub fn set_window_backdrop(material: Option<MaterialTone>, bg_opacity: f32) {
    WINDOW_MATERIAL.store(
        material.map_or(NO_MATERIAL, MaterialTone::pack),
        Ordering::Relaxed,
    );
    BG_OPACITY.store(bg_opacity.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

fn window_material() -> Option<MaterialTone> {
    match WINDOW_MATERIAL.load(Ordering::Relaxed) {
        NO_MATERIAL => None,
        packed => Some(MaterialTone::unpack(packed)),
    }
}

fn bg_opacity() -> f32 {
    f32::from_bits(BG_OPACITY.load(Ordering::Relaxed))
}

/// 窗口底实色的地方铺的那层：不透的主题底色。窗口底只由宿主在窗口根上按地方铺（实色的
/// 这层、透的地方 [`glass_fill`]），各栏、各视图不再铺页面底。
pub fn solid_fill() -> Rgba {
    Rgba {
        a: 1.0,
        ..colors().bg_base
    }
}

/// 窗口底透的地方（布局声明了材质的停靠区）铺的那层：主题底色按设置的不透明度，透出底下的
/// 材质。没有材质时和 [`solid_fill`] 一样。
pub fn glass_fill() -> Rgba {
    let bg = colors().bg_base;
    match window_material() {
        Some(_) => Rgba {
            a: bg.a * bg_opacity(),
            ..bg
        },
        None => Rgba { a: 1.0, ..bg },
    }
}

/// 窗口底透的地方叠出来的颜色：界面上的字、行底、交给组件库和插件的实色都按它配。和
/// [`glass_fill`] 是同一份算法，读的是同一时刻装上的主题。实色的地方离字更远，对比只会更足。
pub(crate) fn backdrop() -> u32 {
    installed(|installed| installed.backdrop)
}

/// 正文压在窗口底上至少要的对比（WCAG AAA），和次要字拉开层次。
const TEXT_MIN_CONTRAST: f32 = 7.0;
/// 次要字压在窗口底上至少要的对比（WCAG AA 正文）。压在实色填充（主按钮、危险底）上的字
/// 也按这一档。
const TEXT_MUTED_MIN_CONTRAST: f32 = 4.5;
/// 行悬停底和窗口底至少差的明度（CIE L*，0–100）：刚好看得出指针在哪一行。
/// 按明度差不按对比度：同一个对比度在亮底上看着比暗底上重得多，明度差深浅两头观感一样。
const ROW_HOVER_MIN_LIGHTNESS: f32 = 5.0;
/// 行选中底和窗口底至少差的明度。名字不随选中换色，选中全靠这层底，要一眼认得出，
/// 也要和悬停拉开一档。主题配得更深的照主题。
const ROW_SELECTED_MIN_LIGHTNESS: f32 = 15.0;

/// 字实际压着的那层窗口底：有材质时是主题底色按 `opacity` 叠在材质代表色上，不然就是
/// 主题底色。
fn text_backdrop(
    material: Option<MaterialTone>,
    background: Rgba,
    opacity: f32,
    dark: bool,
) -> u32 {
    match material {
        None => pack(background),
        Some(tone) => flatten(
            Rgba {
                a: background.a * opacity,
                ..background
            },
            tone.pick(dark),
        ),
    }
}

/// 把字色往离底更远的那头（白或黑）挪，挪到和底的对比够 `min` 为止；本来就够的不动。
/// 只改明暗，色相跟着混，主题给的冷暖还在。
pub(crate) fn legible(fg: u32, backdrop: u32, min: f32) -> u32 {
    legible_toward(fg, backdrop, min, away_from(backdrop))
}

/// 实色按钮悬停、按下两档的底：主题的叠层叠在静息底上。叠完字往哪头挪都不够 4.5:1 时
/// （浅色主题正文偏浅，主按钮底是中灰，按下往白提，白字就压不住），两档一起反过来、按同样
/// 的力度往字的另一头压：按下照样看得出，字也看得清。两档同一个方向，不一档变亮一档变暗。
fn state_fills(rest: u32, overlays: [Rgba; 2]) -> [u32; 2] {
    let text_side = away_from(rest);
    let themed = overlays.map(|overlay| flatten(overlay, rest));
    if themed
        .iter()
        .all(|fill| contrast_ratio(text_side, *fill) >= TEXT_MUTED_MIN_CONTRAST)
    {
        return themed;
    }
    let other_side = 0xffffff - text_side;
    overlays.map(|overlay| mix(rest, other_side, overlay.a))
}

/// 离 `backdrop` 更远的那头：白或黑。
fn away_from(backdrop: u32) -> u32 {
    if contrast_ratio(0xffffff, backdrop) >= contrast_ratio(0x000000, backdrop) {
        0xffffff
    } else {
        0x000000
    }
}

/// 同 [`legible`]，但只往 `toward` 那头挪；那头挪到底也不够时停在 `toward`。
fn legible_toward(fg: u32, backdrop: u32, min: f32, toward: u32) -> u32 {
    if contrast_ratio(fg, backdrop) >= min {
        return fg;
    }
    // 对比随 t 单调变大，二分找刚好够的那一步，不多挪。
    let (mut short, mut enough) = (0.0_f32, 1.0_f32);
    for _ in 0..16 {
        let t = (short + enough) / 2.0;
        if contrast_ratio(mix(fg, toward, t), backdrop) >= min {
            enough = t;
        } else {
            short = t;
        }
    }
    mix(fg, toward, enough)
}

/// 窗口底下有材质时，把直接压在窗口底上的那几档字色和图标色按实际的窗口底配够对比。主题
/// 是按自己的 `bg_base` 配色的；毛玻璃时字压在底色和材质叠出来的颜色上，底色铺得越薄越接近
/// 材质，材质一般比深色主题的底亮、比浅色主题的底暗，灰字就糊成一片。在装主题这一处改，
/// 自绘色板、[`colors`] 和应用交给组件库的字色是同一组，不用一处处补。没有材质时什么都不改。
pub(crate) fn fit_text_to_backdrop(colors: &mut ThemeColors, dark: bool) {
    let Some(material) = window_material() else {
        return;
    };
    let backdrop = text_backdrop(Some(material), colors.bg_base, bg_opacity(), dark);
    let fit = |color: &mut Rgba, min: f32| {
        let flat = flatten(*color, backdrop);
        let fitted = legible(flat, backdrop, min);
        if fitted != flat {
            *color = rgb(fitted);
        }
    };
    fit(&mut colors.text_base, TEXT_MIN_CONTRAST);
    fit(&mut colors.icon_base, TEXT_MIN_CONTRAST);
    fit(&mut colors.text_muted, TEXT_MUTED_MIN_CONTRAST);
    fit(&mut colors.icon_muted, TEXT_MUTED_MIN_CONTRAST);
    fit(&mut colors.text_faint, TEXT_FAINT_MIN_CONTRAST);
    fit(&mut colors.icon_faint, TEXT_FAINT_MIN_CONTRAST);
}

/// 主题还没装上时的明暗。测试和 `apply` 失败时走这里，装上同明暗的 smelt 主题。
/// 正式切换走 `theme_catalog::apply`，整份换成当前主题。
pub fn set_light(light: bool) {
    install(builtin(light), light);
}

pub fn is_light() -> bool {
    LIGHT_MODE.load(Ordering::Relaxed)
}

/// 主题装上之后，按语义色重填色板。只由 `theme_catalog::apply` 调；`colors` 已经按材质配过
/// 字色（[`fit_text_to_backdrop`]）。
pub(crate) fn install_theme(colors: &ThemeColors, elevation: Arc<Elevation>, dark: bool) {
    let backdrop = text_backdrop(window_material(), colors.bg_base, bg_opacity(), dark);
    let installed = Installed {
        palette: palette_from_tokens(colors, backdrop),
        colors: *colors,
        elevation,
        backdrop,
    };
    install(installed, !dark);
}

fn install(installed: Installed, light: bool) {
    LIGHT_MODE.store(light, Ordering::Relaxed);
    *ACTIVE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(installed);
}

/// 同明暗的 smelt 主题，不按材质配字色。
fn builtin(light: bool) -> Installed {
    let mode = if light {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    let config = theme_catalog::builtin_for_mode(mode);
    let colors = *config.colors();
    let backdrop = pack(colors.bg_base);
    Installed {
        palette: palette_from_tokens(&colors, backdrop),
        colors,
        elevation: config.elevation().clone(),
        backdrop,
    }
}

/// 选中行里的最弱字从选中行里的次要字往选中底走的比例。对比仍够 3:1 才走满。
const TEXT_FAINT_MIX: f32 = 0.40;
/// 弱字自己已经偏暗时只走一小步，避免时间戳掉进底色。
const TEXT_FAINT_SHORT_MIX: f32 = 0.15;
const TEXT_FAINT_MIN_CONTRAST: f32 = 3.0;

/// 每个色板位读一个语义色，不按主题名分支。实色位压在各自的底上取实色；线、焦点环、遮罩
/// 保留透明度。压在实色填充上的字（`on_ink`、`on_bad`）配够 4.5:1，主题自己配得不够的也看
/// 得清。`text_backdrop` 是字实际压着的那层窗口底（[`text_backdrop`]），行底按它保底。
fn palette_from_tokens(c: &ThemeColors, text_backdrop: u32) -> Palette {
    let bg = pack(c.bg_base);
    let on = |color: Rgba| flatten(color, bg);
    let side = on(c.page);
    let text = on(c.text_base);
    let muted = on(c.text_muted);
    let rows = row_fills(on(c.hover), on(c.pressed), bg, text, muted, text_backdrop);
    let ink = on(c.bg_contrast);
    let [ink_hover, ink_press] = state_fills(ink, [c.contrast_hover, c.contrast_pressed]);
    // 静息、悬停、按下三层底共用一个字色。字色只往离静息底更远的那头挪：按下那层压得
    // 够深时，挪到头也只是不够 4.5，不会翻成另一头、在静息底上反而看不清。
    let on_fills = |fg: Rgba, fills: [u32; 3]| {
        let toward = away_from(fills[0]);
        fills.into_iter().fold(flatten(fg, fills[0]), |fg, fill| {
            legible_toward(fg, fill, TEXT_MUTED_MIN_CONTRAST, toward)
        })
    };
    let on_ink = on_fills(c.text_contrast, [ink, ink_hover, ink_press]);
    let submit = on(c.bg_icon_button_contrast);
    let [submit_hover, submit_press] = state_fills(submit, [c.submit_hover, c.contrast_pressed]);
    let submit_fills = [submit, submit_hover, submit_press];
    let on_submit = on_fills(c.submit_icon, submit_fills);
    let bad = on(c.danger.icon);
    let on_bad = [c.text_contrast, c.text_base]
        .map(|fg| flatten(fg, bad))
        .into_iter()
        .max_by(|a, b| contrast_ratio(*a, bad).total_cmp(&contrast_ratio(*b, bad)))
        .map(|fg| legible(fg, bad, TEXT_MUTED_MIN_CONTRAST))
        .unwrap_or(text);
    let neutral = c.bg_button_neutral;
    Palette {
        bg,
        side,
        surface: on(c.surface_raised),
        soft: flatten(c.layer_01, side),
        soft_hover: flatten(c.layer_02, side),
        soft_press: flatten(c.layer_03, side),
        row_hover: rows.hover,
        row_selected: rows.selected,
        hair: c.border_base,
        hair_strong: c.border_strong,
        text,
        muted,
        faint: on(c.text_faint),
        muted_selected: rows.muted_selected,
        faint_selected: rows.faint_selected,
        ink,
        ink_hover,
        ink_press,
        on_ink,
        button_primary: [rgb(ink), rgb(ink_hover), rgb(ink_press)],
        button_primary_foreground: on_ink,
        button_submit: submit_fills.map(rgb),
        button_submit_foreground: on_submit,
        button_secondary: [neutral, over(c.hover, neutral), over(c.pressed, neutral)],
        button_secondary_foreground: text,
        focus: c.border_focus,
        selection: c.selection,
        link: on(c.text_accent),
        // 「在跑」是蓝（第 4 节），取信息色本身；信息图标色可以另配别的色相（默认主题是紫）。
        run: on(c.info.fg),
        need: on(c.warning.icon),
        ok: on(c.success.icon),
        bad,
        on_bad,
        popover_text: c.text_base,
        scrim: c.scrim,
        chart: c.chart.map(on),
    }
}

struct RowFills {
    hover: Rgba,
    selected: Rgba,
    muted_selected: u32,
    faint_selected: u32,
}

/// 列表行的悬停底、选中底和选中行里的次要字。`hover` / `active` 是 `hover` / `pressed` 压在
/// `bg` 上的那两档实色，换算成正文色的透明度，再按字实际压着的那层窗口底（`backdrop`，毛玻璃时是材质）
/// 保底到看得出。材质一般比深色主题的底亮，同一层白压上去显得淡，只照主题的透明度会糊。
fn row_fills(hover: u32, active: u32, bg: u32, text: u32, muted: u32, backdrop: u32) -> RowFills {
    let hover = row_fill(hover, bg, text, backdrop, ROW_HOVER_MIN_LIGHTNESS);
    let selected = row_fill(active, bg, text, backdrop, ROW_SELECTED_MIN_LIGHTNESS);
    let under = flatten(selected, backdrop);
    let muted_selected = legible(muted, under, TEXT_MUTED_MIN_CONTRAST);
    RowFills {
        hover,
        selected,
        muted_selected,
        faint_selected: faint_text(muted_selected, under),
    }
}

/// 一档行底：正文色叠透明度。透明度取主题那一档（`fill` 压在 `bg` 上比 `bg` 往正文色走了
/// 多少）和压在 `backdrop` 上和它差够 `min` 明度要的那档里大的那个。主题把这一档配反了（比底更
/// 远离正文色）就只按 `min` 算。
fn row_fill(fill: u32, bg: u32, ink: u32, backdrop: u32, min: f32) -> Rgba {
    let span = channel_sum(ink) - channel_sum(bg);
    let themed = if span.abs() < 1.0 {
        0.0
    } else {
        ((channel_sum(fill) - channel_sum(bg)) / span).clamp(0.0, 1.0)
    };
    let apart = |t: f32| (lightness(mix(backdrop, ink, t)) - lightness(backdrop)).abs();
    let alpha = if apart(themed) >= min {
        themed
    } else {
        // 明度差随透明度单调变大，二分找刚好够的那一档。
        let (mut short, mut enough) = (themed, 1.0_f32);
        for _ in 0..16 {
            let t = (short + enough) / 2.0;
            if apart(t) >= min {
                enough = t;
            } else {
                short = t;
            }
        }
        enough
    };
    let mut color = rgb(ink);
    color.a = alpha;
    color
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

/// 同明暗的 smelt 主题投影出的色板，不按材质配字色。测试拿它当已知的一组色。
#[cfg(test)]
pub(crate) fn builtin_fallback(light: bool) -> Palette {
    builtin(light).palette
}

/// 半透明色叠在可能也半透明的色上（source-over），结果仍可能半透明：次要按钮的底本身是
/// 半透明的，悬停叠层要叠在它上面，底下的窗口材质照样透上来。
fn over(top: Rgba, bottom: Rgba) -> Rgba {
    let a = top.a + bottom.a * (1.0 - top.a);
    if a <= 0.0 {
        return Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
    }
    let channel = |t: f32, b: f32| (t * top.a + b * bottom.a * (1.0 - top.a)) / a;
    Rgba {
        r: channel(top.r, bottom.r),
        g: channel(top.g, bottom.g),
        b: channel(top.b, bottom.b),
        a,
    }
}

/// 弹层正文来自主题的正文色，半透明时压到实际弹层底色上。
pub(crate) fn popover_foreground(surface: u32) -> u32 {
    flatten(palette().popover_text, surface)
}

/// 半透明色压在实底上取实色。
pub(crate) fn flatten(color: Rgba, behind: u32) -> u32 {
    pack(rgb(behind).blend(color))
}

pub(crate) fn pack(color: Rgba) -> u32 {
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

/// CIE L*：人眼看到的明暗，0 黑 100 白，等距的差看着也差不多一样大。
fn lightness(color: u32) -> f32 {
    let y = relative_luminance(color);
    if y > 216.0 / 24389.0 {
        116.0 * y.cbrt() - 16.0
    } else {
        y * 24389.0 / 27.0
    }
}

pub(crate) fn contrast_ratio(a: u32, b: u32) -> f32 {
    let l1 = relative_luminance(a);
    let l2 = relative_luminance(b);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

/// 当前装上的主题；还没装时装上同明暗的 smelt 主题。
fn installed<T>(read: impl Fn(&Installed) -> T) -> T {
    if let Some(installed) = ACTIVE
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        return read(installed);
    }
    let light = is_light();
    let fallback = builtin(light);
    let value = read(&fallback);
    let mut slot = ACTIVE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.is_none() {
        *slot = Some(fallback);
    }
    value
}

/// 当前色板。渲染里同步读，不碰数据库。
pub fn palette() -> Palette {
    installed(|installed| installed.palette)
}

/// 当前主题的全部语义色，字色已按窗口材质配过。色板 [`Palette`] 没有的位从这里读。
pub fn colors() -> ThemeColors {
    installed(|installed| installed.colors)
}

/// 当前主题的投影（阴影）。
pub fn elevation() -> Arc<Elevation> {
    installed(|installed| installed.elevation.clone())
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
    link,
    side,
    surface,
    soft,
    soft_hover,
    soft_press,
    text,
    muted,
    faint,
    muted_selected,
    faint_selected,
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

slots!(Rgba => row_hover, row_selected, hair, hair_strong, focus, selection, scrim);

/// 列表行里的次要字：选中行压在选中底上，换成按它配过的那档。
pub fn row_muted(selected: bool) -> u32 {
    if selected { muted_selected() } else { muted() }
}

/// 列表行里的最弱字，同 [`row_muted`]。
pub fn row_faint(selected: bool) -> u32 {
    if selected { faint_selected() } else { faint() }
}

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

/// 把和 `bg` 差一档的实色底（侧栏的 `side`）换成压在 `bg` 上的半透明黑或白：压在实色
/// `bg` 上和 `fill` 一样深浅，底下是窗口材质时跟着透。和 `bg` 一样深时全透明。
/// 主题带色相时只对得上深浅，色相跟着 `bg` 走。
pub fn shade(fill: u32) -> Rgba {
    shade_over(fill, bg())
}

fn shade_over(fill: u32, base: u32) -> Rgba {
    let ink = if channel_sum(fill) < channel_sum(base) {
        0x000000
    } else {
        0xffffff
    };
    let span = channel_sum(ink) - channel_sum(base);
    let mut color = rgb(ink);
    color.a = if span.abs() < 1.0 {
        0.0
    } else {
        ((channel_sum(fill) - channel_sum(base)) / span).clamp(0.0, 1.0)
    };
    color
}

fn channel_sum(color: u32) -> f32 {
    ((color >> 16) & 0xff) as f32 + ((color >> 8) & 0xff) as f32 + (color & 0xff) as f32
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
/// 按钮不要用这里的高度去压 `smelt_ui::Button`，文字按钮自己有 24 / 28 / 32 三档。
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
/// 侧栏和工具页签走交通灯那条 [`crate::window_bar::TOP_BAR_HEIGHT`]，不用它。
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

    /// 宿主各材质代表色的两头：深色里最亮、最暗的，浅色里最暗、最亮的。字和行底在这两头
    /// 都成立，中间的就都成立。
    const TEST_MATERIALS: [MaterialTone; 2] = [
        MaterialTone {
            dark: 0x363a40,
            light: 0xd0d0d3,
        },
        MaterialTone {
            dark: 0x2a2a2a,
            light: 0xe6e6e6,
        },
    ];

    /// 材质的两个代表色打包进一个原子量再取出来不变，也撞不上「没有材质」。
    #[test]
    fn material_tone_survives_packing() {
        for tone in TEST_MATERIALS {
            assert_ne!(tone.pack(), NO_MATERIAL);
            assert_eq!(MaterialTone::unpack(tone.pack()), tone);
        }
    }

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

    /// 一份只有一款主题的 smelt 主题文件，求值后取那款主题。
    fn parse_theme(mode: &str, colors: serde_json::Value) -> crate::theme_config::ThemeConfig {
        let source = serde_json::json!({
            "version": 1,
            "name": "test",
            "themes": [{ "name": "T", "mode": mode, "colors": colors }]
        });
        crate::theme_config::parse_theme_set(&source.to_string())
            .unwrap()
            .themes
            .remove(0)
    }

    /// `Rgba` 量化回 `0xRRGGBBAA`，比较时不受 Hsla 来回换算的浮点误差影响。
    fn rgba_hex(color: Rgba) -> u32 {
        let q = |channel: f32| (channel.clamp(0.0, 1.0) * 255.0).round() as u32;
        (q(color.r) << 24) | (q(color.g) << 16) | (q(color.b) << 8) | q(color.a)
    }

    /// 行底压在字底那层窗口底上：悬停、选中都看得出，选中比悬停深一档；选中行里的次要字
    /// 压在选中底上还够读。深浅主题、有没有材质都要成立。
    #[test]
    fn row_fills_stand_out_on_the_backdrop() {
        for light in [true, false] {
            let p = builtin_fallback(light);
            let colors = *theme_catalog::builtin_for_mode(if light {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            })
            .colors();
            let (hover, active) = (flatten(colors.hover, p.bg), flatten(colors.pressed, p.bg));
            for material in std::iter::once(None).chain(TEST_MATERIALS.into_iter().map(Some)) {
                let backdrop = text_backdrop(material, rgb(p.bg), 0.0, !light);
                // 装主题时先按同一外观的材质配字色，再由色板计算行底。
                let (text, muted) = if material.is_some() {
                    (
                        legible(p.text, backdrop, TEXT_MIN_CONTRAST),
                        legible(p.muted, backdrop, TEXT_MUTED_MIN_CONTRAST),
                    )
                } else {
                    (p.text, p.muted)
                };
                let rows = row_fills(hover, active, p.bg, text, muted, backdrop);
                let hover_under = flatten(rows.hover, backdrop);
                let selected_under = flatten(rows.selected, backdrop);
                let at = format!("light={light} backdrop={backdrop:06x}");
                assert!(
                    (lightness(hover_under) - lightness(backdrop)).abs()
                        >= ROW_HOVER_MIN_LIGHTNESS - 0.3,
                    "{at}"
                );
                assert!(
                    (lightness(selected_under) - lightness(backdrop)).abs()
                        >= ROW_SELECTED_MIN_LIGHTNESS - 0.3,
                    "{at}"
                );
                assert!(rows.selected.a > rows.hover.a, "{at}: 选中要比悬停深");
                assert!(
                    contrast_ratio(rows.muted_selected, selected_under) >= TEXT_MUTED_MIN_CONTRAST,
                    "{at}"
                );
                assert!(
                    contrast_ratio(rows.faint_selected, selected_under) >= TEXT_FAINT_MIN_CONTRAST,
                    "{at}"
                );
            }
        }
    }

    #[test]
    fn row_fill_keeps_a_theme_that_already_stands_out() {
        // 主题配了比保底更深的选中：照主题，不往回收。
        let fill = row_fill(
            0x5a5a58,
            0x1e1e1d,
            0xecece8,
            0x1e1e1d,
            ROW_SELECTED_MIN_LIGHTNESS,
        );
        let themed = (0x5a * 2 + 0x58 - (0x1e * 2 + 0x1d)) as f32
            / (0xec * 2 + 0xe8 - (0x1e * 2 + 0x1d)) as f32;
        assert!((fill.a - themed).abs() < 0.001, "{}", fill.a);
    }

    #[test]
    fn row_fill_turns_a_reversed_theme_toward_the_text() {
        // 深色底上配了比底还暗的选中：方向反了，按保底往正文色那头铺。
        let fill = row_fill(
            0x101010,
            0x1e1e1d,
            0xecece8,
            0x1e1e1d,
            ROW_SELECTED_MIN_LIGHTNESS,
        );
        assert_eq!(rgba_hex(Rgba { a: 1.0, ..fill }), rgba_hex(rgb(0xecece8)));
        let landed = flatten(fill, 0x1e1e1d);
        assert!(
            (lightness(landed) - lightness(0x1e1e1d)).abs() >= ROW_SELECTED_MIN_LIGHTNESS - 0.3
        );
    }

    /// 侧栏那一档压回 `bg` 上，深浅要和原来的实色差不出一级；和 `bg` 一样深时不铺。
    #[test]
    fn shade_lands_on_the_sidebar_fill_over_the_stage() {
        for light in [true, false] {
            let p = builtin_fallback(light);
            let shade = shade_over(p.side, p.bg);
            assert!(shade.a > 0.0 && shade.a < 0.3, "{light}: {}", shade.a);
            let landed = flatten(shade, p.bg);
            let diff = channel_sum(landed) - channel_sum(p.side);
            assert!(diff.abs() <= 6.0, "{light} {:06x} -> {landed:06x}", p.side);
        }
        assert_eq!(shade_over(0x202020, 0x202020).a, 0.0);
    }

    #[test]
    fn each_slot_reads_its_token() {
        let theme = parse_theme(
            "dark",
            serde_json::json!({
                "bg_base": "#111111", "text_base": "#FAFAFA", "page": "#0C0C0C",
                "surface_raised": "#181818", "layer_01": "#202020", "layer_02": "#282828",
                "layer_03": "#303030", "border_base": "#FFFFFF14", "border_strong": "#FFFFFF26",
                "text_muted": "#A3A3A3", "text_faint": "#737373", "bg_contrast": "#FAFAFA",
                "contrast_hover": "#FFFFFF", "contrast_pressed": "#E0E0E0",
                "text_contrast": "#171717", "bg_button_neutral": "#FFFFFF20",
                "hover": "#FFFFFF10", "pressed": "#FFFFFF20",
                "border_focus": "#6EA0FF8C", "selection": "#6EA0FF40", "text_accent": "#89B4FA",
                "info.fg": "#3B82F6", "warning.icon": "#F08A55", "success.icon": "#16A34A",
                "danger.icon": "#DC2626", "scrim": "#00000073",
                "chart.1": "#000001", "chart.2": "#000002", "chart.3": "#000003",
                "chart.4": "#000004", "chart.5": "#000005"
            }),
        );
        let palette = palette_from_tokens(theme.colors(), 0x111111);
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
        assert_eq!(palette.faint, 0x737373);
        assert_eq!(palette.ink, 0xfafafa);
        assert_eq!(palette.ink_hover, 0xffffff);
        assert_eq!(palette.ink_press, 0xe0e0e0);
        assert_eq!(palette.on_ink, 0x171717);
        assert_eq!(
            palette.button_primary.map(rgba_hex),
            [0xfafafaff, 0xffffffff, 0xe0e0e0ff]
        );
        assert_eq!(palette.button_primary_foreground, 0x171717);
        assert_eq!(rgba_hex(palette.button_secondary[0]), 0xffffff20);
        assert!(
            palette.button_secondary[1].a > palette.button_secondary[0].a
                && palette.button_secondary[2].a > palette.button_secondary[1].a,
            "悬停、按下叠在半透明的底上，越叠越实"
        );
        assert_eq!(palette.button_secondary_foreground, 0xfafafa);
        assert_eq!(rgba_hex(palette.focus), 0x6ea0ff8c);
        assert_eq!(rgba_hex(palette.selection), 0x6ea0ff40);
        assert_eq!(palette.link, 0x89b4fa);
        assert_eq!(palette.run, 0x3b82f6);
        assert_eq!(palette.need, 0xf08a55);
        assert_eq!(palette.ok, 0x16a34a);
        assert_eq!(palette.bad, 0xdc2626);
        assert!(contrast_ratio(palette.on_bad, palette.bad) >= TEXT_MUTED_MIN_CONTRAST);
        assert_eq!(rgba_hex(palette.popover_text), 0xfafafaff);
        assert_eq!(rgba_hex(palette.scrim), 0x00000073);
        assert_eq!(palette.chart, [1, 2, 3, 4, 5]);
    }

    /// 主按钮底够深时悬停、按下照主题的叠层往亮提；底是中灰、往亮提就压不住白字时，两档
    /// 一起往深处压，越按越深，白字始终够 4.5:1。
    #[test]
    fn state_fills_flip_only_when_the_overlay_drowns_the_text() {
        let lighten = [rgba(0xfcfcfc1f), rgba(0xfcfcfc3d)];
        let deep = 0x171717;
        assert_eq!(
            state_fills(deep, lighten),
            lighten.map(|o| flatten(o, deep))
        );

        let gray = 0x5c6166;
        let [hover, press] = state_fills(gray, lighten);
        let on_white = |fill| contrast_ratio(0xffffff, fill);
        assert!(on_white(gray) < on_white(hover) && on_white(hover) < on_white(press));
        assert!(on_white(hover) >= TEXT_MUTED_MIN_CONTRAST);
    }

    /// 次要按钮的底本身半透明时，悬停叠层叠在它上面，结果仍透出底下的窗口。
    #[test]
    fn overlays_compose_onto_translucent_fills() {
        let composed = over(rgba(0xffffff10), rgba(0xffffff20));
        assert!(composed.a > 0x20 as f32 / 255.0 && composed.a < 1.0);
        assert_eq!(rgba_hex(Rgba { a: 1.0, ..composed }), 0xffffffff);
        // 128/255 的黑叠在白上：255 × (1 − 128/255) = 127。
        assert_eq!(rgba_hex(over(rgba(0x00000080), rgb(0xffffff))), 0x7f7f7fff);
        assert_eq!(over(rgba(0x00000000), rgba(0x00000000)).a, 0.0);
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

    /// 毛玻璃时 smelt 两套主题的字压在材质上：正文、次要字、弱字都够各自的对比，层次
    /// 还在（正文比次要字清楚，次要字比弱字清楚）。
    #[test]
    fn text_fitted_to_the_material_stays_legible_and_ranked() {
        for tone in TEST_MATERIALS {
            for dark in [true, false] {
                let mut colors = *theme_catalog::builtin_for_mode(if dark {
                    ThemeMode::Dark
                } else {
                    ThemeMode::Light
                })
                .colors();
                let backdrop = text_backdrop(Some(tone), colors.bg_base, 0.0, dark);
                let fit = |color: &mut Rgba, min: f32| {
                    *color = rgb(legible(flatten(*color, backdrop), backdrop, min))
                };
                fit(&mut colors.text_base, TEXT_MIN_CONTRAST);
                fit(&mut colors.text_muted, TEXT_MUTED_MIN_CONTRAST);
                fit(&mut colors.text_faint, TEXT_FAINT_MIN_CONTRAST);
                let [text, muted, faint] =
                    [colors.text_base, colors.text_muted, colors.text_faint].map(pack);
                assert!(contrast_ratio(text, backdrop) >= TEXT_MIN_CONTRAST);
                assert!(contrast_ratio(muted, backdrop) >= TEXT_MUTED_MIN_CONTRAST);
                assert!(contrast_ratio(faint, backdrop) >= TEXT_FAINT_MIN_CONTRAST - 0.01);
                assert!(contrast_ratio(text, backdrop) > contrast_ratio(muted, backdrop));
                assert!(contrast_ratio(muted, backdrop) > contrast_ratio(faint, backdrop));
            }
        }
    }

    /// 本来就够对比的字不动；不够的只挪到刚好够，不一路推到纯白纯黑。
    #[test]
    fn legible_moves_only_as_far_as_needed() {
        assert_eq!(legible(0xecece8, 0x363a40, TEXT_MIN_CONTRAST), 0xecece8);
        let lifted = legible(0xa3a29c, 0x363a40, TEXT_MUTED_MIN_CONTRAST);
        assert!(contrast_ratio(lifted, 0x363a40) >= TEXT_MUTED_MIN_CONTRAST);
        assert!(contrast_ratio(lifted, 0x363a40) < TEXT_MUTED_MIN_CONTRAST + 0.1);
        let darkened = legible(0x6b6a65, 0xd6d6da, TEXT_MUTED_MIN_CONTRAST);
        assert!(relative_luminance(darkened) < relative_luminance(0x6b6a65));
    }

    /// 没有材质时字底就是主题底色，不管底色不透明度设成多少（窗口不透明）。
    #[test]
    fn without_material_the_backdrop_is_the_theme_background() {
        for opacity in [0.0, 0.5, 1.0] {
            assert_eq!(text_backdrop(None, rgb(0x1e1e1d), opacity, true), 0x1e1e1d);
        }
    }

    /// 有材质时字底是底色叠在材质上：铺满是底色，不铺是材质，中间按比例混。
    #[test]
    fn with_material_the_backdrop_blends_theme_background_over_it() {
        let tone = MaterialTone {
            dark: 0x404040,
            light: 0xd0d0d0,
        };
        let bg = rgb(0x000000);
        assert_eq!(text_backdrop(Some(tone), bg, 1.0, true), 0x000000);
        assert_eq!(text_backdrop(Some(tone), bg, 0.0, true), 0x404040);
        assert_eq!(text_backdrop(Some(tone), bg, 0.5, true), 0x202020);
        assert_eq!(
            text_backdrop(Some(tone), rgb(0xffffff), 0.0, false),
            0xd0d0d0
        );
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

    /// 随 App 带的每份主题文件都合格式；投影出的字压在各自的底上都看得清：主按钮三档上的字、
    /// 危险底上的字至少 4.5:1，次要字 4.5:1，最弱字 3:1，四个状态色分得开。浅色主题的正文
    /// 偏浅时主按钮按下那档最容易不够（Ayu Light、Catppuccin Latte、Solarized Light），锁住。
    #[test]
    fn bundled_themes_parse_and_stay_legible() {
        let dirs = [
            concat!(env!("CARGO_MANIFEST_DIR"), "/themes"),
            concat!(env!("CARGO_MANIFEST_DIR"), "/../plugins/themes/themes"),
        ];
        let mut checked = 0;
        for dir in dirs {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_none_or(|ext| ext != "json") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                let set = crate::theme_config::parse_theme_set(&source)
                    .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
                for theme in &set.themes {
                    let name = theme.name();
                    let c = theme.colors();
                    let p = palette_from_tokens(c, pack(c.bg_base));
                    for fill in [p.ink, p.ink_hover, p.ink_press] {
                        let ratio = contrast_ratio(p.on_ink, fill);
                        assert!(
                            ratio >= TEXT_MUTED_MIN_CONTRAST,
                            "{name} 主按钮上的字 {ratio:.2}"
                        );
                    }
                    for fill in p.button_submit.map(pack) {
                        let ratio = contrast_ratio(p.button_submit_foreground, fill);
                        assert!(
                            ratio >= TEXT_MUTED_MIN_CONTRAST,
                            "{name} 图标主按钮上的图标 {ratio:.2}"
                        );
                    }
                    let ratio = contrast_ratio(p.on_bad, p.bad);
                    assert!(
                        ratio >= TEXT_MUTED_MIN_CONTRAST,
                        "{name} 危险底上的字 {ratio:.2}"
                    );
                    assert!(
                        contrast_ratio(p.muted, p.bg) >= TEXT_MUTED_MIN_CONTRAST - 0.01,
                        "{name}"
                    );
                    assert!(
                        contrast_ratio(p.faint, p.bg) >= TEXT_FAINT_MIN_CONTRAST - 0.01,
                        "{name}"
                    );
                    let status = [p.run, p.need, p.ok, p.bad];
                    for (i, a) in status.iter().enumerate() {
                        for b in &status[i + 1..] {
                            assert_ne!(a, b, "{name} 四个状态色要分得开");
                        }
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 30, "只查到 {checked} 款主题");
    }
}
