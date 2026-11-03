//! 文件预览的语法高亮：syntect 解析 + two-face 的语法集，只输出语义类别，颜色由 Dart 的
//! 主题给。设计和实测见 `docs/remote-mobile-files.md` 第 5 节。
//!
//! 整篇一次解析太慢（手机上约 100 µs/行，两万行要两秒），所以按窗口取：Dart 滚到哪取
//! 到哪，解析状态每 [`CHECKPOINT_EVERY`] 行存一份，取后面的窗口从最近的检查点接着解析，
//! 顺序往下滚不重复解析。

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Mutex, OnceLock};

use syntect::easy::ScopeRangeIterator;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxSet};

use crate::api::files::{HighlightedLine, TokenKind, TokenSpan};

/// 超过这么大的文件只显示纯文本：解析状态和检查点都要占内存，跳到末尾也要等太久。
pub(crate) const HIGHLIGHT_MAX_BYTES: usize = 2 * 1024 * 1024;
/// 有一行超过这么长（压缩过的 js、json）整篇不高亮：正则在超长行上会很慢。
pub(crate) const HIGHLIGHT_MAX_LINE: usize = 10_000;
/// 每隔多少行存一份解析状态。
const CHECKPOINT_EVERY: usize = 1000;

/// 语法集只加载一次，第一次打开文本文件时才加载（约 5 ms）。
fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// 一份打开着的文本。正文留在 Rust，Dart 按行取。
pub(crate) struct Document {
    text: String,
    /// 每行在 `text` 里的字节范围，不含换行符。
    lines: Vec<Range<usize>>,
    /// 认出来的语言名（给人看）；认不出是 `None`。
    language: Option<String>,
    /// 最长一行的字数，建文档时数一次（Dart 在界面线程上同步取）。
    max_line_chars: usize,
    /// 语法在语法集里的下标；`None` 不高亮（认不出、太大、行太长）。
    syntax: Option<usize>,
    parser: Mutex<Parser>,
}

// SAFETY: 不自动 Send 只因为 oniguruma 版的 `ParseState` 里存着上一次匹配的捕获组
// （`onig::Region`，裸指针指向它自己 malloc 的数组）。那块内存只归这个 Region 所有，
// 不和正则对象或线程绑定，换线程读写、释放都没问题；onig crate 只是没标。并发访问由
// `Document::parser` 的 Mutex 排开。
unsafe impl Send for Parser {}

struct Parser {
    /// 第 i 份是第 `i * CHECKPOINT_EVERY` 行开始前的状态。第 0 份一开始就有。
    checkpoints: Vec<(ParseState, ScopeStack)>,
    /// scope → 类别。同一个 scope 在一份文件里会出现成千上万次，转字符串再比前缀很贵。
    kinds: HashMap<Scope, Option<TokenKind>>,
    /// 语法报错后整篇退回纯文本，不再重试。
    failed: bool,
}

impl Document {
    /// `path` 用来认语言（文件名、扩展名），认不出再看首行（shebang）。
    pub(crate) fn new(path: &str, text: String) -> Self {
        let lines = line_ranges(&text);
        let set = syntaxes();
        let syntax = detect_syntax(set, path, lines.first().map(|range| &text[range.clone()]));
        let language = syntax.map(|index| set.syntaxes()[index].name.clone());
        let too_big = text.len() > HIGHLIGHT_MAX_BYTES
            || lines.iter().any(|range| range.len() > HIGHLIGHT_MAX_LINE);
        let syntax = syntax.filter(|_| !too_big);
        let checkpoints = syntax
            .map(|index| vec![(ParseState::new(&set.syntaxes()[index]), ScopeStack::new())])
            .unwrap_or_default();
        let max_line_chars = lines
            .iter()
            .map(|range| text[range.clone()].chars().count())
            .max()
            .unwrap_or(0);
        Self {
            text,
            lines,
            language,
            max_line_chars,
            syntax,
            parser: Mutex::new(Parser {
                checkpoints,
                kinds: HashMap::new(),
                failed: false,
            }),
        }
    }

