//! 运行时那台机器上的路径。
//!
//! 协议里的 `cwd`、项目根、文件树路径都属于运行时，不是桌面所在的系统。远程运行时
//! 是 macOS / Linux，分隔符是 `/`。本机 Windows 运行时的路径带盘符或 `\`，按 Windows
//! 规则切。桌面不拿 `std::path` 去拼一条要发给另一台机器的路径。
//!
//! 本机客户端自己的文件（配置、数据库、系统文件框）继续用 `std::path`。

/// 拼一段。绝对的 `child` 整段替换；空的 `child` 只补一个末尾分隔符。
pub fn join(base: &str, child: &str) -> String {
    if is_absolute(child) || is_windows_absolute(child) {
        return child.to_string();
    }
    if is_windows_path(base) || is_windows_path(child) {
        return join_windows(base, child);
    }
    if child.is_empty() {
        if base.is_empty() || base.ends_with('/') {
            return base.to_string();
        }
        return format!("{base}/");
    }
    if base.is_empty() || base.ends_with('/') {
        format!("{base}{child}")
    } else {
        format!("{base}/{child}")
    }
}

/// 去掉最后一段。根 `/` 和空路径没有父目录；相对路径只剩一段时父目录是空串。
pub fn parent(path: &str) -> Option<String> {
    if is_windows_path(path) {
        return parent_windows(path);
    }
    let (absolute, parts) = significant_parts(path);
    if parts.is_empty() {
        return None;
    }
    let parent_parts = &parts[..parts.len() - 1];
    Some(render(absolute, parent_parts))
}

/// 最后一段普通名字。`.`、`..` 和根目录没有文件名。
pub fn file_name(path: &str) -> Option<&str> {
    if is_windows_path(path) {
        return file_name_windows(path);
    }
    let name = raw_name(path)?;
    if name == "." || name == ".." {
        None
    } else {
        Some(name)
    }
}

/// `path` 去掉 `prefix` 之后剩下的相对路径。前缀对不上就是 `None`；刚好相等是空串。
pub fn strip_prefix(path: &str, prefix: &str) -> Option<String> {
    if is_windows_path(path) || is_windows_path(prefix) {
        return strip_prefix_windows(path, prefix);
    }
    let (path_abs, path_parts) = significant_parts(path);
    let (prefix_abs, prefix_parts) = significant_parts(prefix);
    if path_abs != prefix_abs || path_parts.len() < prefix_parts.len() {
        return None;
    }
    if path_parts[..prefix_parts.len()] != prefix_parts[..] {
        return None;
    }
    Some(path_parts[prefix_parts.len()..].join("/"))
}

/// `prefix` 的每一段都是 `path` 的前缀。`/proj` 不包含 `/project`。
pub fn starts_with(path: &str, prefix: &str) -> bool {
    strip_prefix(path, prefix).is_some()
}

/// 扩展名，不含点。`.gitignore` 没有扩展名；`archive.tar.gz` 的扩展名是 `gz`。
pub fn extension(path: &str) -> Option<&str> {
    let name = file_name(path)?;
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    Some(ext)
}

/// 去掉重复的 `/` 和 `.`，并消掉能消的 `..`。绝对路径不会冒出根上面。
pub fn clean(path: &str) -> String {
    if is_windows_path(path) {
        return clean_windows(path);
    }
    let absolute = is_absolute(path);
    let mut stack: Vec<&str> = Vec::new();
    for part in path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if part == ".." {
            if stack.last().is_some_and(|top| *top != "..") {
                stack.pop();
            } else if !absolute {
                stack.push("..");
            }
        } else {
            stack.push(part);
        }
    }
    render(absolute, &stack)
}

fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

fn is_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.contains('\\')
        || path.starts_with("\\\\")
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('\\')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

fn join_windows(base: &str, child: &str) -> String {
    if child.is_empty() {
        if base.is_empty() || base.ends_with('\\') || base.ends_with('/') {
            return base.replace('/', "\\");
        }
        return format!("{}\\", base.replace('/', "\\"));
    }
    let base = base.replace('/', "\\");
    let child = child.replace('/', "\\");
    if base.is_empty() || base.ends_with('\\') {
        format!("{base}{child}")
    } else {
        format!("{base}\\{child}")
    }
}

