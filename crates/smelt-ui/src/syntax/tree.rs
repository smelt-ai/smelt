//! 一份文本的语法树：增量解析、注入层、按范围出样式、出折叠段。
//!
//! 文本改了先 [`SyntaxTree::edit`] 把旧树按改动挪好，再 [`SyntaxTree::parse`]：tree-sitter 拿
//! 挪好的旧树只重解析改到的地方。解析可以中途叫停（前台有时间限制、后台有更新的改动），叫停时
//! 留着挪好的旧树，样式照旧能出，只是新打的字暂时没颜色。
//!
//! 嵌在里面的别种语言（Markdown 围栏里的代码、HTML 里的脚本、Rust 宏里的代码）按注入查询找出来，
//! 每处用自己的语法另外解析成一层，限定只解析那几段（`set_included_ranges`）。只嵌一层：层里
//! 再嵌的不解析。层的颜色盖在宿主上。
//!
//! 同一个节点被几条高亮模式命中时，查询里靠后的那条算数（和 tree-sitter 自己的高亮器一致）；
//! 嵌套的节点里层盖外层，样式按字段叠：里层没写的字段（比如粗细）沿用外层的。

use std::collections::{BTreeSet, HashMap};
use std::ops::{ControlFlow, Range};
use std::sync::Arc;

use gpui::{HighlightStyle, SharedString};
use gpui_base::input::{FoldRange, HighlightStyleResolver, Rope};
use tree_sitter::{
    InputEdit, Node, ParseOptions, ParseState, Parser, Query, QueryCursor, StreamingIterator as _,
    Tree,
};

use super::language::{Grammar, grammar};

/// 注入层的总量上限。超了的部分不解析、不上色，避免一份巨大的 Markdown 把解析拖住。
const MAX_INJECTION_RANGES: usize = 4096;
const MAX_INJECTION_BYTES: usize = 512 * 1024;
/// 不合并的注入（每个围栏代码块一层）最多解析这么多层。
const MAX_SEPARATE_LAYERS: usize = 512;
/// `@injection.language` 捕获到的语言名最长这么多字节，再长的不当语言名。
const MAX_LANGUAGE_NAME_BYTES: usize = 64;

/// 解析被叫停了。
struct Stopped;

/// 一份文本和它的语法树。没有语法的语言（纯文本、不认识的）只记文本，不出样式也不出折叠。
///
/// 可以整份克隆（树和文本都是共享引用计数的）送到后台解析，解析完再换回来。
#[derive(Clone)]
pub struct SyntaxTree {
    grammar: Option<Arc<Grammar>>,
    text: Rope,
    tree: Option<Tree>,
    layers: Vec<Layer>,
}

#[derive(Clone)]
struct Layer {
    grammar: Arc<Grammar>,
    /// 这层解析的几段，按起点排好。
    ranges: Vec<tree_sitter::Range>,
    /// 几段合起来的首尾。
    bytes: Range<usize>,
    tree: Tree,
    combined: bool,
}

impl SyntaxTree {
    /// `language` 是语言名或别名，不分大小写。
    pub fn new(language: &str) -> Self {
        Self {
            grammar: grammar(language),
            text: Rope::new(),
            tree: None,
            layers: Vec::new(),
        }
    }

