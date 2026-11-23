//! 运行时那台机器上的路径。
//!
//! 协议里的 `cwd`、项目根、文件树路径都属于运行时，不是桌面所在的系统。桌面不拿
//! `std::path` 去拼一条要发给另一台机器的路径。
//!
//! 按哪种系统的规则切由调用方用 [`PathStyle`] 说明，不从字符串里猜：POSIX 的文件名
//! 可以带 `\`，`C:` 也可以是一个普通的目录名。
//!
//! 本机客户端自己的文件（配置、数据库、系统文件框）继续用 `std::path`。

use typed_path::{
    Utf8Components as _, Utf8UnixComponent, Utf8UnixPath, Utf8WindowsComponent, Utf8WindowsPath,
    Utf8WindowsPrefix, Utf8WindowsPrefixComponent,
};

/// 路径属于哪种系统。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStyle {
    Posix,
    Windows,
}

impl PathStyle {
    /// 本机运行时和桌面在同一台机器上。
    pub const LOCAL: Self = if cfg!(windows) {
        Self::Windows
    } else {
        Self::Posix
    };

    /// 运行时 `system.describe` 报的 `os`。没报（老运行时）按 POSIX。
    pub fn of_os(os: Option<&str>) -> Self {
        match os {
            Some("windows") => Self::Windows,
            _ => Self::Posix,
        }
    }

    /// 拼一段。绝对的 `child` 整段替换；空的 `child` 只补一个末尾分隔符。
    pub fn join(self, base: &str, child: &str) -> String {
        match self {
            Self::Posix => join_posix(base, child),
            Self::Windows => join_windows(base, child),
        }
    }

    /// 绝对路径。Windows 认盘符根、UNC、verbatim 和 `\` 开头，也认 `/` 开头（运行时有时这样报）；
    /// 盘符相对路径如 `C:foo` 不算绝对路径。
    pub fn is_absolute(self, path: &str) -> bool {
        match self {
            Self::Posix => Utf8UnixPath::new(path).is_absolute(),
            Self::Windows => Utf8WindowsPath::new(path).has_root(),
        }
    }

    /// 去掉最后一段。根和空路径没有父目录；相对路径只剩一段时父目录是空串。
    pub fn parent(self, path: &str) -> Option<String> {
        match self {
            Self::Posix => parent_posix(path),
            Self::Windows => parent_windows(path),
        }
    }

    /// 最后一段普通名字。`.`、`..` 和根目录没有文件名。
    pub fn file_name(self, path: &str) -> Option<&str> {
        match self {
            Self::Posix => Utf8UnixPath::new(path).file_name(),
            Self::Windows => Utf8WindowsPath::new(path).file_name(),
        }
    }

    /// `path` 去掉 `prefix` 之后剩下的相对路径。前缀对不上就是 `None`；刚好相等是空串。
    /// Windows 不区分大小写。
    pub fn strip_prefix(self, path: &str, prefix: &str) -> Option<String> {
        match self {
            Self::Posix => strip_prefix_posix(path, prefix),
            Self::Windows => strip_prefix_windows(path, prefix),
        }
    }

    /// `prefix` 的每一段都是 `path` 的前缀。`/proj` 不包含 `/project`。
    pub fn starts_with(self, path: &str, prefix: &str) -> bool {
        self.strip_prefix(path, prefix).is_some()
    }

    /// 扩展名，不含点。`.gitignore` 没有扩展名；`archive.tar.gz` 的扩展名是 `gz`。
    pub fn extension(self, path: &str) -> Option<&str> {
        let name = self.file_name(path)?;
        let (stem, ext) = name.rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        Some(ext)
    }

    /// 去掉重复的分隔符和 `.`，并消掉能消的 `..`。绝对路径不会冒出根上面。
    pub fn clean(self, path: &str) -> String {
        match self {
            Self::Posix => clean_posix(path),
            Self::Windows => clean_windows(path),
        }
    }
}

fn join_posix(base: &str, child: &str) -> String {
    if Utf8UnixPath::new(child).is_absolute() {
        return child.to_string();
    }
    if child.is_empty() {
        if base.is_empty() || base.ends_with('/') {
            return base.to_string();
        }
        return format!("{base}/");
    }
    Utf8UnixPath::new(base).join(child).into_string()
}

fn posix_parts(path: &str) -> (bool, Vec<&str>) {
    let mut has_root = false;
    let mut parts = Vec::new();
    for component in Utf8UnixPath::new(path).components() {
        match component {
            Utf8UnixComponent::RootDir => has_root = true,
            Utf8UnixComponent::CurDir => {}
            Utf8UnixComponent::ParentDir => parts.push(".."),
            Utf8UnixComponent::Normal(seg) => parts.push(seg),
        }
    }
    (has_root, parts)
}

