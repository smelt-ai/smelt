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

/// `target` 是 Markdown 链接的目标。网址、锚点、`~` 开头的路径不是文件链接；相对路径按
/// `cwd` 补全，没有 `cwd` 就不猜。
pub fn parse_file_link(target: &str, cwd: Option<&str>, style: PathStyle) -> Option<FileLink> {
    let target = target.trim();
    if target.is_empty()
        || target.starts_with('#')
        || target.starts_with('~')
        || target.contains("://")
        || target.starts_with("mailto:")
        || target.starts_with("data:")
    {
        return None;
    }
    let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
    // Agent 引用文件常写成 grep / 编译器诊断那种 `path:行号` 或 `path:行号:列号`
    // （不是 `#L行号` 片段）。没有 `#` 片段时才从冒号后缀里抠行号，不然 `:2765`
    // 会被当成文件名的一部分拼进路径。
    let (path, fragment) = if fragment.is_empty() {
        match extract_trailing_line_number(path) {
            Some((base, line)) => (base, format!("L{line}")),
            None => (path, String::new()),
        }
    } else {
        (path, fragment.to_string())
    };
    if path.is_empty() {
        return None;
    }
    let absolute = if style.is_absolute(path) {
        path.to_string()
    } else {
        style.join(cwd?, path)
    };
    Some(FileLink {
        path: style.clean(&absolute),
        fragment,
    })
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
}