fn split_windows(path: &str) -> (String, Vec<String>) {
    let path = path.replace('/', "\\");
    if let Some(rest) = path.strip_prefix("\\\\") {
        let mut pieces = rest.split('\\').filter(|part| !part.is_empty());
        let server = pieces.next().unwrap_or("");
        let share = pieces.next().unwrap_or("");
        let prefix = format!("\\\\{server}\\{share}");
        let parts = pieces.map(str::to_string).collect();
        return (prefix, parts);
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        let prefix = path[..2].to_string();
        let rest = path[2..].trim_start_matches('\\');
        let parts = if rest.is_empty() {
            Vec::new()
        } else {
            rest.split('\\')
                .filter(|part| !part.is_empty() && *part != ".")
                .map(str::to_string)
                .collect()
        };
        return (prefix, parts);
    }
    let parts = path
        .split('\\')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(str::to_string)
        .collect();
    (String::new(), parts)
}

fn render_windows(prefix: &str, parts: &[String]) -> String {
    if prefix.is_empty() {
        return parts.join("\\");
    }
    if parts.is_empty() {
        return if prefix.starts_with("\\\\") {
            prefix.to_string()
        } else {
            format!("{prefix}\\")
        };
    }
    format!("{prefix}\\{}", parts.join("\\"))
}

fn parent_windows(path: &str) -> Option<String> {
    let (prefix, parts) = split_windows(path);
    if parts.is_empty() {
        return None;
    }
    let parent = &parts[..parts.len() - 1];
    if prefix.is_empty() && parent.is_empty() {
        return Some(String::new());
    }
    Some(render_windows(&prefix, parent))
}

fn file_name_windows(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || trimmed.ends_with(':') {
        return None;
    }
    let name = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    if name == "." || name == ".." || name.contains(':') {
        None
    } else {
        Some(name)
    }
}

fn strip_prefix_windows(path: &str, prefix: &str) -> Option<String> {
    let (path_prefix, path_parts) = split_windows(path);
    let (prefix_prefix, prefix_parts) = split_windows(prefix);
    if !path_prefix.eq_ignore_ascii_case(&prefix_prefix) || path_parts.len() < prefix_parts.len() {
        return None;
    }
    if path_parts
        .iter()
        .zip(&prefix_parts)
        .any(|(path, prefix)| !path.eq_ignore_ascii_case(prefix))
    {
        return None;
    }
    Some(path_parts[prefix_parts.len()..].join("\\"))
}

fn clean_windows(path: &str) -> String {
    let normalized = path.replace('/', "\\");
    let (mut prefix, _) = split_windows(&normalized);
    let absolute = !prefix.is_empty();
    let mut stack: Vec<String> = Vec::new();
    let body = if normalized.starts_with("\\\\") {
        let mut pieces = normalized.trim_start_matches('\\').split('\\');
        let _server = pieces.next();
        let _share = pieces.next();
        pieces.collect::<Vec<_>>().join("\\")
    } else if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        normalized[2..].trim_start_matches('\\').to_string()
    } else {
        prefix.clear();
        normalized
    };
    for part in body.split('\\').filter(|part| !part.is_empty() && *part != ".") {
        if part == ".." {
            if stack.last().is_some_and(|top| top != "..") {
                stack.pop();
            } else if !absolute {
                stack.push("..".to_string());
            }
        } else {
            stack.push(part.to_string());
        }
    }
    render_windows(&prefix, &stack)
}

/// 去掉空段和 `.`，保留 `..` 和普通名字。
fn significant_parts(path: &str) -> (bool, Vec<&str>) {
    let parts = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    (is_absolute(path), parts)
}

fn raw_name(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.rsplit('/').next().unwrap_or(trimmed))
}

