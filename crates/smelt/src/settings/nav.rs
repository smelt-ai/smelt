//! 设置窗口的信息架构：左侧默认展开的两级导航 + 功能入口对应的独立内容页。
//!
//! gpui-component 的 `Settings` 在 `groups.len() > 1` 时会画二级菜单，但点击只是
//! 滚动到分组，右侧仍是整页。这里把每个入口做成独立 `SettingsSection`，由壳层
//! 只渲染当前这一页。分类是默认展开的一级节点，所有二级入口初始都直接可见。

use gpui_component::{Icon, IconName};

/// 设置窗口当前选中的内容页。深链接（检查更新、外观、机器、资源）也走这里。
///
/// 原先按子功能拆开的叶子页收成 7 个高内聚页。外观深链接走 [`Self::appearance`]；
/// macOS 上的检查更新深链接走 `maintenance_update`，落到「系统更新与维护」。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SettingsSection {
    /// 主题、界面字号、终端字体、窗口与背景。
    #[default]
    Appearance,
    /// 原「通知」。
    Notifications,
    Shortcuts,
    /// Agent 运行环境、新建与启动、对话与工作区。
    AgentEnvironment,
    /// 面板插件和 Agent Hooks。
    PluginsAndHooks,
    /// 已连接的远程机器，以及手机 iroh 远程访问。
    Remote,
    /// 资源占用、应用更新（仅 macOS 展示该分组）、后台服务、存储清理。
    /// 停在这一页时才采样资源。
    Maintenance,
}

impl SettingsSection {
    pub fn appearance() -> Self {
        Self::Appearance
    }

    /// 检查更新深链接。更新分组只在 macOS 上画出来，页面本身各平台都有。
    #[cfg(target_os = "macos")]
    pub fn maintenance_update() -> Self {
        Self::Maintenance
    }

