//! 语法高亮：认语言、类别映射、按窗口解析和检查点、退回纯文本的几种情况。

use super::*;

fn kinds(line: &HighlightedLine) -> Vec<(&str, TokenKind)> {
    line.spans
        .iter()
        .map(|span| (span.text.as_str(), span.kind))
        .collect()
}

fn text_of(line: &HighlightedLine) -> String {
    line.spans.iter().map(|span| span.text.as_str()).collect()
}

#[test]
fn languages_are_detected_by_name_extension_and_shebang() {
    let language = |path: &str, text: &str| {
        Document::new(path, text.to_string())
            .language()
            .map(str::to_string)
    };
    assert_eq!(language("/p/src/main.rs", "fn main() {}").as_deref(), Some("Rust"));
    assert_eq!(language("/p/web/app.ts", "let a = 1").as_deref(), Some("TypeScript"));
    assert_eq!(language("/p/Cargo.toml", "[package]").as_deref(), Some("TOML"));
    assert!(language("/p/Dockerfile", "FROM alpine").is_some());
    assert_eq!(
        language("/p/bin/run", "#!/bin/bash\necho hi\n").as_deref(),
        Some("Bourne Again Shell (bash)")
    );
    // 认不出（含纯文本）不上色。
    let notes = Document::new("/p/notes.unknownext", "hello".to_string());
    assert_eq!(notes.language(), None);
    assert!(!notes.highlighted());
}

#[test]
fn rust_lines_get_semantic_kinds_and_keep_their_text() {
    let source = "// hi\nfn main() {\n    let s = \"x\"; let n = 42;\n}\n";
    let document = Document::new("/p/main.rs", source.to_string());
    assert!(document.highlighted());
    let lines = document.lines(0, 10);
    assert_eq!(lines.len(), 4, "末尾换行后没有多出一行");
    assert_eq!(lines[0].number, 1);
    assert_eq!(kinds(&lines[0]), vec![("// hi", TokenKind::Comment)]);
    let second = kinds(&lines[1]);
    assert!(second.contains(&("fn", TokenKind::Keyword)), "{second:?}");
    assert!(second.contains(&("main", TokenKind::Function)), "{second:?}");
    let third = kinds(&lines[2]);
    // 引号跟着字符串走，不单独算标点。
    assert!(third.contains(&("\"x\"", TokenKind::String)), "{third:?}");
    assert!(third.contains(&("42", TokenKind::Number)), "{third:?}");
    // 片段拼起来就是原文，换行符不显示。
    for (line, expected) in lines.iter().zip(source.lines()) {
        assert_eq!(text_of(line), expected);
    }
}

#[test]
fn a_late_window_continues_from_a_checkpoint_and_matches_a_full_parse() {
    // 第 1500 行前有一个跨行的块注释没关：从检查点续解析时，状态要跟整篇从头解析一样。
    let mut source = String::new();
    for ix in 0..1490 {
        source.push_str(&format!("let v{ix} = {ix};\n"));
    }
    source.push_str("/* open\n");
    for _ in 0..20 {
        source.push_str("still comment\n");
    }
    source.push_str("*/ let done = 1;\n");
    let windowed = Document::new("/p/a.js", source.clone());
    let late = windowed.lines(1495, 1505);
    let full = Document::new("/p/a.js", source).lines(0, 1505);
    assert_eq!(late, full[1495..1505].to_vec());
    assert!(late.iter().all(|line| kinds(line) == vec![("still comment", TokenKind::Comment)]));
    // 再取前面的窗口也没问题（从第 0 份检查点重新来）。
    assert_eq!(windowed.lines(0, 3), full[0..3].to_vec());
}

#[test]
fn big_files_and_minified_lines_fall_back_to_plain_text() {
    let long_line = format!("var a = \"{}\";\n", "x".repeat(HIGHLIGHT_MAX_LINE + 1));
    let minified = Document::new("/p/app.min.js", long_line);
    assert!(minified.language().is_some_and(|name| name.starts_with("JavaScript")));
    assert!(!minified.highlighted());
    assert_eq!(minified.lines(0, 1)[0].spans[0].kind, TokenKind::Plain);

    let big = "let a = 1;\n".repeat(HIGHLIGHT_MAX_BYTES / 10);
    assert!(!Document::new("/p/big.js", big).highlighted());
}

#[test]
fn windows_line_endings_and_empty_files() {
    let crlf = Document::new("/p/a.py", "x = 1\r\ny = 2\r\n".to_string());
    let lines = crlf.lines(0, 5);
    assert_eq!(lines.len(), 2);
    assert_eq!(text_of(&lines[0]), "x = 1");
    assert_eq!(text_of(&lines[1]), "y = 2");

    let empty = Document::new("/p/empty.rs", String::new());
    assert_eq!(empty.line_count(), 1);
    assert_eq!(empty.lines(0, 1)[0].spans, Vec::new());
    assert!(empty.lines(3, 9).is_empty());
}

#[test]
fn scope_rules_prefer_the_specific_category() {
    assert_eq!(kind_of_scope("string.other.link.title.markdown"), Some(TokenKind::Link));
    assert_eq!(kind_of_scope("string.quoted.double.rust"), Some(TokenKind::String));
    assert_eq!(kind_of_scope("constant.numeric.integer"), Some(TokenKind::Number));
    assert_eq!(kind_of_scope("constant.language.boolean"), Some(TokenKind::Constant));
    assert_eq!(kind_of_scope("keyword.operator.arithmetic"), Some(TokenKind::Operator));
    assert_eq!(kind_of_scope("keyword.control"), Some(TokenKind::Keyword));
    assert_eq!(kind_of_scope("punctuation.definition.string.begin"), None);
    assert_eq!(kind_of_scope("punctuation.separator"), Some(TokenKind::Punctuation));
    // 前缀要按整段比：`stringy` 不是 `string`。
    assert_eq!(kind_of_scope("stringy.thing"), None);
    assert_eq!(kind_of_scope("source.rust"), None);
}

#[test]
fn markdown_source_marks_headings_and_links() {
    let document = Document::new(
        "/p/README.md",
        "# Title\n\nSee [docs](https://x.y).\n".to_string(),
    );
    let lines = document.lines(0, 3);
    assert!(kinds(&lines[0]).iter().all(|(_, kind)| *kind == TokenKind::Heading), "{:?}", kinds(&lines[0]));
    assert!(kinds(&lines[2]).iter().any(|(_, kind)| *kind == TokenKind::Link), "{:?}", kinds(&lines[2]));
}
