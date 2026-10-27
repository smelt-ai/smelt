//! 设置窗口的信息架构：左侧按分组列出各页，每页一个独立内容页。
//!
//! gpui-component 的 `Settings` 在 `groups.len() > 1` 时会画二级菜单，但点击只是
//! 滚动到分组，右侧仍是整页。这里把每一页做成独立 `SettingsSection`，由壳层只渲染
//! 当前这一页；页内再用 `SettingGroup` 分节。

use gpui_component::{Icon, IconName};

/// 设置窗口当前选中的内容页。深链接（检查更新、外观、机器、资源）也走这里。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SettingsSection {
    /// 主题、字号与字体、窗口与背景。
    #[default]
    Appearance,
    Notifications,
    Shortcuts,
    PanelPlugins,
    /// 本机 Agent CLI、Hooks 接入、启动项、对话命令与多工作区。都只作用于本机运行时。
    Agents,
    /// 连到其他机器的运行时，以及让配对过的设备连进这台机器。
    Remote,
    /// 停在这一页时才采样。
    Resources,
    /// 后台服务、旧版本残留。
    Maintenance,
    /// 版本、应用更新（macOS、Windows）、帮助与反馈。
    About,
}

impl SettingsSection {
    pub fn appearance() -> Self {
        Self::Appearance
    }

    /// 检查更新的深链接。
    #[cfg(any(target_os = "macos", windows))]
    pub fn app_update() -> Self {
        Self::About
    }

    pub fn element_key(&self) -> gpui::SharedString {
        match self {
            Self::Appearance => "appearance".into(),
            Self::Notifications => "notifications".into(),
            Self::Shortcuts => "shortcuts".into(),
            Self::PanelPlugins => "panel-plugins".into(),
            Self::Agents => "agents".into(),
            Self::Remote => "remote".into(),
            Self::Resources => "resources".into(),
            Self::Maintenance => "maintenance".into(),
            Self::About => "about".into(),
        }
    }
}

#[derive(Clone)]
pub struct SettingsNavLeaf {
    pub title: String,
    pub icon: Icon,
    pub section: SettingsSection,
    pub keywords: Vec<String>,
}

#[derive(Clone)]
pub struct SettingsNavCategory {
    pub title: &'static str,
    pub keywords: &'static [&'static str],
    pub children: Vec<SettingsNavLeaf>,
}

