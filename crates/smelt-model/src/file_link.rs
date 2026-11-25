//! 对话消息里的文件链接：Markdown 链接的目标是不是本地文件、指向哪个绝对路径和哪一行。
//!
//! 桌面和手机共用这一份规则，点同一个链接打开的是同一个文件。路径属于对话所在的那台
//! 机器，按它的 [`PathStyle`] 补全，不经过 `std::path`。

use crate::runtime_path::PathStyle;

/// 解析出来的文件链接。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLink {
    /// 那台机器上的绝对路径。
    pub path: String,
    /// `#` 后面的片段，不含 `#`。`path:行号` 写法会换成 `L行号`。没有就是空串。
    pub fragment: String,
}

impl FileLink {
    /// 片段里的行号（1 基）：`L42`、`L42-L50` 取起始行。
    pub fn line(&self) -> Option<u32> {
        let rest = self.fragment.strip_prefix('L')?;
        let digits: &str = rest
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or_default();
        digits.parse().ok().filter(|line| *line > 0)
    }
}

/// `target` 是 Markdown 链接的目标。`file://` URI、绝对路径、相对路径（按 `cwd` 补全）
/// 都认作文件链接；网址、锚点、`~` 开头的路径和无法补全的相对路径返回 `None`。
pub fn parse_file_link(target: &str, cwd: Option<&str>, style: PathStyle) -> Option<FileLink> {
    let target = target.trim();
    if target.is_empty()
        || target.starts_with('#')
        || target.starts_with('~')
        || target.starts_with("mailto:")
        || target.starts_with("data:")
    {
        return None;
    }
    // 先按原始字符串拆 `#fragment`，再解码路径里的 `%23`，免得文件名里的 `#` 被误拆。
    let (raw_path, fragment) = target.split_once('#').unwrap_or((target, ""));
    let path_str = if raw_path
        .as_bytes()
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"file://"))
    {
        parse_file_uri(&raw_path[7..], style)?
    } else if raw_path.contains("://") {
        return None;
    } else if raw_path.is_empty() {
        return None;
    } else {
        raw_path.to_string()
    };

    // Agent 引用文件常写成 grep / 编译器诊断那种 `path:行号` 或 `path:行号:列号`
    // （不是 `#L行号` 片段）。没有 `#` 片段时才从冒号后缀里抠行号，不然 `:2765`
    // 会被当成文件名的一部分拼进路径。
    let (base_path, fragment) = if fragment.is_empty() {
        match extract_trailing_line_number(&path_str) {
            Some((base, line)) => (base, format!("L{line}")),
            None => (path_str.as_str(), String::new()),
        }
    } else {
        (path_str.as_str(), fragment.to_string())
    };
    if base_path.is_empty() {
        return None;
    }
    let absolute = if style.is_absolute(base_path) {
        base_path.to_string()
    } else {
        style.join(cwd?, base_path)
    };
    Some(FileLink {
        path: style.clean(&absolute),
        fragment,
    })
}

/// 解析 `file://` 之后的部分，按目标机器的 [`PathStyle`] 还原成本地路径或 UNC 路径。
fn parse_file_uri(rest: &str, style: PathStyle) -> Option<String> {
    if rest.is_empty() {
        return None;
    }
    if rest.starts_with('/') {
        let decoded = percent_decode(rest)?;
        return normalize_uri_path(&decoded, style);
    }
    // 兼容部分模型少打一条斜杠的 Windows 写法：`file://E:/AI/a.md` 或 `file://E%3A/AI/a.md`。
    if style == PathStyle::Windows {
        let decoded = percent_decode(rest)?;
        if is_windows_drive_with_slash(&decoded) {
            return Some(decoded);
        }
    }
    let sep = match style {
        PathStyle::Posix => rest.find('/'),
        PathStyle::Windows => rest.find(['/', '\\']),
    }?;
    let (authority, tail) = rest.split_at(sep);
    if authority.is_empty() || tail.is_empty() {
        return None;
    }
    if authority.eq_ignore_ascii_case("localhost") {
        let decoded = percent_decode(tail)?;
        return normalize_uri_path(&decoded, style);
    }
    match style {
        PathStyle::Windows => {
            let decoded_tail = percent_decode(tail)?;
            let has_share = decoded_tail
                .trim_start_matches(['/', '\\'])
                .split(['/', '\\'])
                .next()
                .is_some_and(|seg| !seg.is_empty());
            if !has_share {
                return None;
            }
            let decoded_auth = percent_decode(authority)?;
            Some(format!("//{decoded_auth}{decoded_tail}"))
        }
        // POSIX 上非 localhost 的 authority 指向另一台主机，不误当本机路径。
        PathStyle::Posix => None,
    }
}