    pub(crate) fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub(crate) fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// 会不会上色。认出了语言但文件太大时是 `false`。
    pub(crate) fn highlighted(&self) -> bool {
        self.syntax.is_some()
    }

    /// 最长一行有几个字（Unicode 标量）。不换行显示时用来定横向能滚多宽。
    pub(crate) fn max_line_chars(&self) -> usize {
        self.max_line_chars
    }

    pub(crate) fn byte_len(&self) -> usize {
        self.text.len()
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// `[start, end)` 行，0 基。越界的部分不返回。
    pub(crate) fn lines(&self, start: usize, end: usize) -> Vec<HighlightedLine> {
        let end = end.min(self.lines.len());
        if start >= end {
            return Vec::new();
        }
        let Some(syntax) = self.syntax else {
            return self.plain(start, end);
        };
        let mut parser = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        if parser.failed {
            return self.plain(start, end);
        }
        match self.parse(&mut parser, syntax, start, end) {
            Some(lines) => lines,
            None => {
                parser.failed = true;
                self.plain(start, end)
            }
        }
    }

    fn plain(&self, start: usize, end: usize) -> Vec<HighlightedLine> {
        (start..end)
            .map(|ix| {
                let text = &self.text[self.lines[ix].clone()];
                HighlightedLine {
                    number: ix as u32 + 1,
                    spans: if text.is_empty() {
                        Vec::new()
                    } else {
                        vec![TokenSpan {
                            text: text.to_string(),
                            kind: TokenKind::Plain,
                        }]
                    },
                }
            })
            .collect()
    }

    /// 从 `start` 之前最近的检查点接着解析到 `end`，沿途补上检查点。语法报错返回 `None`。
    fn parse(
        &self,
        parser: &mut Parser,
        syntax: usize,
        start: usize,
        end: usize,
    ) -> Option<Vec<HighlightedLine>> {
        let set = syntaxes();
        debug_assert!(set.syntaxes().get(syntax).is_some());
        let from = (start / CHECKPOINT_EVERY).min(parser.checkpoints.len() - 1);
        let (mut state, mut stack) = parser.checkpoints[from].clone();
        let mut out = Vec::with_capacity(end - start);
        // 带换行解析：语法集是 newlines 版，很多规则靠 `\n` 收尾。
        let mut line = String::new();
        for ix in from * CHECKPOINT_EVERY..end {
            if ix % CHECKPOINT_EVERY == 0 && ix / CHECKPOINT_EVERY == parser.checkpoints.len() {
                parser.checkpoints.push((state.clone(), stack.clone()));
            }
            let body = &self.text[self.lines[ix].clone()];
            line.clear();
            line.push_str(body);
            line.push('\n');
            let ops = state.parse_line(&line, set).ok()?;
            if ix < start {
                for (_, op) in &ops {
                    stack.apply(op).ok()?;
                }
                continue;
            }
            let mut spans: Vec<TokenSpan> = Vec::new();
            for (range, op) in ScopeRangeIterator::new(&ops, &line) {
                stack.apply(op).ok()?;
                // 换行符只用来收尾语法规则，不显示。
                let range = range.start.min(body.len())..range.end.min(body.len());
                if range.is_empty() {
                    continue;
                }
                let kind = token_kind(&stack, &mut parser.kinds);
                match spans.last_mut() {
                    Some(last) if last.kind == kind => last.text.push_str(&body[range]),
                    _ => spans.push(TokenSpan {
                        text: body[range].to_string(),
                        kind,
                    }),
                }
            }
            out.push(HighlightedLine {
                number: ix as u32 + 1,
                spans,
            });
        }
        Some(out)
    }
}

fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (ix, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            let end = if ix > start && text.as_bytes()[ix - 1] == b'\r' {
                ix - 1
            } else {
                ix
            };
            lines.push(start..end);
            start = ix + 1;
        }
    }
    // 末尾没有换行的最后一行；空文件也算一行空行，和编辑器一致。
    if start < text.len() || lines.is_empty() {
        lines.push(start..text.len());
    }
    lines
}