/// 设置侧栏：三组，每组一行小标题加若干页。
pub fn settings_nav() -> Vec<SettingsNavCategory> {
    vec![
        SettingsNavCategory {
            title: "通用",
            keywords: &["常规", "偏好设置"],
            children: vec![
                leaf(
                    "外观",
                    IconName::Palette,
                    SettingsSection::Appearance,
                    &[
                        "主题",
                        "颜色模式",
                        "深色",
                        "暗色",
                        "夜间",
                        "浅色",
                        "亮色",
                        "配色",
                        "Default",
                        "Ayu",
                        "Catppuccin",
                        "界面字号",
                        "UI 字号",
                        "界面字体",
                        "字体大小",
                        "调整字号",
                        "缩放",
                        "terminal",
                        "终端",
                        "终端字号",
                        "代码字体",
                        "等宽字体",
                        "字体",
                        "窗口",
                        "背景",
                        "背景图",
                        "背景图片",
                        "壁纸",
                        "透明",
                        "透明度",
                        "不透明度",
                        "背景透明",
                        "标题栏玻璃",
                        "玻璃",
                        "液态玻璃",
                        "毛玻璃",
                    ],
                ),
                leaf(
                    "通知",
                    IconName::Bell,
                    SettingsSection::Notifications,
                    &[
                        "Agent",
                        "Agent通知",
                        "提醒",
                        "notification",
                        "系统通知",
                        "等待批准通知",
                        "等待输入通知",
                        "任务完成通知",
                        "任务失败通知",
                        "终端响铃通知",
                        "审批",
                        "批准",
                        "输入",
                        "完成",
                        "失败",
                        "响铃",
                        "bell",
                    ],
                ),
                leaf(
                    "快捷键",
                    Icon::new(gpui_kit_assets::IconName::Keyboard),
                    SettingsSection::Shortcuts,
                    &[
                        "键盘快捷键",
                        "键盘",
                        "按键",
                        "组合键",
                        "shortcut",
                        "keymap",
                        "cmd",
                        "command",
                    ],
                ),
                leaf(
                    "面板插件",
                    Icon::new(gpui_kit_assets::IconName::Puzzle),
                    SettingsSection::PanelPlugins,
                    &[
                        "插件",
                        "plugin",
                        "扩展",
                        "Tool Panel",
                        "右侧面板",
                        "导入插件",
                        "安装插件",
                        "删除插件",
                        "卸载插件",
                        "重新加载",
                        "gpui-shell",
                    ],
                ),
            ],
        },
        SettingsNavCategory {
            title: "Agent 与远程",
            keywords: &["运行时"],
            children: vec![
                leaf(
                    "Agent",
                    IconName::Bot,
                    SettingsSection::Agents,
                    &[
                        "智能体",
                        "AI",
                        "会话",
                        "运行环境",
                        "cli",
                        "安装",
                        "版本",
                        "检测",
                        "路径",
                        "claude",
                        "codex",
                        "copilot",
                        "grok",
                        "cursor",
                        "opencode",
                        "kiro",
                        "pi",
                        "hook",
                        "hooks",
                        "Agent Hooks",
                        "通知接入",
                        "权限接入",
                        "配置文件",
                        "快捷启动项",
                        "启动",
                        "launch",
                        "命令",
                        "+",
                        "加号",
                        "新建菜单",
                        "项目菜单",
                        "Agent 对话",
                        "adapter",
                        "适配器",
                        "workspace",
                        "工作区",
                        "多工作区",
                        "config dir",
                        "启动命令",
                        "参数",
                    ],
                ),
                leaf(
                    "远程",
                    IconName::Globe,
                    SettingsSection::Remote,
                    &[
                        "多机器",
                        "机器",
                        "远程机器",
                        "添加机器",
                        "添加远程机器",
                        "其他运行时",
                        "配对串",
                        "smeltd pair",
                        "离线",
                        "版本不兼容",
                        "远程访问",
                        "开启远程",
                        "Relay 地址",
                        "允许远程写入",
                        "手机",
                        "移动端",
                        "配对",
                        "二维码",
                        "token",
                        "iroh",
                        "relay",
                        "分享",
                        "只读",
                        "写入",
                    ],
                ),
            ],
        },
        SettingsNavCategory {
            title: "系统",
            keywords: &[],
            children: vec![
                leaf(
                    "资源占用",
                    IconName::MemoryStick,
                    SettingsSection::Resources,
                    &[
                        "资源",
                        "性能",
                        "内存",
                        "内存占用",
                        "CPU",
                        "占用",
                        "卡",
                        "慢",
                        "活动监视器",
                        "GPUI",
                    ],
                ),
                leaf(
                    "维护",
                    Icon::new(gpui_kit_assets::IconName::Wrench),
                    SettingsSection::Maintenance,
                    &[
                        "smeltd",
                        "守护进程",
                        "守护",
                        "后台",
                        "服务",
                        "后台服务",
                        "进程",
                        "重启",
                        "重启守护进程",
                        "重启以升级",
                        "会话服务",
                        "卡死",
                        "残留数据清理",
                        "清理",
                        "残留",
                        "存储",
                        "空间",
                        "缓存",
                        "清缓存",
                        "磁盘",
                        "旧数据",
                        "~/.smelt",
                        "高级",
                    ],
                ),
                leaf(
                    "关于",
                    IconName::Info,
                    SettingsSection::About,
                    &[
                        "软件版本",
                        "应用版本",
                        "版本",
                        "更新",
                        "检查更新",
                        "更新通道",
                        "自动下载安装",
                        "升级",
                        "下载",
                        "通道",
                        "生产版",
                        "内测版",
                        "自动更新",
                        "帮助",
                        "帮助文档",
                        "文档",
                        "反馈",
                        "反馈问题",
                        "日志",
                        "源码",
                        "GitHub",
                    ],
                ),
            ],
        },
    ]
}

fn leaf(
    title: &str,
    icon: impl Into<Icon>,
    section: SettingsSection,
    keywords: &[&str],
) -> SettingsNavLeaf {
    SettingsNavLeaf {
        title: title.to_string(),
        icon: icon.into(),
        section,
        keywords: keywords.iter().map(|word| (*word).to_string()).collect(),
    }
}

/// 按标题和关键词过滤侧栏。空查询返回完整树。
pub fn filter_settings_nav(nav: &[SettingsNavCategory], query: &str) -> Vec<SettingsNavCategory> {
    let query = query.trim();
    if query.is_empty() {
        return nav.to_vec();
    }
    let terms = query_terms(query);
    nav.iter()
        .filter_map(|category| {
            let category_hit = category_matches_exact(category, query);
            let children = category
                .children
                .iter()
                .filter(|child| {
                    let child_values = std::iter::once(child.title.as_str())
                        .chain(child.keywords.iter().map(String::as_str));
                    category_hit
                        || matches_query(
                            std::iter::once(category.title)
                                .chain(category.keywords.iter().copied())
                                .chain(child_values.clone()),
                            &terms,
                        ) && matches_any_term(child_values, &terms)
                })
                .cloned()
                .collect::<Vec<_>>();
            if children.is_empty() {
                None
            } else {
                let mut filtered = category.clone();
                filtered.children = children;
                Some(filtered)
            }
        })
        .collect()
}

fn category_matches_exact(category: &SettingsNavCategory, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    std::iter::once(category.title)
        .chain(category.keywords.iter().copied())
        .any(|value| value.to_lowercase() == query)
}

