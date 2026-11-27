//! 界面图标目录：资源路径与内嵌 Phosphor SVG 字节表。

use gpui::SharedString;

macro_rules! icon_names {
    ($($variant:ident => $name:literal),* $(,)?) => {
        /// 界面图标。名字是 smelt 的叫法，图形来自 Phosphor（bold 粗细，系统标志这类品牌标志用
        /// fill；Phosphor 画走样的 Linux 企鹅取 Font Awesome），对应关系在
        /// `assets/phosphor/icons.txt`，svg 由 `scripts/vendor-icons.py` 按清单取。只收用到的：加一枚
        /// 先在清单加一行跑脚本，再在这里加一行。
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum IconName {
            $($variant),*
        }

        impl IconName {
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// 资源路径 `icons/<名字>.svg`。插件（包括 JS 插件）登记图标时写这个路径。
            pub const fn asset_path(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("icons/", $name, ".svg")),*
                }
            }

            pub(crate) const fn svg(self) -> &'static [u8] {
                match self {
                    $(Self::$variant => include_bytes!(concat!("../assets/phosphor/", $name, ".svg"))),*
                }
            }
        }
    };
}

icon_names! {
    Plus => "plus",
    Minus => "minus",
    Close => "close",
    Check => "check",
    Search => "search",
    Copy => "copy",
    Ellipsis => "ellipsis",
    Loader => "loader",
    Refresh => "refresh",
    Restore => "restore",
    Undo => "undo",
    Pencil => "pencil",
    SquarePen => "square-pen",
    Trash => "trash",
    Replace => "replace",
    CaseSensitive => "case-sensitive",
    ChevronDown => "chevron-down",
    ChevronLeft => "chevron-left",
    ChevronRight => "chevron-right",
    ChevronsUpDown => "chevrons-up-down",
    ArrowUp => "arrow-up",
    ArrowDown => "arrow-down",
    ArrowRight => "arrow-right",
    ExternalLink => "external-link",
    Info => "info",
    Warning => "warning",
    CircleAlert => "circle-alert",
    CircleX => "circle-x",
    CircleCheck => "circle-check",
    Clock => "clock",
    Calendar => "calendar",
    Bell => "bell",
    BellOff => "bell-off",
    Pin => "pin",
    PinOff => "pin-off",
    Star => "star",
    StarFill => "star-fill",
    Eye => "eye",
    EyeOff => "eye-off",
    Play => "play",
    Pause => "pause",
    Stop => "stop",
    Asterisk => "asterisk",
    Sparkles => "sparkles",
    Terminal => "terminal",
    Bot => "bot",
    MessageSquare => "message-square",
    MessagesSquare => "messages-square",
    Inbox => "inbox",
    Archive => "archive",
    File => "file",
    Folder => "folder",
    FolderOpen => "folder-open",
    FolderPlus => "folder-plus",
    FolderTree => "folder-tree",
    Repositories => "repositories",
    GitBranch => "git-branch",
    BookOpen => "book-open",
    Brain => "brain",
    Globe => "globe",
    Puzzle => "puzzle",
    Settings => "settings",
    SlidersHorizontal => "sliders-horizontal",
    Wrench => "wrench",
    Workflow => "workflow",
    Webhook => "webhook",
    Inspector => "inspector",
    Palette => "palette",
    Keyboard => "keyboard",
    Languages => "languages",
    Sun => "sun",
    Coffee => "coffee",
    Gauge => "gauge",
    Cpu => "cpu",
    MemoryStick => "memory-stick",
    AppWindow => "app-window",
    LayoutDashboard => "layout-dashboard",
    LayoutGrid => "layout-grid",
    PanelLeft => "panel-left",
    PanelRight => "panel-right",
    PanelLeftFilled => "panel-left-filled",
    PanelRightFilled => "panel-right-filled",
    Maximize => "maximize",
    Minimize => "minimize",
    Monitor => "monitor",
    Laptop => "laptop",
    Apple => "apple",
    Linux => "linux",
    Windows => "windows",
}

impl IconName {
    pub fn path(self) -> SharedString {
        SharedString::new_static(self.asset_path())
    }

    /// 资源路径对应的图标；不是 `icons/<名字>.svg` 或没收这枚时返回 `None`。
    pub fn from_asset_path(path: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|icon| icon.asset_path() == path)
    }
}

/// 能按资源路径画出来的图标：[`IconName`]，以及组件的图标令牌。
pub trait IconNamed {
    fn path(self) -> SharedString;
}

impl IconNamed for IconName {
    fn path(self) -> SharedString {
        IconName::path(self)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// 清单和这张表是两份手写的名单，要一一对上：清单多了是白收，表多了编不过。
    #[test]
    fn table_matches_manifest() {
        let manifest: BTreeSet<_> = include_str!("../assets/phosphor/icons.txt")
            .lines()
            .filter_map(|line| line.split('#').next()?.split_whitespace().next())
            .map(|name| format!("icons/{name}.svg"))
            .collect();
        let table: BTreeSet<_> = IconName::ALL
            .iter()
            .map(|icon| icon.asset_path().to_owned())
            .collect();
        assert_eq!(manifest, table);
    }

    /// 变体名和资源名是同一个名字的两种写法，`StarFill` ↔ `star-fill`。
    #[test]
    fn variant_names_match_asset_names() {
        for icon in IconName::ALL {
            let mut kebab = String::new();
            for (index, ch) in format!("{icon:?}").chars().enumerate() {
                if ch.is_ascii_uppercase() && index > 0 {
                    kebab.push('-');
                }
                kebab.push(ch.to_ascii_lowercase());
            }
            assert_eq!(icon.asset_path(), format!("icons/{kebab}.svg"));
        }
    }

    #[test]
    fn asset_paths_round_trip() {
        for icon in IconName::ALL {
            assert_eq!(IconName::from_asset_path(icon.asset_path()), Some(*icon));
            assert!(!icon.svg().is_empty());
        }
        assert_eq!(IconName::from_asset_path("icons/nope.svg"), None);
        assert_eq!(IconName::from_asset_path("smelt-icons/trash.svg"), None);
    }
}
