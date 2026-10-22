//! mermaid 图的配色：从 smelt 当前色板生成 merman 的 `themeVariables`。
//!
//! 走 mermaid 的 `base` 主题。只有它按 `themeVariables` 整套重算；`default` /
//! `dark` 是 mermaid 官网那套淡紫、淡黄，smelt 换成什么主题都不搭。做法参照 Zed
//! 的 `mermaid_render`：节点、连线、文字、分组取界面语义色；饼图、git 分支、思维
//! 导图这类要区分多个系列的图，用几个语义色轮换。
//!
//! `darkMode` 必须写进 `themeVariables`。merman 跟 mermaid.js 一样只从这里读，
//! 写在配置根上不生效。
//!
//! base 主题算完显式变量以后，还会无条件再改两组颜色（对照 merman
//! `theme/base.rs`，即 mermaid.js 的 `theme-base.js`）：
//! - `cScale0..11`：亮色压暗 25，暗色压暗 75（HSL 亮度百分点）；
//! - `git0..7`：亮色压暗 25，暗色提亮 25。
//!
//! 直接传想要的颜色，思维导图在暗色下会变成一片黑。所以先把 HSL 亮度反向挪回去
//! 再交给 merman。写成 `hsl()` 是因为 merman 解析 hsl 时保留色相和饱和度，改亮度
//! 不经过 RGB 来回取整。挪过头会顶到 0 或 100，只能夹住：暗色下分区底最亮 25%，
//! 亮色下最亮 75%。

use std::hash::{Hash, Hasher};

use gpui::{Hsla, rgb};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::ui_theme::{self, Palette};

/// 系列色个数，git 分支正好 8 条。
const SERIES_LEN: usize = 8;
/// base 主题的分区色个数（`THEME_COLOR_LIMIT`），思维导图、时间线、看板用。
const SCALE_LEN: usize = 12;
/// 分区底从卡片底往系列色走多少。
const SECTION_TINT: f32 = 0.3;
/// 便签底从卡片底往黄色走多少。
const NOTE_TINT: f32 = 0.18;

/// 画 mermaid 要用的那几个语义色。只放从色板原样取来的值，派生计算留给
/// `site_config`：渲染路径每帧都要取一次它来算缓存 key。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct DiagramColors {
    light: bool,
    stage: u32,
    column: u32,
    card: u32,
    hover: u32,
    border: u32,
    border_mid: u32,
    border_loud: u32,
    text: u32,
    text_muted: u32,
    accent: u32,
    blue: u32,
    green: u32,
    yellow: u32,
    purple: u32,
    red: u32,
}

impl DiagramColors {
    /// 当前装上的主题。跟侧栏、气泡读同一份色板，不另读 `cx.theme()`，
    /// 免得两边亮暗对不上。
    pub(super) fn current() -> Self {
        Self::from_palette(&ui_theme::palette(), ui_theme::is_light())
    }

    pub(super) fn from_palette(palette: &Palette, light: bool) -> Self {
        Self {
            light,
            stage: palette.bg_stage,
            column: palette.bg_column,
            card: palette.bg_card,
            hover: palette.bg_hover,
            border: palette.border,
            border_mid: palette.border_mid,
            border_loud: palette.border_loud,
            text: palette.text,
            text_muted: palette.text_muted,
            accent: palette.accent,
            blue: palette.blue,
            green: palette.green,
            yellow: palette.yellow,
            purple: palette.purple,
            red: palette.red,
        }
    }