fn render_posix(has_root: bool, parts: &[&str]) -> String {
    if has_root {
        if parts.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", parts.join("/"))
        }
    } else {
        parts.join("/")
    }
}

fn parent_posix(path: &str) -> Option<String> {
    let (has_root, parts) = posix_parts(path);
    if parts.is_empty() {
        return None;
    }
    Some(render_posix(has_root, &parts[..parts.len() - 1]))
}

fn strip_prefix_posix(path: &str, prefix: &str) -> Option<String> {
    let (path_root, path_parts) = posix_parts(path);
    let (prefix_root, prefix_parts) = posix_parts(prefix);
    if path_root != prefix_root || path_parts.len() < prefix_parts.len() {
        return None;
    }
    if path_parts[..prefix_parts.len()] != prefix_parts[..] {
        return None;
    }
    Some(path_parts[prefix_parts.len()..].join("/"))
}

fn clean_posix(path: &str) -> String {
    let (has_root, parts) = posix_parts(path);
    let mut stack: Vec<&str> = Vec::new();
    for part in parts {
        if part == ".." {
            if stack.last().is_some_and(|top| *top != "..") {
                stack.pop();
            } else if !has_root {
                stack.push("..");
            }
        } else {
            stack.push(part);
        }
    }
    render_posix(has_root, &stack)
}

fn join_windows(base: &str, child: &str) -> String {
    let child_path = Utf8WindowsPath::new(child);
    if child_path.has_root() || child_path.components().has_prefix() {
        return child.to_string();
    }
    let normalized_base = normalize_windows_slashes(base);
    if child.is_empty() {
        if normalized_base.is_empty() || normalized_base.ends_with('\\') {
            return normalized_base;
        }
        return format!("{normalized_base}\\");
    }
    let normalized_child = child.replace('/', "\\");
    if normalized_base.is_empty() || normalized_base.ends_with('\\') {
        format!("{normalized_base}{normalized_child}")
    } else {
        format!("{normalized_base}\\{normalized_child}")
    }
}

/// 将普通 Windows 路径里的 `/` 规范为 `\`；verbatim 前缀本身已由 Windows 规定使用 `\`，保持原样。
fn normalize_windows_slashes(path: &str) -> String {
    if path.starts_with(r"\\?\") {
        path.to_string()
    } else {
        path.replace('/', "\\")
    }
}

struct WindowsParts<'a> {
    prefix: Option<Utf8WindowsPrefixComponent<'a>>,
    /// 是否带物理或隐式根（`C:\`、`\`、`\\server\share`、`\\?\C:\` 为 true；`C:foo`、`foo` 为 false）。
    has_root: bool,
    parts: Vec<&'a str>,
}

fn windows_parts(path: &str) -> WindowsParts<'_> {
    let comps = Utf8WindowsPath::new(path).components();
    let has_root = comps.has_root();
    let mut prefix = None;
    let mut parts = Vec::new();
    for component in comps {
        match component {
            Utf8WindowsComponent::Prefix(p) => prefix = Some(p),
            Utf8WindowsComponent::RootDir | Utf8WindowsComponent::CurDir => {}
            Utf8WindowsComponent::ParentDir => parts.push(".."),
            Utf8WindowsComponent::Normal(seg) => parts.push(seg),
        }
    }
    WindowsParts {
        prefix,
        has_root,
        parts,
    }
}

fn format_windows_prefix(prefix: Utf8WindowsPrefixComponent<'_>) -> String {
    match prefix.kind() {
        Utf8WindowsPrefix::Verbatim(_)
        | Utf8WindowsPrefix::VerbatimUNC(..)
        | Utf8WindowsPrefix::VerbatimDisk(_) => prefix.as_str().to_string(),
        Utf8WindowsPrefix::DeviceNS(_)
        | Utf8WindowsPrefix::UNC(..)
        | Utf8WindowsPrefix::Disk(_) => prefix.as_str().replace('/', "\\"),
    }
}

