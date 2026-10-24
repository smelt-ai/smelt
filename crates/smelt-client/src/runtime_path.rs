//! 运行时那台机器上的路径。
//!
//! 协议里的 `cwd`、项目根、文件树路径都属于运行时，不是桌面所在的系统。第 1 步运行时
//! 只有 macOS 和 Linux，分隔符固定是 `/`。桌面在 Windows 上如果用 `std::path::Path::join`，
//! 会把 `\` 拼进发给 Linux 的路径。
//!
//! 本机客户端自己的文件（配置、数据库、系统文件框）继续用 `std::path`。

/// 拼一段。绝对的 `child` 整段替换；空的 `child` 只补一个末尾 `/`。
pub fn join(base: &str, child: &str) -> String {
    if is_absolute(child) {
        return child.to_string();
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
    let (absolute, parts) = significant_parts(path);
    if parts.is_empty() {
        return None;
    }
    let parent_parts = &parts[..parts.len() - 1];
    Some(render(absolute, parent_parts))
}

/// 最后一段普通名字。`.`、`..` 和根目录没有文件名。
pub fn file_name(path: &str) -> Option<&str> {
    let name = raw_name(path)?;
    if name == "." || name == ".." {
        None
    } else {
        Some(name)
    }
}

/// `path` 去掉 `prefix` 之后剩下的相对路径。前缀对不上就是 `None`；刚好相等是空串。
pub fn strip_prefix(path: &str, prefix: &str) -> Option<String> {
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
}
