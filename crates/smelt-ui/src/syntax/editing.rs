//! 编辑器的语言服务（gpui-base 的 `LanguageProvider`）：语言别名、各语言的编辑规则（自动补全
//! 括号引号、缩进），以及光标是不是在字符串或注释里（引号括号在字符串、注释里不自动补全）。
//!
//! 全局一份，`smelt_ui::init` 装上。

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use gpui::{App, SharedString};
use gpui_base::input::language_config::LanguageConfig;
use gpui_base::input::{
    AutoClosingPair, BracketPair, IndentationRules, LanguageProvider, Rope, SyntaxContext,
    SyntaxContextProvider,
};
use regex::Regex;

use super::language::{grammar, language_name};

pub(crate) fn init(cx: &mut App) {
    gpui_base::input::set_language_provider(Rc::new(Languages::new()), cx);
}

struct Languages {
    configs: HashMap<&'static str, Rc<LanguageConfig>>,
    fallback: Rc<LanguageConfig>,
}

impl Languages {
    fn new() -> Self {
        Self {
            configs: ["text", "json", "python"]
                .into_iter()
                .map(|name| (name, Rc::new(language_config(name))))
                .collect(),
            fallback: Rc::new(LanguageConfig::default()),
        }
    }
}

impl LanguageProvider for Languages {
    fn language_name(&self, name: &str) -> SharedString {
        match language_name(name) {
            Some(name) => name.into(),
            None => name.trim().to_lowercase().into(),
        }
    }

    fn config(&self, name: &str) -> Rc<LanguageConfig> {
        self.configs.get(name).unwrap_or(&self.fallback).clone()
    }

    fn syntax_context_provider(&self, name: &str) -> Option<Rc<dyn SyntaxContextProvider>> {
        let grammar = grammar(name)?;
        Some(Rc::new(CursorContext::new(grammar.language.clone())?))
    }
}

/// 各语言的编辑规则。没列的语言用 gpui-base 的默认：圆方花括号和引号自动补全（字符串、注释
/// 里不补），按括号缩进。
fn language_config(language: &str) -> LanguageConfig {
    static PYTHON_INDENT: LazyLock<IndentationRules> = LazyLock::new(|| {
        IndentationRules::new(
            Regex::new(r"[\{\(\[:]\s*$").expect("合法的正则"),
            Regex::new(r"^\s*[\}\)\]]").expect("合法的正则"),
        )
    });
    match language {
        // 纯文本不认括号，也不补全任何东西。
        "text" => LanguageConfig::default()
            .brackets([])
            .auto_closing_pairs([]),
        // JSON 没有单引号和圆括号。
        "json" => LanguageConfig::default()
            .brackets([BracketPair::new("{", "}"), BracketPair::new("[", "]")])
            .auto_closing_pairs(
                [
                    AutoClosingPair::new("{", "}"),
                    AutoClosingPair::new("[", "]"),
                    AutoClosingPair::new("\"", "\""),
                ]
                .into_iter()
                .map(|pair| pair.not_in([SyntaxContext::String, SyntaxContext::Comment])),
            ),
        // 只有 Python 行尾的冒号开新块。
        "python" => LanguageConfig::default().indentation_rules(PYTHON_INDENT.clone()),
        _ => LanguageConfig::default(),
    }
}

/// 光标处是代码、字符串还是注释。按节点名粗分（名字带 `comment` 的是注释、带 `string` 的是
/// 字符串），所有语言通用，不用每种语言写查询。
///
/// 只拿得到文本拿不到改动，没法增量：文本变了就整份重新解析，限 5ms，超时当代码处理。同一份
/// 文本只解析一次（只在键入括号引号时问，不是每次按键）。
struct CursorContext {
    parser: std::cell::RefCell<tree_sitter::Parser>,
    /// 上次问的文本和它的树；树是 `None` 表示那份文本没在时限内解析完。
    tree: std::cell::RefCell<Option<(Rope, Option<tree_sitter::Tree>)>>,
}

impl CursorContext {
    const PARSE_BUDGET: Duration = Duration::from_millis(5);

    fn new(language: tree_sitter::Language) -> Option<Self> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).ok()?;
        Some(Self {
            parser: std::cell::RefCell::new(parser),
            tree: std::cell::RefCell::new(None),
        })
    }

    fn parse(&self, text: &Rope) -> Option<tree_sitter::Tree> {
        let deadline = Instant::now() + Self::PARSE_BUDGET;
        let mut progress = |_: &tree_sitter::ParseState| {
            if Instant::now() >= deadline {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        };
        let options = tree_sitter::ParseOptions::new().progress_callback(&mut progress);
        let mut parser = self.parser.borrow_mut();
        let tree = parser.parse_with_options(
            &mut |offset, _| super::tree::read_bytes(text, offset),
            None,
            Some(options),
        );
        if tree.is_none() {
            // 超时的解析留着进度，下次会接着解析，换了文本就不对了。
            parser.reset();
        }
        tree
    }
}