fn render(absolute: bool, parts: &[&str]) -> String {
    if absolute {
        if parts.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", parts.join("/"))
        }
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_uses_slash_and_keeps_absolute_child() {
        assert_eq!(join("/home/u/proj", "src"), "/home/u/proj/src");
        assert_eq!(join("/home/u/proj/", "src"), "/home/u/proj/src");
        assert_eq!(
            join("/home/u/proj", "src/main.rs"),
            "/home/u/proj/src/main.rs"
        );
        assert_eq!(join("/home/u/proj", "/etc/hosts"), "/etc/hosts");
        assert_eq!(join("/", "src"), "/src");
        assert_eq!(join("", "src"), "src");
        assert_eq!(join("/home/u/proj", ""), "/home/u/proj/");
        assert_eq!(join("/home/u/proj/", ""), "/home/u/proj/");
        assert_eq!(join("", ""), "");
    }

    #[test]
    fn parent_stops_at_root_and_keeps_relative_empty() {
        assert_eq!(parent("/home/u/proj"), Some("/home/u".into()));
        assert_eq!(parent("/home/u/proj/"), Some("/home/u".into()));
        assert_eq!(parent("/home"), Some("/".into()));
        assert_eq!(parent("/"), None);
        assert_eq!(parent(""), None);
        assert_eq!(parent("src/main.rs"), Some("src".into()));
        assert_eq!(parent("src"), Some("".into()));
        assert_eq!(parent("/home/u/proj/./src"), Some("/home/u/proj".into()));
    }

    #[test]
    fn file_name_skips_root_dot_and_dotdot() {
        assert_eq!(file_name("/home/u/proj"), Some("proj"));
        assert_eq!(file_name("/home/u/proj/"), Some("proj"));
        assert_eq!(file_name("Makefile"), Some("Makefile"));
        assert_eq!(file_name(".gitignore"), Some(".gitignore"));
        assert_eq!(file_name("/"), None);
        assert_eq!(file_name(""), None);
        assert_eq!(file_name("."), None);
        assert_eq!(file_name(".."), None);
        assert_eq!(file_name("/home/.."), None);
    }

    #[test]
    fn strip_prefix_is_segment_wise() {
        assert_eq!(
            strip_prefix("/home/u/proj/src/main.rs", "/home/u/proj").as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            strip_prefix("/home/u/proj/src", "/home/u/proj/").as_deref(),
            Some("src")
        );
        assert_eq!(
            strip_prefix("/home/u/proj", "/home/u/proj").as_deref(),
            Some("")
        );
        assert_eq!(
            strip_prefix("/home/u/proj", "/").as_deref(),
            Some("home/u/proj")
        );
        assert_eq!(strip_prefix("/project", "/proj"), None);
        assert_eq!(strip_prefix("/a/b", "/a/b/c"), None);
        assert_eq!(strip_prefix("src/main.rs", "/src"), None);
        assert_eq!(strip_prefix("/a/./b/c", "/a/b").as_deref(), Some("c"));
    }

    #[test]
    fn starts_with_requires_a_whole_segment() {
        assert!(starts_with("/home/u/proj/src", "/home/u/proj"));
        assert!(starts_with("/home/u/proj", "/home/u/proj"));
        assert!(starts_with("/home/u/proj/", "/home/u/proj"));
        assert!(!starts_with("/home/u/project", "/home/u/proj"));
        assert!(!starts_with("/home/u/proj", "/home/u/proj/src"));
        assert!(!starts_with("src/main.rs", "/src"));
    }

    #[test]
    fn extension_follows_the_file_name() {
        assert_eq!(extension("/repo/src/main.rs"), Some("rs"));
        assert_eq!(extension("shot.PNG"), Some("PNG"));
        assert_eq!(extension("/tmp/archive.tar.gz"), Some("gz"));
        assert_eq!(extension("Makefile"), None);
        assert_eq!(extension(".gitignore"), None);
        assert_eq!(extension("/repo/README"), None);
        assert_eq!(extension("foo."), Some(""));
    }

    #[test]
    fn clean_collapses_dot_and_dotdot() {
        assert_eq!(clean("/home/u/proj/../src//./lib"), "/home/u/src/lib");
        assert_eq!(clean("/../etc"), "/etc");
        assert_eq!(clean("a/../../b"), "../b");
        assert_eq!(clean(""), "");
        assert_eq!(clean("/"), "/");
        assert_eq!(clean("///a//b/"), "/a/b");
    }

    #[test]
    fn windows_join_keeps_drive_and_absolute_child() {
        assert_eq!(join(r"C:\proj", "src"), r"C:\proj\src");
        assert_eq!(join(r"C:\proj", "D:/other"), "D:/other");
        assert_eq!(join("/home/u", "src"), "/home/u/src");
    }

    #[test]
    fn windows_parent_file_name_strip_and_clean() {
        assert_eq!(parent(r"C:\proj\src"), Some(r"C:\proj".into()));
        assert_eq!(file_name(r"C:\proj\src"), Some("src"));
        assert_eq!(
            strip_prefix(r"C:\Proj\Src\main.rs", r"c:\proj").as_deref(),
            Some(r"Src\main.rs")
        );
        assert_eq!(clean("C:/proj/../src"), r"C:\src");
    }
}