/// 用户经常会混输中英文（`agent 通知`、`API key`），也会带连字符或空格。
/// 每个词可以命中“分类语境 + 入口标题 + 同义词”的任意位置，但所有词都必须命中，
/// 避免一个宽泛的 `agent` 把整棵树都留下。
fn query_terms(query: &str) -> Vec<String> {
    let lowered = query.to_lowercase();
    let terms = lowered
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        vec![lowered]
    } else {
        terms
    }
}

fn matches_query<'a>(values: impl IntoIterator<Item = &'a str>, terms: &[String]) -> bool {
    let haystack = values
        .into_iter()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    let compact = haystack
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .collect::<String>();

    terms.iter().all(|term| {
        let compact_term = term
            .chars()
            .filter(|ch| ch.is_alphanumeric())
            .collect::<String>();
        haystack.contains(term) || (!compact_term.is_empty() && compact.contains(&compact_term))
    })
}

fn matches_any_term<'a>(values: impl IntoIterator<Item = &'a str>, terms: &[String]) -> bool {
    let values = values.into_iter().collect::<Vec<_>>();
    terms
        .iter()
        .any(|term| matches_query(values.iter().copied(), std::slice::from_ref(term)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(nav: &[SettingsNavCategory]) -> Vec<&str> {
        nav.iter().map(|category| category.title).collect()
    }

    fn child_titles(nav: &[SettingsNavCategory], title: &str) -> Vec<String> {
        nav.iter()
            .find(|category| category.title == title)
            .map(|category| {
                category
                    .children
                    .iter()
                    .map(|child| child.title.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn matching_sections(nav: &[SettingsNavCategory], query: &str) -> Vec<SettingsSection> {
        filter_settings_nav(nav, query)
            .into_iter()
            .flat_map(|category| category.children)
            .map(|child| child.section)
            .collect()
    }

    #[test]
    fn nav_is_two_groups_of_pages() {
        let nav = settings_nav();
        assert_eq!(titles(&nav), ["通用", "Agent 与远程", "系统"]);
        assert_eq!(
            child_titles(&nav, "通用"),
            ["外观", "通知", "快捷键", "面板插件"]
        );
        assert_eq!(child_titles(&nav, "Agent 与远程"), ["Agent", "远程"]);
        assert_eq!(child_titles(&nav, "系统"), ["资源占用", "维护", "关于"]);
        let sections = nav
            .iter()
            .flat_map(|category| category.children.iter())
            .map(|child| child.section.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            sections,
            [
                SettingsSection::Appearance,
                SettingsSection::Notifications,
                SettingsSection::Shortcuts,
                SettingsSection::PanelPlugins,
                SettingsSection::Agents,
                SettingsSection::Remote,
                SettingsSection::Resources,
                SettingsSection::Maintenance,
                SettingsSection::About,
            ]
        );
    }

    #[test]
    fn search_keeps_only_matching_pages_under_their_group() {
        let nav = settings_nav();
        let filtered = filter_settings_nav(&nav, "守护");
        assert_eq!(titles(&filtered), ["系统"]);
        assert_eq!(child_titles(&filtered, "系统"), ["维护"]);

        let by_group = filter_settings_nav(&nav, "通用");
        assert_eq!(child_titles(&by_group, "通用").len(), 4);
    }

    #[test]
    fn search_understands_user_tasks_and_multiple_terms() {
        let nav = settings_nav();
        assert_eq!(
            matching_sections(&nav, "agent通知"),
            [SettingsSection::Notifications]
        );
        assert_eq!(
            matching_sections(&nav, "后台服务"),
            [SettingsSection::Maintenance]
        );

        for (query, expected) in [
            ("任务完成通知", SettingsSection::Notifications),
            ("Relay 地址", SettingsSection::Remote),
            ("添加机器", SettingsSection::Remote),
            ("内存占用", SettingsSection::Resources),
            ("导入插件", SettingsSection::PanelPlugins),
            ("自动下载安装", SettingsSection::About),
            ("清缓存", SettingsSection::Maintenance),
            ("终端字号", SettingsSection::Appearance),
            ("毛玻璃", SettingsSection::Appearance),
            ("背景图", SettingsSection::Appearance),
            ("hook", SettingsSection::Agents),
            ("快捷启动项", SettingsSection::Agents),
            ("config dir", SettingsSection::Agents),
            ("反馈问题", SettingsSection::About),
            ("帮助文档", SettingsSection::About),
        ] {
            assert_eq!(matching_sections(&nav, query), [expected], "query: {query}");
        }
    }

    #[test]
    fn element_keys_are_unique() {
        let keys = settings_nav()
            .into_iter()
            .flat_map(|category| category.children)
            .map(|child| child.section.element_key())
            .collect::<Vec<_>>();
        let unique = keys.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), keys.len(), "{keys:?}");
    }
}