impl SyntaxContextProvider for CursorContext {
    fn context_at(&self, text: &Rope, offset: usize) -> SyntaxContext {
        let offset = offset.min(text.len());
        let mut cached = self.tree.borrow_mut();
        if cached.as_ref().is_none_or(|(cached, _)| cached != text) {
            *cached = Some((text.clone(), self.parse(text)));
        }
        let Some((_, Some(tree))) = cached.as_ref() else {
            return SyntaxContext::Code;
        };
        context_in(tree, text, offset)
    }
}

fn context_in(tree: &tree_sitter::Tree, text: &Rope, offset: usize) -> SyntaxContext {
    let root = tree.root_node();
    let mut node = root.descendant_for_byte_range(offset, offset);
    // 在文本最末尾可能什么都碰不到，退一个字节。
    if node.is_none() && offset > 0 {
        node = root.descendant_for_byte_range(offset - 1, offset - 1);
    }
    while let Some(current) = node {
        if current.is_named()
            && let Some(context) = classify(current.kind())
        {
            // 在字符串开头的引号前面插字，还在字符串外面；在结尾引号前面插字，在里面。
            let opens_here = offset == current.start_byte()
                && matches!(
                    current.kind(),
                    "string"
                        | "string_literal"
                        | "raw_string_literal"
                        | "template_string"
                        | "string_start"
                );
            if context == SyntaxContext::String && opens_here {
                return SyntaxContext::Code;
            }
            return context;
        }
        node = current.parent();
    }
    // 光标紧跟在行注释末尾时，节点可能选到外层；接着打的字还是注释，块注释的结尾符之后则不是。
    if offset > 0 && !matches!(text.byte(offset - 1), b'\n' | b'\r') {
        let mut previous = root.descendant_for_byte_range(offset - 1, offset);
        while let Some(current) = previous {
            if current.end_byte() == offset
                && classify(current.kind()) == Some(SyntaxContext::Comment)
                && (current.kind() == "line_comment"
                    || (current.kind() == "comment"
                        && starts_line_comment(text, current.start_byte())))
            {
                return SyntaxContext::Comment;
            }
            previous = current.parent();
        }
    }
    SyntaxContext::Code
}

fn classify(kind: &str) -> Option<SyntaxContext> {
    let kind = kind.to_ascii_lowercase();
    // 字符串里嵌的代码（`${…}`、`#{…}`）是代码。
    if kind == "interpolation" || kind == "template_substitution" {
        Some(SyntaxContext::Code)
    } else if kind.contains("comment") {
        Some(SyntaxContext::Comment)
    } else if kind.contains("string") {
        Some(SyntaxContext::String)
    } else {
        None
    }
}

/// 从 `start` 起的注释是不是 `#` 或 `//` 开头的行注释。
fn starts_line_comment(text: &Rope, start: usize) -> bool {
    match text.get_byte(start) {
        Some(b'#') => true,
        Some(b'/') => text.get_byte(start + 1) == Some(b'/'),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(language: &str, source: &str, offset: usize) -> SyntaxContext {
        let provider = Languages::new()
            .syntax_context_provider(language)
            .expect("有语法的语言");
        provider.context_at(&Rope::from(source), offset)
    }

    #[test]
    fn strings_and_comments_are_told_apart_from_code() {
        let json = r#"{"key": "value"}"#;
        assert_eq!(context("json", json, 10), SyntaxContext::String);
        assert_eq!(context("json", json, 0), SyntaxContext::Code);
        assert_eq!(context("json", json, 7), SyntaxContext::Code);
        for (language, source, offset, expected) in [
            ("python", "# note", 6, SyntaxContext::Comment),
            ("python", "# note\n", 7, SyntaxContext::Code),
            ("rust", "// note", 7, SyntaxContext::Comment),
            ("rust", "/// note\n", 8, SyntaxContext::Comment),
            ("rust", "/// note\n", 9, SyntaxContext::Code),
            ("rust", "/* note */", 10, SyntaxContext::Code),
        ] {
            assert_eq!(
                context(language, source, offset),
                expected,
                "{source:?} at {offset}"
            );
        }
    }

    /// 同一个服务问过一份文本，换了文本再问，和新建的服务答得一样。
    #[test]
    fn changed_text_is_parsed_again() {
        let provider = Languages::new().syntax_context_provider("json").unwrap();
        assert_eq!(
            provider.context_at(&Rope::from(r#"{"key": "value"}"#), 10),
            SyntaxContext::String
        );
        assert_eq!(
            provider.context_at(&Rope::from(r#"{"key": 1234567}"#), 10),
            SyntaxContext::Code
        );
    }

    #[test]
    fn aliases_resolve_and_unknown_languages_have_no_context() {
        let languages = Languages::new();
        assert_eq!(languages.language_name("RS"), "rust");
        assert_eq!(languages.language_name("Unknown"), "unknown");
        assert!(
            languages
                .syntax_context_provider("not-a-language")
                .is_none()
        );
        assert!(
            languages
                .config("text")
                .auto_closing_pairs
                .as_ref()
                .is_some_and(Vec::is_empty)
        );
        assert!(languages.config("python").indentation_rules.is_some());
        assert!(languages.config("rust").indentation_rules.is_none());
    }
}
