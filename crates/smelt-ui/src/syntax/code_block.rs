//! 富文本（Markdown）里围栏代码块的高亮：按围栏写的语言整块解析一遍，出带颜色的段。
//!
//! 富文本按代码块缓存结果，同一个代码块只解析一次；换主题时高亮器换一个，缓存跟着失效。
//! 富文本视图建的时候拿不到 `App`，所以当前主题的高亮器放在一个全局里，换主题时
//! （`theme_catalog::apply`）换掉。

use std::ops::Range;
use std::sync::{Arc, RwLock};

use gpui::HighlightStyle;
use gpui_base::input::{Rope, SharedHighlightStyleResolver};
use gpui_base::text::CodeBlock;

use super::SyntaxTree;

/// 大于这么多字节的代码块不上色：在界面线程上整块解析，太大会卡一下。
const MAX_CODE_BYTES: usize = 256 * 1024;

/// 富文本视图要的高亮器形状（`TextView::shared_code_block_highlighter`）。
pub type CodeBlockHighlighter =
    Arc<dyn Fn(&CodeBlock) -> Vec<(Range<usize>, HighlightStyle)> + Send + Sync>;

static CURRENT: RwLock<Option<CodeBlockHighlighter>> = RwLock::new(None);

/// 换主题时调：之后建的富文本视图用这套样式给代码块上色。
pub(crate) fn install(resolver: SharedHighlightStyleResolver) {
    let highlighter: CodeBlockHighlighter = Arc::new(move |block| highlight(block, &*resolver));
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(highlighter);
}

/// 当前主题的代码块高亮器；还没装过主题时是 `None`，代码块不上色。
pub fn code_block_highlighter() -> Option<CodeBlockHighlighter> {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn highlight(
    block: &CodeBlock,
    resolver: &dyn gpui_base::input::HighlightStyleResolver,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(language) = block.lang() else {
        return Vec::new();
    };
    let code = block.code();
    if code.len() > MAX_CODE_BYTES {
        return Vec::new();
    }
    let mut tree = SyntaxTree::new(&language);
    if tree.language().is_none() {
        return Vec::new();
    }
    tree.parse(&Rope::from(code.as_ref()), &mut || false);
    let mut styles = tree.styles(0..code.len(), resolver);
    // 富文本只要有颜色的段，没样式的段是正文字色。
    styles.retain(|(_, style)| *style != HighlightStyle::default());
    styles
}

#[cfg(test)]
mod tests {
    use gpui::hsla;

    use super::*;

    struct Keywords;

    impl gpui_base::input::HighlightStyleResolver for Keywords {
        fn style(&self, name: &str) -> Option<HighlightStyle> {
            (name == "keyword").then(|| HighlightStyle {
                color: Some(hsla(0., 1., 0.5, 1.)),
                ..HighlightStyle::default()
            })
        }
    }

    #[test]
    fn colors_only_the_styled_parts_of_known_languages() {
        let block = CodeBlock::from_code("fn main() {}", Some("Rust"));
        let ranges: Vec<_> = highlight(&block, &Keywords)
            .into_iter()
            .map(|(range, _)| range)
            .collect();
        assert_eq!(ranges, vec![0..2]);

        let plain = CodeBlock::from_code("fn main() {}", Some("text"));
        assert!(highlight(&plain, &Keywords).is_empty());
        let unlabelled = CodeBlock::from_code("fn main() {}", None::<&str>);
        assert!(highlight(&unlabelled, &Keywords).is_empty());
    }
}