    /// 语言的规范名；没有语法时是 `None`。
    pub fn language(&self) -> Option<&'static str> {
        self.grammar.as_ref().map(|grammar| grammar.name)
    }

    /// 最近一次 [`Self::parse`] 收到的文本（叫停了也算）。
    pub fn text(&self) -> &Rope {
        &self.text
    }

    /// 改动之前先调：把旧树和各层按改动挪位置。可以连着调几次，再统一解析。
    pub fn edit(&mut self, edit: &InputEdit) {
        if let Some(tree) = &mut self.tree {
            tree.edit(edit);
        }
        for layer in &mut self.layers {
            layer.tree.edit(edit);
            for range in &mut layer.ranges {
                edit.edit_range(range);
            }
            if let Some(bytes) = bounding_bytes(&layer.ranges) {
                layer.bytes = bytes;
            }
        }
    }

    /// 不知道改了哪里（整份换了文本）时调：丢掉旧树，下次从头解析。
    pub fn reset(&mut self) {
        self.tree = None;
        self.layers.clear();
    }

    /// 换上新文本，先不解析：样式和折叠沿用挪好的旧树，之后再 [`Self::parse`]（比如到后台）。
    pub fn defer(&mut self, text: &Rope) {
        self.text = text.clone();
    }

    /// 按 `text` 解析，旧树已经用 [`Self::edit`] 挪好。`stop` 解析途中会被反复问，返回 true 就
    /// 停下；停下时返回 false，旧树留着，文本已经换成 `text`。
    pub fn parse(&mut self, text: &Rope, stop: &mut dyn FnMut() -> bool) -> bool {
        self.text = text.clone();
        let Some(grammar) = self.grammar.clone() else {
            return true;
        };
        match parse(&grammar.language, &[], self.tree.as_ref(), text, stop) {
            Ok(tree) => self.tree = tree,
            Err(Stopped) => return false,
        }
        match self.parse_layers(&grammar, stop) {
            Ok(layers) => {
                self.layers = layers;
                true
            }
            Err(Stopped) => false,
        }
    }

    /// 按注入查询找出嵌着的别种语言，一处一层（或同语言并成一层）解析。旧层按语言和位置能
    /// 对上就拿来增量解析。
    fn parse_layers(
        &self,
        grammar: &Grammar,
        stop: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<Layer>, Stopped> {
        let (Some(injections), Some(tree)) = (&grammar.injections, &self.tree) else {
            return Ok(Vec::new());
        };
        let Some(content_capture) = injections.content else {
            return Ok(Vec::new());
        };
        let text = &self.text;
        let old_separate: HashMap<(&str, Vec<(usize, usize)>), &Tree> = self
            .layers
            .iter()
            .filter(|layer| !layer.combined)
            .map(|layer| ((layer.grammar.name, range_key(&layer.ranges)), &layer.tree))
            .collect();
        let old_combined: HashMap<&str, &Tree> = self
            .layers
            .iter()
            .filter(|layer| layer.combined)
            .map(|layer| (layer.grammar.name, &layer.tree))
            .collect();

        // 同一个信息串（`rust`、`rs`……）只查一次语言表。
        let mut resolved: HashMap<String, Option<Arc<Grammar>>> = HashMap::new();
        let mut combined: Vec<(Arc<Grammar>, Budget)> = Vec::new();
        let mut layers = Vec::new();
        let mut separate = 0usize;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&injections.query, tree.root_node(), RopeText(text));
        while let Some(found) = matches.next() {
            let pattern = &injections.patterns[found.pattern_index];
            if !pattern.combined && separate >= MAX_SEPARATE_LAYERS {
                continue;
            }
            let name = match &pattern.language {
                Some(name) => Some(name.to_string()),
                None => found
                    .captures
                    .iter()
                    .find(|capture| Some(capture.index) == injections.language)
                    .and_then(|capture| language_text(text, capture.node.byte_range())),
            };
            let Some(name) = name else {
                continue;
            };
            let injected = resolved
                .entry(name)
                .or_insert_with_key(|name| super::language::grammar(name))
                .clone();
            let Some(injected) = injected else {
                continue;
            };
            let mut ranges = Vec::new();
            for capture in found.captures.iter().filter(|c| c.index == content_capture) {
                content_ranges(capture.node, pattern.include_children, &mut ranges);
            }
            if injected.name == "markdown_inline" {
                ranges.retain(|range| has_inline_markup(text, range.start_byte..range.end_byte));
            }
            if ranges.is_empty() {
                continue;
            }
            sort_ranges(&mut ranges);
            if pattern.combined {
                let index = match combined.iter().position(|(g, _)| g.name == injected.name) {
                    Some(index) => index,
                    None => {
                        combined.push((injected, Budget::default()));
                        combined.len() - 1
                    }
                };
                combined[index].1.extend(ranges);
                continue;
            }
            let mut budget = Budget::default();
            if !budget.extend(ranges) {
                continue;
            }
            separate += 1;
            let old = old_separate
                .get(&(injected.name, range_key(&budget.ranges)))
                .copied();
            if let Some(layer) = parse_layer(injected, budget.ranges, old, false, text, stop)? {
                layers.push(layer);
            }
        }

        for (injected, budget) in combined {
            let mut ranges = budget.ranges;
            sort_ranges(&mut ranges);
            if injected.name == "markdown_inline" {
                ranges = keep_inline_separators(ranges);
            }
            let old = old_combined.get(injected.name).copied();
            if let Some(layer) = parse_layer(injected, ranges, old, true, text, stop)? {
                layers.push(layer);
            }
        }
        layers.sort_by_key(|layer| layer.bytes.start);
        Ok(layers)
    }

    /// `range` 里的样式：按顺序、不重叠、铺满整个范围（没颜色的段给默认样式），这是编辑器要的
    /// 形状。`resolver` 按高亮名查样式，查不到的名字不上色。
    pub fn styles(
        &self,
        range: Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let range = range.start.min(self.text.len())..range.end.min(self.text.len());
        let mut spans = Vec::new();
        for (query, found) in self.captures(&range) {
            // 同一个高亮名只查一次样式；查不到的不上色，外层的颜色照常露出来。
            let names = query.capture_names();
            let mut cache: Vec<Option<Option<HighlightStyle>>> = vec![None; names.len()];
            for (span, capture) in found {
                let style = *cache[capture as usize]
                    .get_or_insert_with(|| resolver.style(names[capture as usize]));
                if let Some(style) = style {
                    spans.push((span, style));
                }
            }
        }
        let styles = paint(&range, &spans);
        // 后台解析没回来时树是旧的，节点边界可能落在新文本的字中间；排版切在字中间会崩，
        // 所以对齐到字边界。
        let mut snapped: Vec<(Range<usize>, HighlightStyle)> = Vec::with_capacity(styles.len());
        for (span, style) in styles {
            let start = self.text.floor_char_boundary(span.start);
            let end = self.text.floor_char_boundary(span.end);
            match snapped.last_mut() {
                Some((last, _)) if start < last.end => last.end = last.end.max(end),
                _ if start < end => snapped.push((start..end, style)),
                _ => {}
            }
        }
        if let Some((last, _)) = snapped.last_mut() {
            last.end = range.end;
        }
        if let Some((first, _)) = snapped.first_mut() {
            first.start = range.start;
        }
        if snapped.is_empty() && !range.is_empty() {
            snapped.push((range, HighlightStyle::default()));
        }
        snapped
    }

    /// 整份文本的高亮名和范围，按画的先后排：外层先、里层后，嵌着的语言在最后；后面的盖前面的，
    /// 只盖它设了的那几项。差异视图在后台先记下名字，画的时候才按主题查样式，换主题不用重新解析。
    pub fn capture_names(&self) -> Vec<(Range<usize>, SharedString)> {
        let mut out = Vec::new();
        for (query, found) in self.captures(&(0..self.text.len())) {
            let names = query.capture_names();
            let mut cache: Vec<Option<SharedString>> = vec![None; names.len()];
            for (span, capture) in found {
                let name = cache[capture as usize]
                    .get_or_insert_with(|| SharedString::from(names[capture as usize].to_string()));
                out.push((span, name.clone()));
            }
        }
        out
    }

    /// `range` 里命中的高亮，每棵树一组：（查询，[(范围，高亮名下标)]），宿主语言先、嵌着的层后。
    fn captures(&self, range: &Range<usize>) -> Vec<(&Query, Vec<(Range<usize>, u32)>)> {
        let mut out = Vec::new();
        if let (Some(grammar), Some(tree)) = (&self.grammar, &self.tree) {
            let found = tree_captures(&grammar.highlights, tree.root_node(), range, &self.text);
            out.push((&grammar.highlights, found));
        }
        for layer in &self.layers {
            if layer.bytes.end <= range.start || layer.bytes.start >= range.end {
                continue;
            }
            let found = tree_captures(
                &layer.grammar.highlights,
                layer.tree.root_node(),
                range,
                &self.text,
            );
            out.push((&layer.grammar.highlights, found));
        }
        out
    }

    /// 能折叠的行段：跨三行以上（首尾行号差至少 2）的具名节点，碰到 `bytes` 的才算。同一行
    /// 开头的只留最外层那个。
    pub fn fold_ranges(&self, bytes: Range<usize>) -> Vec<FoldRange> {
        fn collect(node: Node, bytes: &Range<usize>, ranges: &mut Vec<FoldRange>) {
            if node.end_byte() <= bytes.start || node.start_byte() >= bytes.end {
                return;
            }
            let start = node.start_position().row;
            let end = node.end_position().row;
            if end.saturating_sub(start) < 2 {
                return;
            }
            ranges.push(FoldRange::new(start, end));
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect(child, bytes, ranges);
            }
        }

        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let root = tree.root_node();
        let mut ranges = Vec::new();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            collect(child, &bytes, &mut ranges);
        }
        ranges.sort_by_key(|range| range.start_line);
        ranges.dedup_by_key(|range| range.start_line);
        ranges
    }
}