    /// 把整套配色喂进缓存摘要。缓存只在进程内存里，不要求跨版本稳定。
    pub(super) fn feed(&self, digest: &mut Sha256) {
        struct Feed<'a>(&'a mut Sha256);
        impl Hasher for Feed<'_> {
            fn write(&mut self, bytes: &[u8]) {
                self.0.update(bytes);
            }
            fn finish(&self) -> u64 {
                0
            }
        }
        self.hash(&mut Feed(digest));
    }

    /// 要区分多个系列时轮换的颜色：五个语义色，再补三个两两混出来的。
    fn series(&self) -> [u32; SERIES_LEN] {
        [
            self.blue,
            self.green,
            self.purple,
            self.yellow,
            self.red,
            ui_theme::mix(self.blue, self.green, 0.5),
            ui_theme::mix(self.purple, self.red, 0.5),
            ui_theme::mix(self.yellow, self.green, 0.5),
        ]
    }

    /// 压在 `background` 上的字：正文和舞台底里，对比更高的那个。
    fn label_on(&self, background: u32) -> u32 {
        if ui_theme::contrast_ratio(self.text, background)
            >= ui_theme::contrast_ratio(self.stage, background)
        {
            self.text
        } else {
            self.stage
        }
    }

    /// 交给 `Engine::with_site_config` 的整份站点配置。
    ///
    /// 变量表用数组收集，不写成一个大 `json!`：条目多了会撞宏的递归上限。
    pub(super) fn site_config(&self, font_family: &str) -> Value {
        let text = css_hex(self.text);
        let stage = css_hex(self.stage);
        let column = css_hex(self.column);
        let card = css_hex(self.card);
        let hover = css_hex(self.hover);
        let border = css_hex(self.border_mid);
        let line = css_hex(self.text_muted);
        let cluster_border = css_hex(self.border);
        let actor_line = css_hex(self.border_loud);
        let note = css_hex(ui_theme::mix(self.card, self.yellow, NOTE_TINT));
        let note_border = css_hex(self.yellow);
        let point = css_hex(self.accent);

        let fixed: &[(&str, &String)] = &[
            ("background", &stage),
            ("primaryColor", &card),
            ("primaryTextColor", &text),
            ("primaryBorderColor", &border),
            ("secondaryColor", &hover),
            ("secondaryTextColor", &text),
            ("secondaryBorderColor", &border),
            ("tertiaryColor", &column),
            ("tertiaryTextColor", &text),
            ("tertiaryBorderColor", &border),
            // 流程图、通用文字和连线
            ("mainBkg", &card),
            ("nodeBkg", &card),
            ("nodeBorder", &border),
            ("nodeTextColor", &text),
            ("textColor", &text),
            ("titleColor", &text),
            ("labelTextColor", &text),
            ("labelColor", &text),
            ("classText", &text),
            ("scaleLabelColor", &text),
            ("lineColor", &line),
            ("arrowheadColor", &line),
            ("transitionColor", &line),
            ("edgeLabelBackground", &stage),
            ("clusterBkg", &column),
            ("clusterBorder", &cluster_border),
            // 便签
            ("noteBkgColor", &note),
            ("noteBorderColor", &note_border),
            ("noteTextColor", &text),
            // 时序图
            ("actorBkg", &card),
            ("actorBorder", &border),
            ("actorTextColor", &text),
            ("actorLineColor", &actor_line),
            ("signalColor", &text),
            ("signalTextColor", &text),
            ("sequenceNumberColor", &stage),
            ("labelBoxBkgColor", &card),
            ("labelBoxBorderColor", &border),
            ("loopTextColor", &text),
            ("activationBkgColor", &hover),
            ("activationBorderColor", &border),
            // 状态图、ER
            ("stateBkg", &card),
            ("stateLabelColor", &text),
            ("attributeBackgroundColorOdd", &card),
            ("attributeBackgroundColorEven", &stage),
            // 饼图
            ("pieTitleTextColor", &text),
            ("pieSectionTextColor", &text),
            ("pieLegendTextColor", &text),
            ("pieStrokeColor", &stage),
            ("pieOuterStrokeColor", &border),
            // 象限图
            ("quadrant1Fill", &card),
            ("quadrant2Fill", &hover),
            ("quadrant3Fill", &card),
            ("quadrant4Fill", &hover),
            ("quadrant1TextFill", &text),
            ("quadrant2TextFill", &text),
            ("quadrant3TextFill", &text),
            ("quadrant4TextFill", &text),
            ("quadrantPointFill", &point),
            ("quadrantPointTextFill", &text),
            ("quadrantTitleFill", &text),
            ("quadrantXAxisTextFill", &text),
            ("quadrantYAxisTextFill", &text),
            ("quadrantExternalBorderStrokeFill", &border),
            ("quadrantInternalBorderStrokeFill", &border),
        ];
        let mut vars: Map<String, Value> = fixed
            .iter()
            .map(|(key, value)| ((*key).to_string(), Value::from(value.as_str())))
            .collect();
        vars.insert("darkMode".into(), Value::Bool(!self.light));
        vars.insert("fontFamily".into(), font_family.into());

        let series = self.series();
        let (scale_shift, git_shift) = if self.light {
            (-25.0, -25.0)
        } else {
            (-75.0, 25.0)
        };
        for index in 0..SCALE_LEN {
            let color = series[index % SERIES_LEN];
            let section = ui_theme::mix(self.card, color, SECTION_TINT);
            vars.insert(
                format!("cScale{index}"),
                precompensated(section, scale_shift).into(),
            );
            vars.insert(format!("cScaleLabel{index}"), text.clone().into());
            vars.insert(format!("pie{}", index + 1), css_hex(color).into());
        }
        for (index, color) in series.iter().copied().enumerate() {
            vars.insert(
                format!("git{index}"),
                precompensated(color, git_shift).into(),
            );
            vars.insert(
                format!("gitBranchLabel{index}"),
                css_hex(self.label_on(color)).into(),
            );
            vars.insert(
                format!("fillType{index}"),
                css_hex(ui_theme::mix(self.card, color, SECTION_TINT)).into(),
            );
        }

        json!({
            "theme": "base",
            "fontFamily": font_family,
            "themeVariables": vars,
        })
    }
}

