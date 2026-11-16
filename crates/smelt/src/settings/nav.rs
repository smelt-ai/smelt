//! 设置的信息架构：左侧按分组列出各页，每页一个独立内容页。
//!
//! gpui-component 的 `Settings` 在 `groups.len() > 1` 时会画二级菜单，但点击只是
//! 滚动到分组，右侧仍是整页。这里把每一页做成独立 `SettingsSection`，由壳层只渲染
//! 当前这一页；页内再用 `SettingGroup` 分节。

use gpui::SharedString;
use gpui_component::{Icon, IconName};
use smelt_plugin_api::{SettingsGroup, ViewInfo};

/// 设置当前选中的内容页。深链接（外观、机器、资源、插件的设置页）也走这里。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SettingsSection {
    /// 主题、字号与字体、布局、背景。
    #[default]
    Appearance,
    Shortcuts,
    PanelPlugins,
    /// 本机 Agent CLI、Hooks 接入、启动项、对话命令与多工作区。都只作用于本机运行时。
    Agents,
    /// 连到其他机器的运行时，以及让配对过的设备连进这台机器。
    Remote,
    /// 插件登记的页（带 `settings` 的视图，见 `smelt_plugin_api::register_views`），带视图 id。
    Plugin(SharedString),
}

impl SettingsSection {
    pub fn appearance() -> Self {
        Self::Appearance
    }