/// 一棵树在 `range` 里的高亮（范围截到 `range` 里，高亮名下标），按画的先后排：外层先、
/// 里层后；同一节点只留查询里最靠后的模式。
fn tree_captures(
    query: &Query,
    root: Node,
    range: &Range<usize>,
    text: &Rope,
) -> Vec<(Range<usize>, u32)> {
    let mut found: Vec<(usize, usize, usize, u32)> = Vec::new();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range.clone());
    let mut matches = cursor.matches(query, root, RopeText(text));
    while let Some(m) = matches.next() {
        for capture in m.captures {
            let node = capture.node.byte_range();
            if node.end <= range.start || node.start >= range.end || node.is_empty() {
                continue;
            }
            found.push((node.start, node.end, m.pattern_index, capture.index));
        }
    }
    found.sort_unstable_by(|a, b| {
        (a.0, std::cmp::Reverse(a.1), a.2).cmp(&(b.0, std::cmp::Reverse(b.1), b.2))
    });
    // 同一节点只留最后一条（排在最后的模式下标最大）。
    found.dedup_by(|later, kept| {
        let same = later.0 == kept.0 && later.1 == kept.1;
        if same {
            *kept = *later;
        }
        same
    });
    found
        .into_iter()
        .map(|(start, end, _, capture)| (start.max(range.start)..end.min(range.end), capture))
        .collect()
}