fn css_hex(color: u32) -> String {
    format!("#{:06x}", color & 0x00ff_ffff)
}

/// merman 会在 base 主题算完以后给这个颜色的 HSL 亮度加上 `shift`（负数是压暗）。
/// 先减回去，加完正好落在 `target`；超出 0..=100 的部分只能夹住。
fn precompensated(target: u32, shift: f32) -> String {
    let hsla = Hsla::from(rgb(target));
    let hue = (hsla.h * 360.0).round() % 360.0;
    let saturation = (hsla.s * 100.0).round();
    let lightness = (hsla.l * 100.0).round();
    let before = (lightness - shift).clamp(0.0, 100.0);
    format!("hsl({hue}, {saturation}%, {before}%)")
}

#[cfg(test)]
mod tests {
    use sha2::Digest as _;

    use super::*;

    fn fallback(light: bool) -> DiagramColors {
        DiagramColors::from_palette(&ui_theme::builtin_fallback(light), light)
    }

    #[test]
    fn precompensated_undoes_the_base_theme_shift() {
        // 0x333333 的亮度正好 20%，压暗 75 之前得是 95%。
        assert_eq!(precompensated(0x333333, -75.0), "hsl(0, 0%, 95%)");
        // 亮色分区压暗 25：60% 先抬到 85%。
        assert_eq!(precompensated(0x999999, -25.0), "hsl(0, 0%, 85%)");
        // 顶到 100 就夹住，merman 压完只剩 25%。
        assert_eq!(precompensated(0x999999, -75.0), "hsl(0, 0%, 100%)");
        // 暗色 git 提亮 25：5% 只能从 0 开始。
        assert_eq!(precompensated(0x0d0d0d, 25.0), "hsl(0, 0%, 0%)");
    }

    #[test]
    fn precompensated_keeps_hue_and_saturation() {
        // 纯蓝：240°、100%、50%。
        assert_eq!(precompensated(0x0000ff, -25.0), "hsl(240, 100%, 75%)");
    }

    #[test]
    fn dark_mode_goes_into_theme_variables() {
        let dark = fallback(false).site_config("Arial");
        assert_eq!(dark["theme"], "base");
        assert_eq!(dark["themeVariables"]["darkMode"], true);
        assert!(
            dark.get("darkMode").is_none(),
            "根上的 darkMode merman 不读"
        );
        let light = fallback(true).site_config("Arial");
        assert_eq!(light["themeVariables"]["darkMode"], false);
    }

    #[test]
    fn digest_changes_with_any_color() {
        let base = fallback(false);
        let mut changed = base;
        changed.card ^= 0x010101;
        let digest = |colors: &DiagramColors| {
            let mut hasher = Sha256::new();
            colors.feed(&mut hasher);
            hasher.finalize()
        };
        assert_eq!(digest(&base), digest(&base));
        assert_ne!(digest(&base), digest(&changed));
    }

    #[test]
    fn flowchart_uses_palette_colors() {
        let mut colors = fallback(false);
        colors.card = 0x123456;
        colors.text = 0xabcdef;
        colors.text_muted = 0x654321;
        let svg =
            super::super::render_svg("flowchart TD\nA[开始]-->B[结束]", colors).expect("应能渲染");
        let lower = svg.to_ascii_lowercase();
        assert!(lower.contains("#123456"), "节点底应取卡片色");
        assert!(lower.contains("#abcdef"), "节点字应取正文色");
        assert!(lower.contains("#654321"), "连线应取弱字色");
        assert!(
            !lower.contains("#ececff"),
            "不该再出现 mermaid 默认主题的淡紫"
        );
        assert!(
            !lower.contains("#9370db"),
            "不该再出现 mermaid 默认主题的紫边"
        );
    }

    #[test]
    fn dark_mindmap_sections_are_not_crushed_to_black() {
        let colors = fallback(false);
        let svg = super::super::render_svg("mindmap\n  root((根))\n    一\n    二", colors)
            .expect("应能渲染");
        // cScale1 是第一个分区。merman 压暗 75 以后应当正好落回想要的亮度。
        let section = ui_theme::mix(colors.card, colors.series()[1], SECTION_TINT);
        let hsla = Hsla::from(rgb(section));
        let expected = format!(
            "hsl({}, {}%, {}%)",
            (hsla.h * 360.0).round() % 360.0,
            (hsla.s * 100.0).round(),
            (hsla.l * 100.0).round().min(25.0),
        );
        assert!(svg.contains(&expected), "思维导图分区应为 {expected}");
    }
}