fn normalize_uri_path(decoded: &str, style: PathStyle) -> Option<String> {
    match style {
        PathStyle::Posix => (!decoded.is_empty()).then(|| decoded.to_string()),
        PathStyle::Windows => {
            // 四斜杠 UNC：`file:////server/share/a.md` 解码后是 `//server/share/a.md`。
            if decoded.starts_with("//") || decoded.starts_with("\\\\") {
                let mut parts = decoded
                    .trim_start_matches(['/', '\\'])
                    .split(['/', '\\'])
                    .filter(|seg| !seg.is_empty());
                let _server = parts.next()?;
                let _share = parts.next()?;
                return Some(decoded.to_string());
            }
            let without_leading = decoded.strip_prefix(['/', '\\']).unwrap_or(decoded);
            if is_windows_drive_path(without_leading) {
                return Some(without_leading.to_string());
            }
            if without_leading.is_empty() {
                return None;
            }
            Some(decoded.to_string())
        }
    }
}

fn is_windows_drive_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'/' || b[2] == b'\\')
}

fn is_windows_drive_with_slash(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 从 `path:行号` / `path:行号:列号` 里拆出末尾的行号：从右往左，只要是纯数字的一段就
/// 往前吞（列号可选，最多两段），吞到的最后一段数字是行号。一段都没吞到就不是这种格式，
/// 比如 Windows 盘符 `C:\...`。
fn extract_trailing_line_number(path: &str) -> Option<(&str, u32)> {
    let mut rest = path;
    let mut line = None;
    for _ in 0..2 {
        let Some((head, tail)) = rest.rsplit_once(':') else {
            break;
        };
        if head.is_empty() {
            break;
        }
        let Ok(n) = tail.parse::<u32>() else {
            break;
        };
        line = Some(n);
        rest = head;
    }
    line.map(|n| (rest, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(path: &str, fragment: &str) -> Option<FileLink> {
        Some(FileLink {
            path: path.into(),
            fragment: fragment.into(),
        })
    }

    #[test]
    fn relative_paths_resolve_against_the_cwd() {
        let cwd = Some("/tmp/project");
        assert_eq!(
            parse_file_link("docs/workspace.md", cwd, PathStyle::Posix),
            link("/tmp/project/docs/workspace.md", "")
        );
        assert_eq!(
            parse_file_link("./src/../README.md", cwd, PathStyle::Posix),
            link("/tmp/project/README.md", "")
        );
        assert_eq!(
            parse_file_link("/etc/hosts#L3", cwd, PathStyle::Posix),
            link("/etc/hosts", "L3")
        );
    }

    #[test]
    fn urls_anchors_and_home_paths_are_not_file_links() {
        for target in [
            "https://example.com",
            "#section",
            "~/notes.md",
            "mailto:a@b.c",
            "data:text/plain,hi",
            "  ",
        ] {
            assert_eq!(
                parse_file_link(target, Some("/p"), PathStyle::Posix),
                None,
                "{target}"
            );
        }
        // 没有 cwd 补不全的相对路径不猜。
        assert_eq!(parse_file_link("docs/a.md", None, PathStyle::Posix), None);
    }

    #[test]
    fn colon_line_refs_become_a_line_fragment() {
        let parsed = parse_file_link("src/main.rs:10:5", Some("/p"), PathStyle::Posix).unwrap();
        assert_eq!(
            parsed,
            FileLink {
                path: "/p/src/main.rs".into(),
                fragment: "L10".into()
            }
        );
        assert_eq!(parsed.line(), Some(10));
        assert_eq!(
            parse_file_link("a.rs#L7-L9", Some("/p"), PathStyle::Posix)
                .and_then(|link| link.line()),
            Some(7)
        );
        assert_eq!(
            parse_file_link("a.md#usage", Some("/p"), PathStyle::Posix)
                .and_then(|link| link.line()),
            None
        );
    }

    #[test]
    fn windows_machines_keep_their_drive_paths() {
        assert_eq!(
            parse_file_link(r"src\lib.rs:3", Some(r"C:\code\app"), PathStyle::Windows),
            link(r"C:\code\app\src\lib.rs", "L3")
        );
        assert_eq!(
            parse_file_link(r"D:\notes.md", Some(r"C:\code"), PathStyle::Windows),
            link(r"D:\notes.md", "")
        );
    }

    #[test]
    fn file_uris_resolve_across_windows_and_posix() {
        assert_eq!(
            parse_file_link(
                "file:///E:/AI/minigame-studio/AGENTS.md",
                None,
                PathStyle::Windows
            ),
            link(r"E:\AI\minigame-studio\AGENTS.md", "")
        );
        assert_eq!(
            parse_file_link(
                "file:///E:/AI/minigame-studio/AGENTS.md#L10-L20",
                None,
                PathStyle::Windows
            ),
            link(r"E:\AI\minigame-studio\AGENTS.md", "L10-L20")
        );
        assert_eq!(
            parse_file_link(
                "file:///C:/Users/Administrator/.gemini/config/skills/a/b.md",
                None,
                PathStyle::Windows
            ),
            link(r"C:\Users\Administrator\.gemini\config\skills\a\b.md", "")
        );
        assert_eq!(
            parse_file_link(
                "file:///E%3A/AI/my%20project/%E4%B8%AD%E6%96%87.md",
                None,
                PathStyle::Windows
            ),
            link(r"E:\AI\my project\中文.md", "")
        );
        assert_eq!(
            parse_file_link("file://localhost/E:/AI/a.md", None, PathStyle::Windows),
            link(r"E:\AI\a.md", "")
        );
        assert_eq!(
            parse_file_link("file://E:/AI/a.md", None, PathStyle::Windows),
            link(r"E:\AI\a.md", "")
        );
        assert_eq!(
            parse_file_link("file://server/share/a.md", None, PathStyle::Windows),
            link(r"\\server\share\a.md", "")
        );
        assert_eq!(
            parse_file_link("file:////server/share/a.md", None, PathStyle::Windows),
            link(r"\\server\share\a.md", "")
        );
        assert_eq!(
            parse_file_link("FILE:///Users/a/b.md", None, PathStyle::Posix),
            link("/Users/a/b.md", "")
        );
        assert_eq!(
            parse_file_link("file:///Users/a/a%23b.rs#L5", None, PathStyle::Posix),
            link("/Users/a/a#b.rs", "L5")
        );
        assert_eq!(
            parse_file_link("file://localhost/etc/hosts:12", None, PathStyle::Posix),
            link("/etc/hosts", "L12")
        );
        assert_eq!(
            parse_file_link("file://other-host/etc/hosts", None, PathStyle::Posix),
            None
        );
        assert_eq!(parse_file_link("file://", None, PathStyle::Windows), None);
        assert_eq!(parse_file_link("file:///", None, PathStyle::Windows), None);
        assert_eq!(
            parse_file_link("file:///E:/100%_ok.md", None, PathStyle::Windows),
            link(r"E:\100%_ok.md", "")
        );
        assert_eq!(
            parse_file_link("https://example.com#L10", None, PathStyle::Windows),
            None
        );
    }

    #[test]
    fn multibyte_utf8_paths_do_not_panic() {
        assert_eq!(
            parse_file_link("中文文档.md", Some("/tmp/project"), PathStyle::Posix),
            link("/tmp/project/中文文档.md", "")
        );
        assert_eq!(
            parse_file_link(r"中文\笔记.md", Some(r"E:\AI"), PathStyle::Windows),
            link(r"E:\AI\中文\笔记.md", "")
        );
        assert_eq!(
            parse_file_link(
                "notes/中文文档.md#L3",
                Some("/tmp/project"),
                PathStyle::Posix
            ),
            link("/tmp/project/notes/中文文档.md", "L3")
        );
    }
}