/// 解析一遍。`included` 空着是整份文本。语法版本不合或范围不合法时返回 `Ok(None)`：这份文本
/// 不上色，不算叫停。
fn parse(
    language: &tree_sitter::Language,
    included: &[tree_sitter::Range],
    old: Option<&Tree>,
    text: &Rope,
    stop: &mut dyn FnMut() -> bool,
) -> Result<Option<Tree>, Stopped> {
    let mut parser = Parser::new();
    if parser.set_language(language).is_err()
        || (!included.is_empty() && parser.set_included_ranges(included).is_err())
    {
        return Ok(None);
    }
    let mut stopped = false;
    let mut progress = |_: &ParseState| {
        if stop() {
            stopped = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let tree = parser.parse_with_options(
        &mut |offset, _| read_bytes(text, offset),
        old,
        Some(options),
    );
    if stopped { Err(Stopped) } else { Ok(tree) }
}

/// 解析一层。解析不了返回 `Ok(None)`，这一处不上色；被叫停返回 `Err`。
fn parse_layer(
    grammar: Arc<Grammar>,
    ranges: Vec<tree_sitter::Range>,
    old: Option<&Tree>,
    combined: bool,
    text: &Rope,
    stop: &mut dyn FnMut() -> bool,
) -> Result<Option<Layer>, Stopped> {
    let Some(bytes) = bounding_bytes(&ranges) else {
        return Ok(None);
    };
    let Some(tree) = parse(&grammar.language, &ranges, old, text, stop)? else {
        return Ok(None);
    };
    Ok(Some(Layer {
        grammar,
        ranges,
        bytes,
        tree,
        combined,
    }))
}

/// 给 tree-sitter 读文本。要切字节不能切字符串：旧树会让它从字中间读起，切字符串会 panic，
/// 而这个回调在 C 那边调，panic 直接让进程退出。
pub(super) fn read_bytes(text: &Rope, offset: usize) -> &[u8] {
    if offset >= text.len() {
        return &[];
    }
    let (chunk, chunk_start) = text.chunk(offset);
    &chunk.as_bytes()[offset - chunk_start..]
}

/// 查询里 `#eq?`、`#match?` 这类条件要读节点文本，按 Rope 的块给。
struct RopeText<'a>(&'a Rope);

impl<'a> tree_sitter::TextProvider<&'a [u8]> for RopeText<'a> {
    type I = NodeChunks<'a>;

    fn text(&mut self, node: Node) -> Self::I {
        let range = node.byte_range();
        NodeChunks {
            text: self.0,
            at: range.start,
            end: range.end.min(self.0.len()),
        }
    }
}