    pub fn element_key(&self) -> SharedString {
        match self {
            Self::Appearance => "appearance".into(),
            Self::Shortcuts => "shortcuts".into(),
            Self::PanelPlugins => "panel-plugins".into(),
            Self::Agents => "agents".into(),
            Self::Remote => "remote".into(),
            Self::Plugin(id) => format!("plugin:{id}").into(),
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

/// 设置侧栏：宿主的三组，每组一行小标题加若干页；插件的页按它挑的组排在宿主的页后面，
/// 挑「插件」组的单独成一组放在最后。`views` 是登记过的全部视图，只取设置页。
pub fn settings_nav(views: &[ViewInfo]) -> Vec<SettingsNavCategory> {
    let mut nav = host_nav();
    let mut plugins = SettingsNavCategory {
        title: "插件",
        keywords: &["扩展", "plugin"],
        children: Vec::new(),
    };
    for view in views {
        let Some(page) = &view.settings else {
            continue;
        };
        let leaf = SettingsNavLeaf {
            title: view.title.to_string(),
            icon: Icon::empty().path(view.icon.clone()),
            section: SettingsSection::Plugin(view.id.clone()),
            keywords: page.keywords.iter().map(ToString::to_string).collect(),
        };
        let category = match page.group {
            SettingsGroup::General => &mut nav[0],
            SettingsGroup::Agents => &mut nav[1],
            SettingsGroup::System => &mut nav[2],
            SettingsGroup::Plugins => &mut plugins,
        };
        category.children.push(leaf);
    }
    nav.push(plugins);
    // 「系统」组宿主自己没有页，全靠插件；一页都没有的组不露。
    nav.retain(|category| !category.children.is_empty());
    nav
}

/// 宿主自己的页：通用、Agent 与远程、系统三组，次序和 [`settings_nav`] 里按组放插件页对应。
/// 「系统」组的页都是插件的：「关于」在「更新与帮助」（smelt-updater-ui），终端里的 smelt
/// 命令在「维护」（smelt-maintenance）。
fn host_nav() -> Vec<SettingsNavCategory> {
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
                        "背景",
                        "背景图",
                        "背景图片",
                        "壁纸",
                        "透明",
                        "透明度",
                        "背景透明",
                        "材质",
                        "毛玻璃",
                        "模糊",
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
                        "面板",
                        "布局",
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
                        "额度",
                        "钥匙串",
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
            children: Vec::new(),
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
        let nav = settings_nav(&[]);
        assert_eq!(titles(&nav), ["通用", "Agent 与远程"]);
        assert_eq!(child_titles(&nav, "通用"), ["外观", "快捷键", "面板插件"]);
        assert_eq!(child_titles(&nav, "Agent 与远程"), ["Agent", "远程"]);
        let sections = nav
            .iter()
            .flat_map(|category| category.children.iter())
            .map(|child| child.section.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            sections,
            [
                SettingsSection::Appearance,
                SettingsSection::Shortcuts,
                SettingsSection::PanelPlugins,
                SettingsSection::Agents,
                SettingsSection::Remote,
            ]
        );
    }

    #[test]
    fn search_keeps_only_matching_pages_under_their_group() {
        let nav = settings_nav(&[]);
        let filtered = filter_settings_nav(&nav, "relay");
        assert_eq!(titles(&filtered), ["Agent 与远程"]);
        assert_eq!(child_titles(&filtered, "Agent 与远程"), ["远程"]);

        let by_group = filter_settings_nav(&nav, "通用");
        assert_eq!(child_titles(&by_group, "通用").len(), 3);
    }

    #[test]
    fn search_understands_user_tasks_and_multiple_terms() {
        let nav = settings_nav(&[]);
        for (query, expected) in [
            ("Relay 地址", SettingsSection::Remote),
            ("添加机器", SettingsSection::Remote),
            ("导入插件", SettingsSection::PanelPlugins),
            ("终端字号", SettingsSection::Appearance),
            ("壁纸", SettingsSection::Appearance),
            ("背景图", SettingsSection::Appearance),
            ("hook", SettingsSection::Agents),
            ("快捷启动项", SettingsSection::Agents),
            ("config dir", SettingsSection::Agents),
        ] {
            assert_eq!(matching_sections(&nav, query), [expected], "query: {query}");
        }
    }

    fn view(id: &str, title: &str, settings: Option<smelt_plugin_api::SettingsEntry>) -> ViewInfo {
        ViewInfo {
            id: id.to_string().into(),
            title: title.to_string().into(),
            icon: "icons/bell.svg".into(),
            draws_top_bar: false,
            instances: false,
            settings,
            build: std::rc::Rc::new(|_, _| unreachable!("侧栏不建页")),
        }
    }

    fn plugin_page(id: &str, title: &str, group: SettingsGroup, keywords: &[&str]) -> ViewInfo {
        let keywords = keywords
            .iter()
            .map(|word| SharedString::from(word.to_string()))
            .collect();
        view(
            id,
            title,
            Some(smelt_plugin_api::SettingsEntry { group, keywords }),
        )
    }

    #[test]
    fn plugin_pages_go_after_the_host_pages_of_their_group() {
        let pages = [
            plugin_page("notify/main", "提醒", SettingsGroup::General, &["系统通知"]),
            plugin_page("trader/prefs", "行情", SettingsGroup::Plugins, &["股票"]),
            // 不是设置页的视图不进设置。
            view("trader/main", "交易", None),
            plugin_page("probe/disk", "磁盘", SettingsGroup::System, &[]),
        ];
        let nav = settings_nav(&pages);
        assert_eq!(titles(&nav), ["通用", "Agent 与远程", "系统", "插件"]);
        assert_eq!(
            child_titles(&nav, "通用"),
            ["外观", "快捷键", "面板插件", "提醒"]
        );
        assert_eq!(child_titles(&nav, "系统"), ["磁盘"]);
        assert_eq!(child_titles(&nav, "插件"), ["行情"]);
        // 一页都没有的组不露：没有挑「插件」组、「系统」组的页就没有这两组。
        assert_eq!(titles(&settings_nav(&pages[..1])), ["通用", "Agent 与远程"]);

        assert_eq!(
            matching_sections(&nav, "股票"),
            [SettingsSection::Plugin("trader/prefs".into())]
        );
    }

    #[test]
    fn element_keys_are_unique() {
        let keys = settings_nav(&[])
            .into_iter()
            .flat_map(|category| category.children)
            .map(|child| child.section.element_key())
            .collect::<Vec<_>>();
        let unique = keys.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), keys.len(), "{keys:?}");
    }
}
