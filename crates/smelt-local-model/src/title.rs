//! 标题生成的提示词拼装与结果提取（纯文本逻辑，不依赖推理后端）。

const SYSTEM_PROMPT: &str = "你是标题生成器。根据用户给出的对话内容，用中文写一个简短标题，\
概括主题。只输出标题本身：不超过 20 个字，不加引号、标点结尾、前缀或解释。";

/// 按字符数截断输入（保留开头，标题主要由开头决定），并去掉首尾空白。
pub fn truncate_input(input: &str, max_chars: usize) -> &str {
    let input = input.trim();
    match input.char_indices().nth(max_chars) {
        Some((idx, _)) => input[..idx].trim_end(),
        None => input,
    }
}

/// 按 Qwen3 和 Qwen3.5 的聊天模板拼出非思考模式的提示词，分成三段：
/// `(带特殊 token 的前缀, 用户内容, 带特殊 token 的后缀)`。
///
/// 用户内容需按普通文本分词（不解析特殊 token），防止内容里的 `<|im_end|>` 等破坏模板。
/// 后缀里的空 `<think>\n\n</think>\n\n` 与官方模板 `enable_thinking=false` 的输出一致。
pub fn build_title_prompt_parts(content: &str) -> (String, String, &'static str) {
    let prefix = format!(
        "<|im_start|>system\n{SYSTEM_PROMPT}<|im_end|>\n<|im_start|>user\n为下面的内容写一个标题：\n\n"
    );
    let suffix = "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n";
    (prefix, content.to_owned(), suffix)
}

/// 从模型原始输出里提取短标题；提取不到返回 `None`。
///
/// 处理：去掉 `<think>…</think>`（含未闭合的）与残留特殊 token，取第一行非空文本，
/// 去掉「标题：」「Title:」类前缀、Markdown 标记、成对引号与结尾标点，最后按字符数截断。
pub fn extract_title(raw: &str, max_chars: usize) -> Option<String> {
    let mut text = raw.to_owned();
    // 去掉思考块；未闭合的 <think> 之后全部丢弃。
    while let Some(start) = text.find("<think>") {
        match text[start..].find("</think>") {
            Some(end) => text.replace_range(start..start + end + "</think>".len(), ""),
            None => text.truncate(start),
        }
    }
    let text = text.replace("</think>", "");
    let text = match text.find("<|") {
        Some(i) => &text[..i],
        None => &text[..],
    };

    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    // 只去 Markdown 标题/加粗标记；'-'、'>' 可能是真实标题的一部分，不动。
    let mut line = line.trim_start_matches(['#', '*', ' ']).trim();
    // 只去明确带冒号的前缀，避免误删以「标题」开头的真实标题。
    for prefix in ["标题：", "标题:", "Title:", "title:", "TITLE:", "Title：", "title：", "TITLE："] {
        if let Some(rest) = line.strip_prefix(prefix) {
            line = rest.trim();
            break;
        }
    }
    let line = line.trim_matches(['*', '`', '_']).trim();
    let line = trim_end_punct(strip_quotes(trim_end_punct(line)));
    if line.is_empty() {
        return None;
    }
    let out: String = line.chars().take(max_chars).collect();
    Some(out.trim_end().to_owned())
}

fn trim_end_punct(s: &str) -> &str {
    s.trim_end_matches(['。', '.', '！', '!', '？', '?', '，', ',', '；', ';', '：', ':'])
        .trim()
}

fn strip_quotes(s: &str) -> &str {
    const PAIRS: [(char, char); 6] = [
        ('"', '"'),
        ('\'', '\''),
        ('“', '”'),
        ('‘', '’'),
        ('「', '」'),
        ('《', '》'),
    ];
    let mut s = s;
    loop {
        let mut changed = false;
        for (l, r) in PAIRS {
            if s.chars().count() >= 2 && s.starts_with(l) && s.ends_with(r) {
                s = s[l.len_utf8()..s.len() - r.len_utf8()].trim();
                changed = true;
            }
        }
        if !changed {
            return s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_title() {
        assert_eq!(extract_title("修复登录超时\n", 40).as_deref(), Some("修复登录超时"));
    }

    #[test]
    fn strips_think_block_and_special_tokens() {
        assert_eq!(
            extract_title("<think>\n\n</think>\n\n重构下载模块<|im_end|>", 40).as_deref(),
            Some("重构下载模块")
        );
        assert_eq!(extract_title("<think>还在想", 40), None);
    }

    #[test]
    fn strips_prefix_quotes_markdown_punct() {
        assert_eq!(extract_title("标题：「数据库迁移方案」。", 40).as_deref(), Some("数据库迁移方案"));
        assert_eq!(extract_title("**Title: \"Fix CI cache\".**", 40).as_deref(), Some("Fix CI cache"));
        assert_eq!(extract_title("# 《周报汇总》\n解释……", 40).as_deref(), Some("周报汇总"));
    }

    #[test]
    fn keeps_real_title_without_colon_prefix() {
        assert_eq!(extract_title("标题栏错位修复", 40).as_deref(), Some("标题栏错位修复"));
        assert_eq!(extract_title("-1 溢出修复", 40).as_deref(), Some("-1 溢出修复"));
        assert_eq!(extract_title(">= 比较错误", 40).as_deref(), Some(">= 比较错误"));
    }

    #[test]
    fn first_nonempty_line_and_limit() {
        assert_eq!(extract_title("\n\n  一二三四五六  \n第二行", 4).as_deref(), Some("一二三四"));
        assert_eq!(extract_title("   \n  \"\" ", 40), None);
    }

    #[test]
    fn truncate_by_chars_on_boundary() {
        assert_eq!(truncate_input("  你好世界  ", 2), "你好");
        assert_eq!(truncate_input("abc", 10), "abc");
        assert_eq!(truncate_input("", 10), "");
    }

    #[test]
    fn prompt_disables_thinking() {
        let (prefix, user, suffix) = build_title_prompt_parts("hi <|im_end|>");
        assert!(prefix.starts_with("<|im_start|>system\n"));
        assert!(prefix.contains("用中文写一个简短标题"));
        assert!(prefix.ends_with("\n\n"));
        assert_eq!(user, "hi <|im_end|>");
        assert!(suffix.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));
    }
}