struct NodeChunks<'a> {
    text: &'a Rope,
    at: usize,
    end: usize,
}

impl<'a> Iterator for NodeChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.end {
            return None;
        }
        let (chunk, chunk_start) = self.text.chunk(self.at);
        let chunk = chunk.as_bytes();
        let start = self.at - chunk_start;
        let end = (self.end - chunk_start).min(chunk.len());
        self.at = chunk_start + end;
        (start < end).then(|| &chunk[start..end])
    }
}

/// 注入内容节点要解析的几段。不写 `include-children` 时挖掉子节点（比如引用块里每行开头的
/// `> `），只留子节点之间的空隙。
fn content_ranges(node: Node, include_children: bool, out: &mut Vec<tree_sitter::Range>) {
    let whole = node.range();
    if include_children {
        out.push(whole);
        return;
    }
    let mut start_byte = whole.start_byte;
    let mut start_point = whole.start_point;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let child = child.range();
        if child.start_byte > start_byte {
            out.push(tree_sitter::Range {
                start_byte,
                end_byte: child.start_byte,
                start_point,
                end_point: child.start_point,
            });
        }
        if child.end_byte > start_byte {
            start_byte = child.end_byte;
            start_point = child.end_point;
        }
    }
    if whole.end_byte > start_byte {
        out.push(tree_sitter::Range {
            start_byte,
            end_byte: whole.end_byte,
            start_point,
            end_point: whole.end_point,
        });
    }
}

