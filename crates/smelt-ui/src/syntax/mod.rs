//! 代码语法高亮：tree-sitter 解析，按主题的 `syntax` 段上色。
//!
//! - [`SyntaxTree`]：一份文本的语法树，增量解析、注入层（Markdown 围栏里的代码、HTML 里的
//!   脚本等按自己的语言解析）、按范围出样式和折叠段。
//! - [`highlighter_factory`]：接到 gpui-base 编辑器上（文件页的代码编辑器）。
//! - [`code_block_highlighter`]：给富文本的围栏代码块上色（Markdown）。
//! - `editing`：编辑器的语言服务，光标在字符串、注释里时不自动补全引号括号。
//!
//! 样式不在这里定：高亮查询给每段代码一个名字（`keyword`、`string`、`function.method`……），
//! 拿名字去当前主题查（`theme_catalog::ActiveTheme::syntax`），查不到整名再查第一段。
//!
//! 支持的语言见 `language.rs` 的表，语法包随本库编进来。`queries/` 下是改过的查询，出处见
//! `queries/NOTICE`；其余语言用语法包自带的查询。

mod code_block;
pub(crate) mod editing;
mod editor;
mod language;
mod tree;

pub(crate) use code_block::install as install_code_block_highlighter;
pub use code_block::{CodeBlockHighlighter, code_block_highlighter};
pub use editor::highlighter_factory;
pub use language::language_name;
pub use tree::SyntaxTree;