/// 先按整个文件名（`Dockerfile`、`Makefile` 在语法里是当扩展名登记的），再按扩展名，
/// 最后看首行。认成「Plain Text」等于没认出来。
fn detect_syntax(set: &SyntaxSet, path: &str, first_line: Option<&str>) -> Option<usize> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let by_name = set.find_syntax_by_extension(name);
    let by_extension = name
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .and_then(|(_, ext)| {
            set.find_syntax_by_extension(ext)
                .or_else(|| set.find_syntax_by_extension(&ext.to_ascii_lowercase()))
        });
    let found = by_name
        .or(by_extension)
        .or_else(|| first_line.and_then(|line| set.find_syntax_by_first_line(line)))?;
    if found.name == "Plain Text" {
        return None;
    }
    set.syntaxes()
        .iter()
        .position(|syntax| std::ptr::eq(syntax, found))
}

/// 从最里层的 scope 往外找第一个认得的类别。字符串、注释两头的引号和 `//` 是
/// `punctuation.definition.*`，跳过它们，跟着外层的字符串、注释上色。
fn token_kind(stack: &ScopeStack, kinds: &mut HashMap<Scope, Option<TokenKind>>) -> TokenKind {
    for scope in stack.as_slice().iter().rev() {
        let kind = *kinds
            .entry(*scope)
            .or_insert_with(|| kind_of_scope(&scope.build_string()));
        if let Some(kind) = kind {
            return kind;
        }
    }
    TokenKind::Plain
}

/// 一个 scope 名归哪个类别；不归任何类别是 `None`（交给外层 scope）。顺序有讲究：
/// 越具体的前缀越靠前。
pub(crate) fn kind_of_scope(scope: &str) -> Option<TokenKind> {
    const RULES: &[(&str, TokenKind)] = &[
        ("comment", TokenKind::Comment),
        // Markdown 链接的地址在 `string.other.link` 里，要排在 `string` 前面。
        ("string.other.link", TokenKind::Link),
        ("string", TokenKind::String),
        ("markup.raw", TokenKind::String),
        ("constant.numeric", TokenKind::Number),
        ("constant", TokenKind::Constant),
        ("support.constant", TokenKind::Constant),
        ("variable.language", TokenKind::Constant),
        ("keyword.operator", TokenKind::Operator),
        ("keyword", TokenKind::Keyword),
        ("storage", TokenKind::Keyword),
        ("entity.name.function", TokenKind::Function),
        ("support.function", TokenKind::Function),
        ("variable.function", TokenKind::Function),
        ("entity.name.tag", TokenKind::Tag),
        ("entity.other.attribute-name", TokenKind::Attribute),
        ("entity.other.inherited-class", TokenKind::Type),
        ("entity.name.type", TokenKind::Type),
        ("entity.name.class", TokenKind::Type),
        ("entity.name.struct", TokenKind::Type),
        ("entity.name.enum", TokenKind::Type),
        ("entity.name.trait", TokenKind::Type),
        ("entity.name.interface", TokenKind::Type),
        ("entity.name.union", TokenKind::Type),
        ("support.type", TokenKind::Type),
        ("support.class", TokenKind::Type),
        ("variable.parameter", TokenKind::Variable),
        ("markup.heading", TokenKind::Heading),
        ("markup.underline.link", TokenKind::Link),
        ("markup.link", TokenKind::Link),
        ("markup.inserted", TokenKind::Inserted),
        ("markup.deleted", TokenKind::Deleted),
        ("markup.bold", TokenKind::Emphasis),
        ("markup.italic", TokenKind::Emphasis),
    ];
    if scope.starts_with("punctuation.definition") {
        return None;
    }
    if scope.starts_with("punctuation") {
        return Some(TokenKind::Punctuation);
    }
    RULES
        .iter()
        .find(|(prefix, _)| {
            scope == *prefix
                || scope
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
        .map(|(_, kind)| *kind)
}