    pub fn element_key(&self) -> gpui::SharedString {
        match self {
            Self::Appearance => "appearance".into(),
            Self::Notifications => "notifications".into(),
            Self::Shortcuts => "shortcuts".into(),
            Self::AgentEnvironment => "agent-environment".into(),
            Self::PluginsAndHooks => "plugins-hooks".into(),
            Self::Remote => "remote".into(),
            Self::Maintenance => "maintenance".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsCategoryId {
    General,
    AgentAndSystem,
}

impl SettingsCategoryId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::AgentAndSystem => "agent-and-system",
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
    pub id: SettingsCategoryId,
    pub title: &'static str,
    pub keywords: &'static [&'static str],
    pub default_open: bool,
    pub children: Vec<SettingsNavLeaf>,
}

/// 设置侧栏的两级导航。一级分类默认展开。
pub fn settings_nav() -> Vec<SettingsNavCategory> {
    vec![
        SettingsNavCategory {
            id: SettingsCategoryId::General,
            title: "通用与交互",
            keywords: &["通用", "常规", "偏好设置", "交互"],
            default_open: true,
            children: vec![
                leaf(
                    "外观、主题与终端",
                    IconName::Palette,
                    SettingsSection::Appearance,
                    &[
                        "外观",
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
                        "输出",
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
                    "通知与提醒",
                    IconName::Bell,
                    SettingsSection::Notifications,
                    &[
                        "通知",
                        "通知与提醒",
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
                    "键盘快捷键",
                    IconName::Asterisk,
                    SettingsSection::Shortcuts,
                    &[
                        "快捷键",
                        "键盘",
                        "按键",
                        "组合键",
                        "shortcut",
                        "keymap",
                        "cmd",
                        "command",
                    ],
                ),
            ],
        },
        SettingsNavCategory {
            id: SettingsCategoryId::AgentAndSystem,
            title: "智能体与系统",
            keywords: &["智能体与系统"],
            default_open: true,
            children: vec![
                leaf(
                    "模型与智能体环境",
                    IconName::Bot,
                    SettingsSection::AgentEnvironment,
                    &[
                        "Agent",
                        "智能体",
                        "AI",
                        "会话",
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
                        "快捷启动项",
                        "启动",
                        "launch",
                        "命令",
                        "+",
                        "加号",
                        "新建菜单",
                        "项目菜单",
                        "终端",
                        "Agent 对话",
                        "adapter",
                        "适配器",
                        "workspace",
                        "工作区",
                        "启动命令",
                        "参数",
                        "预设",
                        "prompt",
                        "persona",
                        "跨 agent",
                        "跨agent",
                        "mcp",
                        "通讯",
                        "通信",
                        "消息",
                    ],
                ),
                leaf(
                    "插件与 Hooks",
                    Icon::new(gpui_kit_assets::IconName::Puzzle),
                    SettingsSection::PluginsAndHooks,
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
                        "hook",
                        "hooks",
                        "Agent Hooks",
                        "通知接入",
                        "权限接入",
                        "配置文件",
                    ],
                ),
                leaf(
                    "远程连接与配对",
                    IconName::Globe,
                    SettingsSection::Remote,
                    &[
                        "多机器",
                        "远程机器",
                        "添加机器",
                        "添加远程机器",
                        "运行时",
                        "其他运行时",
                        "配对串",
                        "smeltd pair",
                        "离线",
                        "版本不兼容",
                        "远程访问",
                        "开启远程",
                        "Relay 地址",
                        "允许远程写入",
                        "远程",
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
                        "项目",
                    ],
                ),
                leaf(
                    "系统更新与维护",
                    IconName::Cpu,
                    SettingsSection::Maintenance,
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
                        "残留数据清理",
                        "清理",
                        "残留",
                        "存储",
                        "空间",
                        "缓存",
                        "清缓存",
                        "磁盘",
                        "旧数据",
                        "历史",
                        "~/.smelt",
                        "维护",
                        "高级",
                        "系统",
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

    fn child_titles(nav: &[SettingsNavCategory], id: SettingsCategoryId) -> Vec<String> {
        nav.iter()
            .find(|category| category.id == id)
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
    fn nav_exposes_two_groups_and_seven_pages() {
        let nav = settings_nav();
        assert_eq!(titles(&nav), ["通用与交互", "智能体与系统"]);
        assert_eq!(
            child_titles(&nav, SettingsCategoryId::General),
            ["外观、主题与终端", "通知与提醒", "键盘快捷键"]
        );
        assert_eq!(
            child_titles(&nav, SettingsCategoryId::AgentAndSystem),
            [
                "模型与智能体环境",
                "插件与 Hooks",
                "远程连接与配对",
                "系统更新与维护"
            ]
        );
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
                SettingsSection::AgentEnvironment,
                SettingsSection::PluginsAndHooks,
                SettingsSection::Remote,
                SettingsSection::Maintenance,
            ]
        );
    }

    #[test]
    fn nav_categories_are_default_open_two_level_branches() {
        let nav = settings_nav();
        assert!(
            nav.iter().all(|category| !category.children.is_empty()),
            "一级分类下面必须有二级页面，不能混入同级叶子"
        );
        assert!(
            nav.iter().all(|category| category.default_open),
            "所有一级分类都应默认展开，让二级页面直接可见"
        );
    }

    #[test]
    fn search_filters_visible_entries_without_changing_page_structure() {
        let nav = settings_nav();
        let filtered = filter_settings_nav(&nav, "守护");
        assert_ne!(
            filtered[0].id, nav[0].id,
            "搜索会让分类换位，折叠状态必须按 SettingsCategoryId 记账，不能按下标"
        );
        assert_eq!(filtered[0].id, SettingsCategoryId::AgentAndSystem);
        assert_eq!(titles(&filtered), ["智能体与系统"]);
        assert_eq!(
            child_titles(&filtered, SettingsCategoryId::AgentAndSystem),
            ["系统更新与维护"]
        );

        let by_category = filter_settings_nav(&nav, "通用");
        assert_eq!(
            child_titles(&by_category, SettingsCategoryId::General).len(),
            3
        );
    }

    #[test]
    fn search_understands_user_tasks_and_multiple_terms() {
        let nav = settings_nav();

        assert_eq!(
            child_titles(
                &filter_settings_nav(&nav, "模型"),
                SettingsCategoryId::AgentAndSystem,
            ),
            ["模型与智能体环境"],
            "分类标题里的一个宽泛词不能把同组其它页都留下"
        );
        assert_eq!(
            child_titles(
                &filter_settings_nav(&nav, "agent 通知"),
                SettingsCategoryId::General,
            ),
            ["通知与提醒"]
        );
        assert!(
            child_titles(
                &filter_settings_nav(&nav, "agent 通知"),
                SettingsCategoryId::AgentAndSystem,
            )
            .contains(&"插件与 Hooks".to_string()),
            "「通知接入」仍应命中插件与 Hooks"
        );
        assert_eq!(
            matching_sections(&nav, "agent通知"),
            [SettingsSection::Notifications]
        );
        assert_eq!(
            matching_sections(&nav, "后台服务"),
            [SettingsSection::Maintenance]
        );
        assert_eq!(
            matching_sections(&nav, "清缓存"),
            [SettingsSection::Maintenance]
        );
        assert_eq!(
            matching_sections(&nav, "预设 prompt"),
            [SettingsSection::AgentEnvironment]
        );

        for (query, expected) in [
            ("任务完成通知", SettingsSection::Notifications),
            ("Relay 地址", SettingsSection::Remote),
            ("添加机器", SettingsSection::Remote),
            ("内存占用", SettingsSection::Maintenance),
            ("导入插件", SettingsSection::PluginsAndHooks),
            ("自动下载安装", SettingsSection::Maintenance),
            ("终端字号", SettingsSection::Appearance),
            ("毛玻璃", SettingsSection::Appearance),
            ("背景图", SettingsSection::Appearance),
            ("守护", SettingsSection::Maintenance),
            ("hook", SettingsSection::PluginsAndHooks),
            ("快捷启动项", SettingsSection::AgentEnvironment),
        ] {
            assert_eq!(matching_sections(&nav, query), [expected], "query: {query}");
        }
    }

    #[test]
    fn every_destination_is_a_second_level_leaf() {
        let nav = settings_nav();
        assert!(nav.iter().all(|category| !category.children.is_empty()));
        let visible_destinations = nav
            .iter()
            .flat_map(|category| category.children.iter())
            .map(|child| child.section.clone())
            .collect::<Vec<_>>();
        assert_eq!(visible_destinations.len(), 7);
        assert!(visible_destinations.contains(&SettingsSection::Shortcuts));
        assert!(visible_destinations.contains(&SettingsSection::Maintenance));
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

    #[test]
    fn deep_links_point_at_merged_pages() {
        assert_eq!(SettingsSection::appearance(), SettingsSection::Appearance);
        #[cfg(target_os = "macos")]
        assert_eq!(
            SettingsSection::maintenance_update(),
            SettingsSection::Maintenance
        );
    }
}