/// `@injection.language` 捕获到的文本当语言名。树可能是旧的，范围落在字中间就不算。
fn language_text(text: &Rope, range: Range<usize>) -> Option<String> {
    if range.is_empty()
        || range.end > text.len()
        || range.len() > MAX_LANGUAGE_NAME_BYTES
        || !text.is_char_boundary(range.start)
        || !text.is_char_boundary(range.end)
    {
        return None;
    }
    let name = text.slice(range).to_string();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// 这段行内文本里有没有可能是 Markdown 行内标记的字符；纯文字的段不值得解析。
///
/// 字符集要包住 `markdown_inline/highlights.scm` 里每种节点的起始字符（强调、行内代码、链接、
/// 图片、自动链接）。树可能是旧的，先对齐到字边界再切。
fn has_inline_markup(text: &Rope, range: Range<usize>) -> bool {
    let end = text.floor_char_boundary(range.end.min(text.len()));
    let start = text.floor_char_boundary(range.start.min(end));
    text.slice(start..end).bytes().any(|byte| {
        matches!(
            byte,
            b'*' | b'_' | b'`' | b'[' | b']' | b'(' | b')' | b'<' | b'>' | b'!' | b'~' | b'$'
        )
    })
}

/// 合并解析的行内内容：只留有标记的段时，相邻两段在解析器眼里是连着的，上一项末尾的反引号
/// 会和下一项开头的配成一对。把段之间的原文也放回去，保住原来的分界。
fn keep_inline_separators(ranges: Vec<tree_sitter::Range>) -> Vec<tree_sitter::Range> {
    let mut budget = Budget::default();
    let mut previous: Option<tree_sitter::Range> = None;
    for range in ranges {
        let mut pending = Vec::with_capacity(2);
        if let Some(previous) = previous
            && previous.end_byte < range.start_byte
        {
            pending.push(tree_sitter::Range {
                start_byte: previous.end_byte,
                end_byte: range.start_byte,
                start_point: previous.end_point,
                end_point: range.start_point,
            });
        }
        pending.push(range);
        if !budget.extend(pending) {
            break;
        }
        previous = Some(range);
    }
    budget.ranges
}

/// 一层要解析的几段，数量和字节数都有上限。
#[derive(Default)]
struct Budget {
    ranges: Vec<tree_sitter::Range>,
    bytes: usize,
}

impl Budget {
    /// 整批放得下才放，放不下一段也不放，返回 false。
    fn extend(&mut self, ranges: Vec<tree_sitter::Range>) -> bool {
        let bytes: usize = ranges
            .iter()
            .map(|r| r.end_byte.saturating_sub(r.start_byte))
            .sum();
        if self.ranges.len() + ranges.len() > MAX_INJECTION_RANGES
            || self.bytes + bytes > MAX_INJECTION_BYTES
        {
            return false;
        }
        self.bytes += bytes;
        self.ranges.extend(ranges);
        true
    }
}

fn sort_ranges(ranges: &mut [tree_sitter::Range]) {
    ranges.sort_unstable_by_key(|range| (range.start_byte, range.end_byte));
}

fn range_key(ranges: &[tree_sitter::Range]) -> Vec<(usize, usize)> {
    ranges.iter().map(|r| (r.start_byte, r.end_byte)).collect()
}

fn bounding_bytes(ranges: &[tree_sitter::Range]) -> Option<Range<usize>> {
    let start = ranges.iter().map(|r| r.start_byte).min()?;
    let end = ranges.iter().map(|r| r.end_byte).max()?;
    Some(start..end)
}

/// 按画的先后把可能重叠的样式段压成不重叠、铺满 `range` 的段：每一段的样式是盖在它上面的所有
/// 段按先后叠起来的，后画的字段盖前画的。
fn paint(
    range: &Range<usize>,
    spans: &[(Range<usize>, HighlightStyle)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    // (位置, 是不是起点, 第几段)。同一位置先收尾再开头。
    let mut events: Vec<(usize, bool, usize)> = Vec::with_capacity(spans.len() * 2);
    for (index, (span, _)) in spans.iter().enumerate() {
        if !span.is_empty() {
            events.push((span.start, true, index));
            events.push((span.end, false, index));
        }
    }
    events.sort_unstable_by_key(|&(at, start, index)| (at, start, index));

    let mut result: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut active: BTreeSet<usize> = BTreeSet::new();
    let mut at = range.start;
    let mut push = |segment: Range<usize>, active: &BTreeSet<usize>| {
        if segment.is_empty() {
            return;
        }
        let style = active
            .iter()
            .fold(HighlightStyle::default(), |acc, &index| {
                overlay(acc, &spans[index].1)
            });
        match result.last_mut() {
            Some((last, last_style)) if last.end == segment.start && *last_style == style => {
                last.end = segment.end;
            }
            _ => result.push((segment, style)),
        }
    };
    for (position, start, index) in events {
        let position = position.clamp(range.start, range.end);
        push(at..position, &active);
        at = position;
        if start {
            active.insert(index);
        } else {
            active.remove(&index);
        }
    }
    push(at..range.end, &active);
    result
}

/// `top` 盖在 `base` 上：`top` 写了的字段用它的，没写的留 `base` 的。
fn overlay(mut base: HighlightStyle, top: &HighlightStyle) -> HighlightStyle {
    if top.color.is_some() {
        base.color = top.color;
    }
    if top.font_weight.is_some() {
        base.font_weight = top.font_weight;
    }
    if top.font_style.is_some() {
        base.font_style = top.font_style;
    }
    if top.background_color.is_some() {
        base.background_color = top.background_color;
    }
    if top.underline.is_some() {
        base.underline = top.underline;
    }
    if top.strikethrough.is_some() {
        base.strikethrough = top.strikethrough;
    }
    if top.fade_out.is_some() {
        base.fade_out = top.fade_out;
    }
    base
}

#[cfg(test)]
mod tests {
    use gpui::{Hsla, hsla};

    use super::*;

    /// 每个高亮名一个颜色，测试按颜色认出是哪个名字。
    struct Names(Vec<&'static str>);

    impl HighlightStyleResolver for Names {
        fn style(&self, name: &str) -> Option<HighlightStyle> {
            let index = self.0.iter().position(|n| *n == name)?;
            Some(HighlightStyle {
                color: Some(color(index)),
                ..HighlightStyle::default()
            })
        }
    }

    fn color(index: usize) -> Hsla {
        hsla(index as f32 / 100., 0.5, 0.5, 1.)
    }

    fn parsed(language: &str, source: &str) -> SyntaxTree {
        let mut tree = SyntaxTree::new(language);
        assert!(tree.parse(&Rope::from(source), &mut || false));
        tree
    }

    /// `source` 里 `needle` 那一段的颜色是哪个名字。
    fn name_at<'a>(
        tree: &SyntaxTree,
        names: &'a Names,
        source: &str,
        needle: &str,
    ) -> Option<&'a str> {
        let start = source.find(needle).expect("needle 在源码里");
        let styles = tree.styles(0..source.len(), names);
        let (_, style) = styles.iter().find(|(range, _)| range.contains(&start))?;
        let color = style.color?;
        names
            .0
            .iter()
            .enumerate()
            .find(|(i, _)| self::color(*i) == color)
            .map(|(_, n)| *n)
    }

    #[test]
    fn styles_cover_the_whole_range_in_order() {
        let source = "fn main() {\n    let s = \"你好\";\n}\n";
        let tree = parsed("rust", source);
        let names = Names(vec!["keyword", "string", "function"]);
        for range in [0..source.len(), 3..20, 0..0] {
            let styles = tree.styles(range.clone(), &names);
            let mut at = range.start;
            for (span, _) in &styles {
                assert_eq!(span.start, at, "{styles:?}");
                assert!(span.end > span.start);
                at = span.end;
            }
            assert_eq!(at, range.end);
        }
        assert_eq!(name_at(&tree, &names, source, "fn"), Some("keyword"));
        assert_eq!(name_at(&tree, &names, source, "\"你好\""), Some("string"));
    }

    #[test]
    fn unknown_languages_have_text_but_no_styles() {
        let tree = parsed("no-such-language", "hello");
        assert_eq!(tree.language(), None);
        assert_eq!(tree.text().to_string(), "hello");
        let styles = tree.styles(0..5, &Names(vec!["keyword"]));
        assert_eq!(styles, vec![(0..5, HighlightStyle::default())]);
        assert!(tree.fold_ranges(0..5).is_empty());
    }

    /// Markdown 围栏里的代码按围栏的语言上色，盖过 Markdown 自己的颜色。
    #[test]
    fn fenced_code_is_highlighted_in_its_own_language() {
        let source = "# Title\n\n```rust\nfn main() {}\n```\n";
        let tree = parsed("markdown", source);
        let names = Names(vec!["text.literal", "keyword", "function", "title"]);
        assert_eq!(name_at(&tree, &names, source, "fn main"), Some("keyword"));
    }

    /// 高亮名按画的先后排，嵌着的语言排在宿主后面；按名字查样式再叠，和直接出样式一样。
    #[test]
    fn capture_names_paint_like_styles() {
        let source = "# Title\n\n```rust\nfn main() {}\n```\n";
        let tree = parsed("markdown", source);
        let captures = tree.capture_names();
        let keyword = source.find("fn").unwrap();
        let last_at_keyword = captures
            .iter()
            .rev()
            .find(|(range, _)| range.contains(&keyword))
            .map(|(_, name)| name.as_ref());
        assert_eq!(last_at_keyword, Some("keyword"));
        assert!(captures.iter().all(|(range, _)| !range.is_empty()));

        let names = Names(vec!["text.literal", "keyword", "function", "title"]);
        let spans: Vec<_> = captures
            .iter()
            .filter_map(|(range, name)| Some((range.clone(), names.style(name)?)))
            .collect();
        let range = 0..source.len();
        assert_eq!(paint(&range, &spans), tree.styles(range.clone(), &names));
    }

    /// 改一处只重解析改到的地方，结果和从头解析一样。
    #[test]
    fn incremental_edit_matches_a_fresh_parse() {
        let before = "fn a() {}\n";
        let after = "fn ab() {}\n";
        let mut tree = parsed("rust", before);
        tree.edit(&InputEdit {
            start_byte: 4,
            old_end_byte: 4,
            new_end_byte: 5,
            start_position: tree_sitter::Point::new(0, 4),
            old_end_position: tree_sitter::Point::new(0, 4),
            new_end_position: tree_sitter::Point::new(0, 5),
        });
        assert!(tree.parse(&Rope::from(after), &mut || false));
        let fresh = parsed("rust", after);
        let names = Names(vec!["keyword", "function"]);
        assert_eq!(
            tree.styles(0..after.len(), &names),
            fresh.styles(0..after.len(), &names)
        );
    }

    /// 叫停时旧树留着、文本换新，样式照样铺满新文本。
    #[test]
    fn stopped_parse_keeps_the_old_tree() {
        let mut tree = parsed("rust", "fn a() {}\n");
        let text = Rope::from("fn a() {}\nfn b() {}\n");
        tree.edit(&InputEdit {
            start_byte: 10,
            old_end_byte: 10,
            new_end_byte: 20,
            start_position: tree_sitter::Point::new(1, 0),
            old_end_position: tree_sitter::Point::new(1, 0),
            new_end_position: tree_sitter::Point::new(2, 0),
        });
        assert!(!tree.parse(&text, &mut || true));
        assert_eq!(tree.text().len(), 20);
        let names = Names(vec!["keyword"]);
        let styles = tree.styles(0..20, &names);
        assert_eq!(styles.last().map(|(r, _)| r.end), Some(20));
        assert_eq!(styles[0].1.color, Some(color(0)), "旧树里的 fn 还有颜色");
    }

    #[test]
    fn folds_multi_line_blocks_once_per_start_line() {
        let source = "fn a() {\n    if x {\n        y();\n    }\n}\n\nfn b() {}\n";
        let tree = parsed("rust", source);
        let folds = tree.fold_ranges(0..usize::MAX);
        let lines: Vec<_> = folds.iter().map(|f| (f.start_line, f.end_line)).collect();
        // fn a 跨 0..4 行；if 块只跨 1..3，也够三行；单行的 fn b 不能折。
        assert_eq!(lines, vec![(0, 4), (1, 3)]);
    }

    #[test]
    fn read_bytes_from_inside_a_character() {
        let rope = Rope::from("let s = \"你好\";");
        let start = "let s = \"".len();
        assert_eq!(read_bytes(&rope, start + 1), &"你好\";".as_bytes()[1..]);
        assert_eq!(read_bytes(&rope, rope.len()), b"");
        assert_eq!(read_bytes(&rope, rope.len() + 4), b"");
    }

    #[test]
    fn later_spans_override_only_the_fields_they_set() {
        let bold = HighlightStyle {
            font_weight: Some(gpui::FontWeight::BOLD),
            ..HighlightStyle::default()
        };
        let red = HighlightStyle {
            color: Some(color(1)),
            ..HighlightStyle::default()
        };
        let painted = paint(&(0..10), &[(2..8, bold), (4..6, red)]);
        let both = HighlightStyle {
            font_weight: Some(gpui::FontWeight::BOLD),
            color: Some(color(1)),
            ..HighlightStyle::default()
        };
        assert_eq!(
            painted,
            vec![
                (0..2, HighlightStyle::default()),
                (2..4, bold),
                (4..6, both),
                (6..8, bold),
                (8..10, HighlightStyle::default()),
            ]
        );
    }
}
