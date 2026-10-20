//! 对话消息里的原始 HTML 按原文显示，不当标记解释。
//!
//! gpui-component 会把 Markdown 里的原始 HTML 当标记解析，不认识的标签会让整段内容
//! 消失。对话里的尖括号多半是随手写的类型参数、组件名或贴进来的片段，不是要渲染的
//! 页面，所以这里接管解析器认出的 HTML 节点，原样显示源码。
//!
//! 按语法树接管，不在原文上加反斜杠：代码块、行内代码本来就不是 HTML 节点，天然不
//! 受影响。在原文上转义要自己认代码块，缩进的围栏（列表里很常见）、`~~~` 围栏认不
//! 全，误转义后代码里会多出 `\`。
//!
//! 例外：成对的 `<b>`/`<i>`/`<u>`/`<s>`/`<mark>` 等简单格式标签，上游在调用插件之前
//! 就配对成了文字样式（`format/markdown.rs` 的 `inline_groups`），仍按格式显示。它们
//! 不会吞内容。

use gpui::{App, SharedString, Window};
use gpui_component::text::{
    InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
    markdown_ast,
};

/// 行内 HTML（段落里的 `<panel>`、`<section class="x">`），在行内显示成一段原文。
pub(crate) struct LiteralInlineHtml;

/// 块级 HTML（单独成段的 `<div>…`），整块显示原文。
pub(crate) struct LiteralBlockHtml;

const INLINE_NAME: &str = "literal-html";
const BLOCK_NAME: &str = "literal-html-block";

fn html_source(node: &markdown_ast::Node) -> Option<SharedString> {
    let markdown_ast::Node::Html(html) = node else {
        return None;
    };
    Some(html.value.clone().into())
}

impl MarkdownPlugin for LiteralInlineHtml {
    fn name(&self) -> &str {
        INLINE_NAME
    }

    fn parse(
        &self,
        node: &markdown_ast::Node,
        _cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let source = html_source(node)?;
        Some(MarkdownNode::new(INLINE_NAME, ()).text(source))
    }

    /// 不给元素：TextView 退回按原文画成一段不可拆的文字，样式跟着所在的段落走
    /// （粗体、链接色等）。
    fn render_inline(
        &self,
        _node: &MarkdownNode,
        _context: &InlineRenderContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<InlineElement> {
        None
    }
}

impl MarkdownPlugin for LiteralBlockHtml {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        BLOCK_NAME
    }

    fn parse(
        &self,
        node: &markdown_ast::Node,
        _cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let source = html_source(node)?;
        Some(MarkdownNode::new(BLOCK_NAME, ()).text(source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_html_nodes_are_claimed() {
        use markdown_ast::{Html, InlineCode, Node};

        let html = Node::Html(Html {
            value: "<panel>".to_string(),
            position: None,
        });
        let code = Node::InlineCode(InlineCode {
            value: "<panel>".to_string(),
            position: None,
        });

        assert_eq!(html_source(&html).as_deref(), Some("<panel>"));
        assert!(html_source(&code).is_none());
    }
}
