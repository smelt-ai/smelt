//! 单行文字，放不下就截短并以省略号收尾。
//!
//! GPUI 的 `.truncate()` 先把每个字符单独排一次、把宽度加起来，用这个和来定截断点，
//! 截完不再按整行实际排版复核一遍。中文标点在单独排和夹在汉字里排时用的不是同一个
//! 回退字体：「：」单独量是半角的 6px，夹在汉字中间就换成苹方的全角 12px。一行里
//! 多几个中文标点，截出来的「前半句…」就比框宽，省略号连同最后几个字被父元素的
//! `overflow_hidden` 裁掉，读着像话说到一半。
//!
//! 这里每次都按整行实际排版的宽度来判断，截断点用二分查找确定。排版结果走 GPUI 的
//! 行排版缓存，重绘时不用重新排。

use gpui::{
    App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, ShapedLine, SharedString, Style, TextAlign, TextStyle, Window,
    px, size,
};

const ELLIPSIS: &str = "…";

/// 单行文字，放不下时末尾截短并加省略号。
///
/// 字体、字号、颜色沿用父元素的文字样式。文字里的换行会排成空格。
pub fn truncated_line(text: impl Into<SharedString>) -> TruncatedLine {
    let text: SharedString = text.into();
    let text = if text.contains(['\n', '\r']) {
        SharedString::from(text.replace(['\r', '\n'], " "))
    } else {
        text
    };
    TruncatedLine { text }
}

pub struct TruncatedLine {
    text: SharedString,
}

pub struct TruncatedLineLayout {
    style: TextStyle,
    font_size: Pixels,
    line_height: Pixels,
}