fn render_windows_parts(
    prefix: Option<Utf8WindowsPrefixComponent<'_>>,
    has_root: bool,
    parts: &[&str],
) -> String {
    match prefix {
        None => {
            if has_root {
                if parts.is_empty() {
                    r"\".to_string()
                } else {
                    format!(r"\{}", parts.join(r"\"))
                }
            } else {
                parts.join(r"\")
            }
        }
        Some(prefix) => {
            let prefix_str = format_windows_prefix(prefix);
            let is_disk = matches!(
                prefix.kind(),
                Utf8WindowsPrefix::Disk(_) | Utf8WindowsPrefix::VerbatimDisk(_)
            );
            if parts.is_empty() {
                if is_disk && has_root {
                    format!(r"{prefix_str}\")
                } else {
                    prefix_str
                }
            } else if is_disk && !has_root {
                format!("{prefix_str}{}", parts.join(r"\"))
            } else {
                format!(r"{prefix_str}\{}", parts.join(r"\"))
            }
        }
    }
}

fn windows_prefix_eq(
    a: Option<Utf8WindowsPrefixComponent<'_>>,
    b: Option<Utf8WindowsPrefixComponent<'_>>,
) -> bool {
    match (a.map(|p| p.kind()), b.map(|p| p.kind())) {
        (None, None) => true,
        (Some(Utf8WindowsPrefix::Disk(da)), Some(Utf8WindowsPrefix::Disk(db))) => da == db,
        (Some(Utf8WindowsPrefix::VerbatimDisk(da)), Some(Utf8WindowsPrefix::VerbatimDisk(db))) => {
            da == db
        }
        (Some(Utf8WindowsPrefix::UNC(sa, sha)), Some(Utf8WindowsPrefix::UNC(sb, shb)))
        | (
            Some(Utf8WindowsPrefix::VerbatimUNC(sa, sha)),
            Some(Utf8WindowsPrefix::VerbatimUNC(sb, shb)),
        ) => sa.eq_ignore_ascii_case(sb) && sha.eq_ignore_ascii_case(shb),
        (Some(Utf8WindowsPrefix::Verbatim(va)), Some(Utf8WindowsPrefix::Verbatim(vb))) => {
            va.eq_ignore_ascii_case(vb)
        }
        (Some(Utf8WindowsPrefix::DeviceNS(da)), Some(Utf8WindowsPrefix::DeviceNS(db))) => {
            da.eq_ignore_ascii_case(db)
        }
        _ => false,
    }
}

fn parent_windows(path: &str) -> Option<String> {
    let parsed = windows_parts(path);
    if parsed.parts.is_empty() {
        return None;
    }
    let parent = &parsed.parts[..parsed.parts.len() - 1];
    Some(render_windows_parts(parsed.prefix, parsed.has_root, parent))
}

fn strip_prefix_windows(path: &str, prefix: &str) -> Option<String> {
    let path_parsed = windows_parts(path);
    let prefix_parsed = windows_parts(prefix);
    if !windows_prefix_eq(path_parsed.prefix, prefix_parsed.prefix)
        || path_parsed.has_root != prefix_parsed.has_root
        || path_parsed.parts.len() < prefix_parsed.parts.len()
    {
        return None;
    }
    if path_parsed
        .parts
        .iter()
        .zip(&prefix_parsed.parts)
        .any(|(a, b)| !a.eq_ignore_ascii_case(b))
    {
        return None;
    }
    Some(path_parsed.parts[prefix_parsed.parts.len()..].join(r"\"))
}

fn clean_windows(path: &str) -> String {
    let parsed = windows_parts(path);
    let mut stack: Vec<&str> = Vec::new();
    for part in parsed.parts {
        if part == ".." {
            if stack.last().is_some_and(|top| *top != "..") {
                stack.pop();
            } else if !parsed.has_root {
                stack.push("..");
            }
        } else {
            stack.push(part);
        }
    }
    render_windows_parts(parsed.prefix, parsed.has_root, &stack)
}

#[cfg(test)]
mod tests {
    use super::PathStyle::{Posix, Windows};

    #[test]
    fn join_uses_slash_and_keeps_absolute_child() {
        assert_eq!(Posix.join("/home/u/proj", "src"), "/home/u/proj/src");
        assert_eq!(Posix.join("/home/u/proj/", "src"), "/home/u/proj/src");
        assert_eq!(
            Posix.join("/home/u/proj", "src/main.rs"),
            "/home/u/proj/src/main.rs"
        );
        assert_eq!(Posix.join("/home/u/proj", "/etc/hosts"), "/etc/hosts");
        assert_eq!(Posix.join("/", "src"), "/src");
        assert_eq!(Posix.join("", "src"), "src");
        assert_eq!(Posix.join("/home/u/proj", ""), "/home/u/proj/");
        assert_eq!(Posix.join("/home/u/proj/", ""), "/home/u/proj/");
        assert_eq!(Posix.join("", ""), "");
    }

    #[test]
    fn parent_stops_at_root_and_keeps_relative_empty() {
        assert_eq!(Posix.parent("/home/u/proj"), Some("/home/u".into()));
        assert_eq!(Posix.parent("/home/u/proj/"), Some("/home/u".into()));
        assert_eq!(Posix.parent("/home"), Some("/".into()));
        assert_eq!(Posix.parent("/"), None);
        assert_eq!(Posix.parent(""), None);
        assert_eq!(Posix.parent("src/main.rs"), Some("src".into()));
        assert_eq!(Posix.parent("src"), Some("".into()));
        assert_eq!(Posix.parent("/home/u/proj/./src"), Some("/home/u/proj".into()));
    }

    #[test]
    fn file_name_skips_root_dot_and_dotdot() {
        assert_eq!(Posix.file_name("/home/u/proj"), Some("proj"));
        assert_eq!(Posix.file_name("/home/u/proj/"), Some("proj"));
        assert_eq!(Posix.file_name("Makefile"), Some("Makefile"));
        assert_eq!(Posix.file_name(".gitignore"), Some(".gitignore"));
        assert_eq!(Posix.file_name("/"), None);
        assert_eq!(Posix.file_name(""), None);
        assert_eq!(Posix.file_name("."), None);
        assert_eq!(Posix.file_name(".."), None);
        assert_eq!(Posix.file_name("/home/.."), None);
    }

    #[test]
    fn strip_prefix_is_segment_wise() {
        assert_eq!(
            Posix
                .strip_prefix("/home/u/proj/src/main.rs", "/home/u/proj")
                .as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            Posix.strip_prefix("/home/u/proj/src", "/home/u/proj/").as_deref(),
            Some("src")
        );
        assert_eq!(
            Posix.strip_prefix("/home/u/proj", "/home/u/proj").as_deref(),
            Some("")
        );
        assert_eq!(
            Posix.strip_prefix("/home/u/proj", "/").as_deref(),
            Some("home/u/proj")
        );
        assert_eq!(Posix.strip_prefix("/project", "/proj"), None);
        assert_eq!(Posix.strip_prefix("/a/b", "/a/b/c"), None);
        assert_eq!(Posix.strip_prefix("src/main.rs", "/src"), None);
        assert_eq!(Posix.strip_prefix("/a/./b/c", "/a/b").as_deref(), Some("c"));
    }

    #[test]
    fn starts_with_requires_a_whole_segment() {
        assert!(Posix.starts_with("/home/u/proj/src", "/home/u/proj"));
        assert!(Posix.starts_with("/home/u/proj", "/home/u/proj"));
        assert!(Posix.starts_with("/home/u/proj/", "/home/u/proj"));
        assert!(!Posix.starts_with("/home/u/project", "/home/u/proj"));
        assert!(!Posix.starts_with("/home/u/proj", "/home/u/proj/src"));
        assert!(!Posix.starts_with("src/main.rs", "/src"));
    }

    #[test]
    fn extension_follows_the_file_name() {
        assert_eq!(Posix.extension("/repo/src/main.rs"), Some("rs"));
        assert_eq!(Posix.extension("shot.PNG"), Some("PNG"));
        assert_eq!(Posix.extension("/tmp/archive.tar.gz"), Some("gz"));
        assert_eq!(Posix.extension("Makefile"), None);
        assert_eq!(Posix.extension(".gitignore"), None);
        assert_eq!(Posix.extension("/repo/README"), None);
        assert_eq!(Posix.extension("foo."), Some(""));
    }

    #[test]
    fn clean_collapses_dot_and_dotdot() {
        assert_eq!(Posix.clean("/home/u/proj/../src//./lib"), "/home/u/src/lib");
        assert_eq!(Posix.clean("/../etc"), "/etc");
        assert_eq!(Posix.clean("a/../../b"), "../b");
        assert_eq!(Posix.clean(""), "");
        assert_eq!(Posix.clean("/"), "/");
        assert_eq!(Posix.clean("///a//b/"), "/a/b");
    }

    /// POSIX 文件名可以带 `\`，`C:` 也可以是普通的目录名。都按 POSIX 规则走：
    /// 不换分隔符，不按 Windows 拆，也不大小写不敏感地比较。
    #[test]
    fn posix_names_that_look_like_windows_stay_posix() {
        assert_eq!(Posix.join("/home/u/proj", r"a\b.txt"), r"/home/u/proj/a\b.txt");
        assert_eq!(Posix.join("/home/u/proj", r"C:\x"), r"/home/u/proj/C:\x");
        assert_eq!(Posix.parent(r"/home/u/proj/a\b.txt"), Some("/home/u/proj".into()));
        assert_eq!(Posix.file_name(r"/home/u/proj/a\b.txt"), Some(r"a\b.txt"));
        assert_eq!(Posix.file_name(r"a\b.txt"), Some(r"a\b.txt"));
        assert_eq!(Posix.parent("C:/proj"), Some("C:".into()));
        assert_eq!(
            Posix
                .strip_prefix(r"/home/u/proj/a\b.txt", "/home/u/proj")
                .as_deref(),
            Some(r"a\b.txt")
        );
        assert_eq!(Posix.strip_prefix("/home/u/Proj/x", "/home/u/proj"), None);
        assert_eq!(Posix.clean(r"/home/u/./a\b.txt"), r"/home/u/a\b.txt");
    }

    #[test]
    fn windows_join_keeps_drive_and_absolute_child() {
        assert_eq!(Windows.join(r"C:\proj", "src"), r"C:\proj\src");
        assert_eq!(Windows.join(r"C:\proj", "src/main.rs"), r"C:\proj\src\main.rs");
        assert_eq!(Windows.join(r"C:\proj", "D:/other"), "D:/other");
        assert_eq!(Windows.join(r"C:\proj", r"\\server\share"), r"\\server\share");
    }

    #[test]
    fn windows_parent_file_name_strip_and_clean() {
        assert_eq!(Windows.parent(r"C:\proj\src"), Some(r"C:\proj".into()));
        assert_eq!(Windows.file_name(r"C:\proj\src"), Some("src"));
        assert_eq!(Windows.file_name("C:/proj/src"), Some("src"));
        assert_eq!(
            Windows
                .strip_prefix(r"C:\Proj\Src\main.rs", r"c:\proj")
                .as_deref(),
            Some(r"Src\main.rs")
        );
        assert!(Windows.starts_with("C:/proj/src", r"C:\proj"));
        assert_eq!(Windows.clean("C:/proj/../src"), r"C:\src");
        assert_eq!(Windows.extension(r"C:\proj\main.RS"), Some("RS"));
    }

    #[test]
    fn windows_root_relative_drive_relative_and_verbatim_paths() {
        // 1. 盘符相对根路径 `\foo\bar` 不得吞掉前导 `\`，也不得与相对路径混淆
        assert_eq!(Windows.clean(r"\foo\..\bar"), r"\bar");
        assert_eq!(Windows.clean(r"\.."), r"\");
        assert_eq!(Windows.parent(r"\foo"), Some(r"\".into()));
        assert_eq!(Windows.parent(r"\"), None);
        assert_eq!(Windows.strip_prefix(r"foo\bar", r"\foo"), None);
        assert_eq!(
            Windows.strip_prefix(r"\Foo\Bar", r"\foo").as_deref(),
            Some("Bar")
        );

        // 2. 盘符相对路径 `C:foo` 不得与盘符绝对路径 `C:\foo` 混淆
        assert!(!Windows.is_absolute("C:foo"));
        assert_eq!(Windows.clean("C:foo"), "C:foo");
        assert_eq!(Windows.clean(r"C:foo\..\bar"), "C:bar");
        assert_eq!(Windows.parent("C:foo"), Some("C:".into()));
        assert_eq!(Windows.parent("C:"), None);
        assert_eq!(Windows.strip_prefix(r"C:\foo\bar", "C:foo"), None);
        assert_eq!(Windows.strip_prefix(r"C:foo\bar", r"C:\foo"), None);

        // 3. Verbatim 与 Verbatim UNC 路径不得误切成普通 UNC
        assert!(Windows.is_absolute(r"\\?\C:\proj\src"));
        assert!(Windows.is_absolute(r"\\?\UNC\server\share\a"));
        assert_eq!(Windows.parent(r"\\?\C:\proj"), Some(r"\\?\C:\".into()));
        assert_eq!(Windows.parent(r"\\?\C:\"), None);
        assert_eq!(Windows.clean(r"\\?\C:\proj\..\src"), r"\\?\C:\src");
        assert_eq!(
            Windows.clean(r"\\?\UNC\server\share\a\..\b"),
            r"\\?\UNC\server\share\b"
        );
        assert_eq!(
            Windows
                .strip_prefix(r"\\?\C:\Proj\Src", r"\\?\c:\proj")
                .as_deref(),
            Some("Src")
        );
        assert_eq!(
            Windows
                .strip_prefix(r"\\server\share\sub\file.rs", r"\\SERVER\Share")
                .as_deref(),
            Some(r"sub\file.rs")
        );
    }
}