impl IntoElement for TruncatedLine {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TruncatedLine {
    type RequestLayoutState = TruncatedLineLayout;
    type PrepaintState = Option<ShapedLine>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = window.text_style();
        let rem_size = window.rem_size();
        let font_size = style.font_size.to_pixels(rem_size);
        let line_height = window.pixel_snap(style.line_height_in_pixels(rem_size));
        let text = self.text.clone();
        let run_style = style.clone();
        let layout_id = window.request_measured_layout(
            Style::default(),
            move |known, available, window, _cx| {
                let width = known.width.unwrap_or_else(|| {
                    // 能截短，最窄可以是 0；最宽是整行排开。
                    let natural = || shape(window, &text, font_size, &run_style).width.ceil();
                    match available.width {
                        AvailableSpace::MinContent => px(0.),
                        AvailableSpace::MaxContent => natural(),
                        AvailableSpace::Definite(limit) => natural().min(limit),
                    }
                });
                size(width, line_height)
            },
        );
        (
            layout_id,
            TruncatedLineLayout {
                style,
                font_size,
                line_height,
            },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        Some(fit_line(
            window,
            &self.text,
            layout.font_size,
            &layout.style,
            bounds.size.width,
        ))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        line: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(line) = line.as_ref() {
            let _ = line.paint(
                bounds.origin,
                layout.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }
}

fn shape(window: &Window, text: &SharedString, font_size: Pixels, style: &TextStyle) -> ShapedLine {
    window
        .text_system()
        .shape_line(text.clone(), font_size, &[style.to_run(text.len())], None)
}

/// 放得下就整行；放不下就找能放下「前缀…」的最长前缀。连省略号都放不下时只画省略号，
/// 多出的部分由父元素裁掉。
fn fit_line(
    window: &Window,
    text: &SharedString,
    font_size: Pixels,
    style: &TextStyle,
    max_width: Pixels,
) -> ShapedLine {
    let full = shape(window, text, font_size, style);
    if full.width <= max_width {
        return full;
    }
    let with_ellipsis = |chars: usize| {
        let end = text
            .char_indices()
            .nth(chars)
            .map_or(text.len(), |(ix, _)| ix);
        let prefix = text[..end].trim_end();
        let candidate = SharedString::from(format!("{prefix}{ELLIPSIS}"));
        shape(window, &candidate, font_size, style)
    };
    // 整行放不下，所以最长只试到少一个字。前缀加长时宽度单调不减，可以二分。
    let total = text.chars().count();
    let mut best = with_ellipsis(0);
    let (mut lo, mut hi) = (1, total.saturating_sub(1));
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let line = with_ellipsis(mid);
        if line.width <= max_width {
            best = line;
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::prelude::FluentBuilder as _;
    use gpui::{
        Context, InteractiveElement, ParentElement, Render, Styled,
        VisualTestContext, div,
    };

    /// 截图里的那句：中文标点多，GPUI 的 `.truncate()` 在 352px 宽时截出来 354px。
    const MIXED: &str = "找到关键线索了：多个同类项目 2 天前报告了一模一样的问题——“新注册的匿名身份返回 HTTP 200 空正文”，解决办法是换身份重试。";

    struct Empty;

    impl Render for Empty {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[cfg(target_os = "macos")]
    fn with_real_glyphs(check: impl FnOnce(&mut Window, &TextStyle)) {
        let mut cx = gpui::TestAppContext::build_with_text_system(
            gpui::TestDispatcher::new(0),
            None,
            std::sync::Arc::new(gpui_apple::AppleTextSystem::new()),
        );
        let (_, cx) = cx.add_window_view(|_, _| Empty);
        cx.update(|window, _| {
            let style = window.text_style();
            check(window, &style);
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mixed_cjk_truncation_fits_and_keeps_the_ellipsis() {
        with_real_glyphs(|window, style| {
            let text = SharedString::from(MIXED);
            let font_size = px(12.);
            for max in [120., 200., 352., 500.] {
                let max = px(max);
                let line = fit_line(window, &text, font_size, style, max);
                assert!(
                    line.width <= max,
                    "截出来比框宽，省略号会被裁掉：{:?} > {max:?}，{:?}",
                    line.width,
                    line.text
                );
                assert!(line.text.ends_with(ELLIPSIS), "要以省略号收尾：{:?}", line.text);
                // 不能截过头：差不到两个汉字就该放下了。
                assert!(
                    line.width > max - px(26.),
                    "截得太多：{:?} 只用了 {max:?} 里的 {:?}",
                    line.text,
                    line.width
                );
            }
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn text_that_fits_is_left_whole_and_a_tiny_box_gets_only_the_ellipsis() {
        with_real_glyphs(|window, style| {
            let text = SharedString::from(MIXED);
            let whole = fit_line(window, &text, px(12.), style, px(2000.));
            assert_eq!(whole.text, text);
            let tiny = fit_line(window, &text, px(12.), style, px(4.));
            assert_eq!(tiny.text.as_ref(), ELLIPSIS);
        });
    }

    struct RowProbe {
        text: &'static str,
        fill: bool,
    }

    impl Render for RowProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let fill = self.fill;
            div().w(px(300.)).child(
                div()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(div().size(px(8.)).flex_shrink_0())
                    .child(
                        div()
                            .debug_selector(|| "TEXT".into())
                            .when(fill, |cell| cell.flex_1())
                            .min_w_0()
                            .overflow_hidden()
                            .child(truncated_line(self.text)),
                    ),
            )
        }
    }

    fn painted_width(cx: &mut gpui::TestAppContext, text: &'static str, fill: bool) -> Pixels {
        let (_, cx) = cx.add_window_view(move |_, _| RowProbe { text, fill });
        let cx: &mut VisualTestContext = cx;
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.debug_bounds("TEXT").expect("画过一次").size.width
    }

    /// 长文字不能把行撑宽：放进 `flex_1 min_w_0` 的格子里，只占剩下的宽度。
    #[gpui::test]
    fn long_text_takes_only_the_remaining_width(cx: &mut gpui::TestAppContext) {
        let long = "a very long progress summary that cannot possibly fit in three hundred pixels";
        assert_eq!(painted_width(cx, long, true), px(300. - 8. - 8.));
    }

    /// 不占满时按整行宽度排，短文字不会被拉宽或压成 0。
    #[gpui::test]
    fn short_text_keeps_its_natural_width(cx: &mut gpui::TestAppContext) {
        let width = painted_width(cx, "short", false);
        assert!(width > px(0.) && width < px(100.), "短文字宽度不对：{width:?}");
    }
}
