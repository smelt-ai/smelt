//! 设置窗口的信息架构：左侧默认展开的两级导航 + 功能入口对应的独立内容页。
//!
//! gpui-component 的 `Settings` 在 `groups.len() > 1` 时会画二级菜单，但点击只是
//! 滚动到分组，右侧仍是整页。这里把每个入口做成独立 `SettingsSection`，由壳层
//! 只渲染当前这一页。分类是默认展开的一级节点，所有二级入口初始都直接可见。

use super::plugins::InstalledPlugin;
use gpui_component::IconName;

/// 设置窗口当前选中的内容页。深链接（检查更新、登录）也走这里，不再用页下标。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SettingsSection {
    #[default]
    AppearanceTheme,
    AppearanceTerminal,
    AppearanceWindow,
    DshModel,
    PiModel,
    PiPlugins,
    AgentRuntime,
    AgentLaunch,
    AgentWorkspace,
    AgentNotify,
    AgentHooks,
    Worktree,
    CollaborationRemote,
    /// `id` 为空表示插件清单还没就绪或尚未安装任何插件。
    Plugin {
        id: String,
    },
    MaintenanceUpdate,
    MaintenanceDaemon,
    MaintenanceStorage,
    Shortcuts,
}

impl SettingsSection {
    pub fn appearance() -> Self {
        Self::AppearanceTheme
    }

    pub fn maintenance_update() -> Self {
        Self::MaintenanceUpdate
    }

    pub fn element_key(&self) -> gpui::SharedString {
        match self {
            Self::AppearanceTheme => "appearance-theme".into(),
            Self::AppearanceTerminal => "appearance-terminal".into(),
            Self::AppearanceWindow => "appearance-window".into(),
            Self::DshModel => "dsh-model".into(),
            Self::PiModel => "pi-model".into(),
            Self::PiPlugins => "pi-plugins".into(),
            Self::AgentRuntime => "agent-runtime".into(),
            Self::AgentLaunch => "agent-launch".into(),
            Self::AgentWorkspace => "agent-workspace".into(),
            Self::AgentNotify => "agent-notify".into(),
            Self::AgentHooks => "agent-hooks".into(),
            Self::Worktree => "worktree".into(),
            Self::CollaborationRemote => "collaboration-remote".into(),
            Self::Plugin { id } => format!("plugin:{id}").into(),
            Self::MaintenanceUpdate => "maintenance-update".into(),
            Self::MaintenanceDaemon => "maintenance-daemon".into(),
            Self::MaintenanceStorage => "maintenance-storage".into(),
            Self::Shortcuts => "shortcuts".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsCategoryId {
    General,
    Agent,
    Workspace,
    Plugins,
    System,
}

impl SettingsCategoryId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Agent => "agent",
            Self::Workspace => "workspace",
            Self::Plugins => "plugins",
            Self::System => "system",
        }
    }
}

#[derive(Clone)]
pub struct SettingsNavLeaf {
    pub title: String,
    pub icon: IconName,
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

/// 设置窗口的作用域。同一套壳可以开出多扇内容不同的窗口，各自单例。
///
/// 智能体的模型与插件是「配一次就去用」的东西，塞进通用设置里要翻三层；这里让它
/// 从智能体页头直接开一扇只有这两页的窗口，而不是在设置里再放一份入口。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SettingsScope {
    /// 通用设置窗口。
    #[default]
    All,
    /// 智能体设置窗口：只有 Pi 模型与智能体插件。
    AgentSetup,
}

impl SettingsScope {
    pub fn window_title(self) -> &'static str {
        match self {
            Self::All => "设置",
            Self::AgentSetup => "智能体设置",
        }
    }

    /// 目标 section 不属于本窗口时的兜底页。
    pub fn default_section(self) -> SettingsSection {
        match self {
            Self::All => SettingsSection::AppearanceTheme,
            Self::AgentSetup => SettingsSection::PiModel,
        }
    }

    /// 本窗口该显示哪些页。
    pub fn nav(self, plugins: &[InstalledPlugin]) -> Vec<SettingsNavCategory> {
        match self {
            Self::All => settings_nav(plugins),
            Self::AgentSetup => agent_setup_nav(),
        }
    }

    /// section 是否属于本窗口。不属于就该回退，否则会画出侧栏里根本没有的页。
    pub fn contains(self, section: &SettingsSection) -> bool {
        nav_contains(&self.nav(&[]), section)
    }
}

/// 已构造好的侧栏里有没有这一页。渲染时用它，省掉为判断归属再建一棵树。
pub fn nav_contains(nav: &[SettingsNavCategory], section: &SettingsSection) -> bool {
    nav.iter()
        .flat_map(|category| category.children.iter())
        .any(|child| &child.section == section)
}

/// 智能体设置窗口的侧栏：只放「配好就能开跑」的两页。
pub fn agent_setup_nav() -> Vec<SettingsNavCategory> {
    vec![SettingsNavCategory {
        id: SettingsCategoryId::Agent,
        title: "智能体",
        keywords: &["Agent", "智能体", "AI"],
        default_open: true,
        children: vec![
            leaf(
                "模型",
                IconName::Bot,
                SettingsSection::PiModel,
                &[
                    "Pi",
                    "pi",
                    "智能体",
                    "模型",
                    "model",
                    "凭据",
                    "API",
                    "API key",
                    "API 密钥",
                    "API 地址",
                    "密钥",
                    "token",
                    "provider",
                    "供应商",
                    "网关",
                    "endpoint",
                    "base url",
                ],
            ),
            leaf(
                "插件",
                IconName::LayoutDashboard,
                SettingsSection::PiPlugins,
                &[
                    "插件",
                    "plugin",
                    "技能",
                    "skill",
                    "扩展",
                    "extension",
                    "能力",
                    "工具",
                    "Pi",
                    "pi",
                    "智能体",
                ],
            ),
        ],
    }]
}

/// 设置侧栏的两级导航。一级分类默认展开；插件子项来自当前已安装清单。
pub fn settings_nav(plugins: &[InstalledPlugin]) -> Vec<SettingsNavCategory> {
    vec![
        SettingsNavCategory {
            id: SettingsCategoryId::General,
            title: "通用",
            keywords: &["常规", "偏好设置"],
            default_open: true,
            children: vec![
                leaf(
                    "主题与界面",
                    IconName::Palette,
                    SettingsSection::AppearanceTheme,
                    &[
                        "外观",
                        "主题",
                        "颜色模式",
                        "深色",
                        "暗色",
                        "夜间",
                        "浅色",
                        "亮色",
                        "界面字号",
                        "UI 字号",
                        "界面字体",
                        "字体大小",
                        "调整字号",
                        "缩放",
                    ],
                ),
                leaf(
                    "终端",
                    IconName::SquareTerminal,
                    SettingsSection::AppearanceTerminal,
                    &[
                        "外观",
                        "terminal",
                        "终端字号",
                        "代码字体",
                        "等宽字体",
                        "字体",
                        "字体大小",
                        "调整字号",
                        "输出",
                    ],
                ),
                leaf(
                    "窗口与背景",
                    IconName::GalleryVerticalEnd,
                    SettingsSection::AppearanceWindow,
                    &[
                        "外观",
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
            id: SettingsCategoryId::Agent,
            title: "模型与 Agent",
            keywords: &["Agent", "智能体", "AI"],
            default_open: true,
            children: vec![
                leaf(
                    "DeepSeek Harness",
                    IconName::Cpu,
                    SettingsSection::DshModel,
                    &[
                        "DeepSeek",
                        "DSH",
                        "Harness",
                        "模型",
                        "model",
                        "凭据",
                        "API",
                        "API key",
                        "API 密钥",
                        "API 地址",
                        "密钥",
                        "token",
                        "provider",
                        "供应商",
                        "网关",
                        "endpoint",
                        "base url",
                        "插件",
                    ],
                ),
                leaf(
                    "Agent 运行环境",
                    IconName::Bot,
                    SettingsSection::AgentRuntime,
                    &[
                        "Agent", "会话", "cli", "安装", "版本", "检测", "路径", "claude", "codex",
                        "copilot", "grok", "cursor", "opencode", "kiro", "pi",
                    ],
                ),
                leaf(
                    "新建与启动",
                    IconName::Play,
                    SettingsSection::AgentLaunch,
                    &[
                        "快捷启动项",
                        "启动",
                        "launch",
                        "命令",
                        "+",
                        "加号",
                        "新建菜单",
                        "项目菜单",
                        "终端",
                    ],
                ),
                leaf(
                    "对话与工作区",
                    IconName::Settings2,
                    SettingsSection::AgentWorkspace,
                    &[
                        "Agent 对话",
                        "会话",
                        "adapter",
                        "适配器",
                        "workspace",
                        "工作区",
                        "启动命令",
                        "参数",
                        "智能体",
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
                    "通知",
                    IconName::Bell,
                    SettingsSection::AgentNotify,
                    &[
                        "Agent",
                        "Agent通知",
                        "提醒",
                        "notification",
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
                    "Agent Hooks",
                    IconName::Network,
                    SettingsSection::AgentHooks,
                    &["hook", "hooks", "通知接入", "权限接入", "配置文件"],
                ),
            ],
        },
        SettingsNavCategory {
            id: SettingsCategoryId::Workspace,
            title: "工作区与远程",
            keywords: &["项目"],
            default_open: true,
            children: vec![
                leaf(
                    "Worktree 文件继承",
                    IconName::FolderOpen,
                    SettingsSection::Worktree,
                    &[
                        "worktree",
                        "工作区",
                        "文件继承",
                        "继承主仓库未跟踪文件",
                        "继承",
                        "未跟踪",
                        "gitignore",
                        ".env",
                        "软链",
                        "跳过",
                        "排除",
                    ],
                ),
                leaf(
                    "手机远程",
                    IconName::Globe,
                    SettingsSection::CollaborationRemote,
                    &[
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
                    ],
                ),
            ],
        },
        plugin_category(plugins),
        SettingsNavCategory {
            id: SettingsCategoryId::System,
            title: "系统",
            keywords: &["维护", "高级"],
            default_open: true,
            children: vec![
                leaf(
                    "应用更新",
                    IconName::Redo,
                    SettingsSection::MaintenanceUpdate,
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
                    ],
                ),
                leaf(
                    "后台服务",
                    IconName::Cpu,
                    SettingsSection::MaintenanceDaemon,
                    &[
                        "smeltd",
                        "守护进程",
                        "守护",
                        "后台",
                        "服务",
                        "进程",
                        "重启",
                        "重启守护进程",
                        "无缝升级",
                        "会话服务",
                        "卡死",
                    ],
                ),
                leaf(
                    "存储清理",
                    IconName::HardDrive,
                    SettingsSection::MaintenanceStorage,
                    &[
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
                    ],
                ),
            ],
        },
    ]
}

fn plugin_category(plugins: &[InstalledPlugin]) -> SettingsNavCategory {
    let mut children = vec![SettingsNavLeaf {
        title: "安装插件".to_string(),
        icon: IconName::Plus,
        section: SettingsSection::Plugin { id: String::new() },
        keywords: vec![
            "安装".to_string(),
            "插件".to_string(),
            "install".to_string(),
        ],
    }];
    children.extend(plugins.iter().map(|plugin| SettingsNavLeaf {
        title: plugin.name.clone(),
        icon: IconName::Frame,
        section: SettingsSection::Plugin {
            id: plugin.id.clone(),
        },
        keywords: vec![
            plugin.name.clone(),
            plugin.id.clone(),
            "插件".to_string(),
            "plugin".to_string(),
        ],
    }));
    SettingsNavCategory {
        id: SettingsCategoryId::Plugins,
        title: "插件",
        keywords: &["扩展", "plugin"],
        default_open: true,
        children,
    }
}

fn leaf(
    title: &str,
    icon: IconName,
    section: SettingsSection,
    keywords: &[&str],
) -> SettingsNavLeaf {
    SettingsNavLeaf {
        title: title.to_string(),
        icon,
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
    fn nav_exposes_task_oriented_groups_as_independent_sections() {
        let nav = settings_nav(&[]);
        assert_eq!(
            titles(&nav),
            ["通用", "模型与 Agent", "工作区与远程", "插件", "系统"]
        );
        assert_eq!(
            child_titles(&nav, SettingsCategoryId::General),
            ["主题与界面", "终端", "窗口与背景", "键盘快捷键"]
        );
        assert_eq!(
            child_titles(&nav, SettingsCategoryId::Agent),
            [
                "DeepSeek Harness",
                "Agent 运行环境",
                "新建与启动",
                "对话与工作区",
                "通知",
                "Agent Hooks"
            ]
        );
        assert_eq!(
            child_titles(&nav, SettingsCategoryId::Workspace),
            ["Worktree 文件继承", "手机远程"]
        );
        assert_eq!(
            child_titles(&nav, SettingsCategoryId::System),
            ["应用更新", "后台服务", "存储清理"]
        );

        let general = nav
            .iter()
            .find(|category| category.id == SettingsCategoryId::General)
            .unwrap();
        let sections = general
            .children
            .iter()
            .map(|child| child.section.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            sections,
            [
                SettingsSection::AppearanceTheme,
                SettingsSection::AppearanceTerminal,
                SettingsSection::AppearanceWindow,
                SettingsSection::Shortcuts,
            ]
        );
        assert_ne!(
            sections[0], sections[1],
            "子菜单必须指向不同内容页，不能共用一个滚动页"
        );
    }

    #[test]
    fn nav_categories_are_default_open_two_level_branches() {
        for nav in [settings_nav(&[]), agent_setup_nav()] {
            assert!(
                nav.iter().all(|category| !category.children.is_empty()),
                "一级分类下面必须有二级页面，不能混入同级叶子"
            );
            assert!(
                nav.iter().all(|category| category.default_open),
                "所有一级分类都应默认展开，让二级页面直接可见"
            );
        }
    }

    #[test]
    fn plugin_children_come_from_catalog() {
        let empty = settings_nav(&[]);
        let plugins = empty
            .iter()
            .find(|category| category.id == SettingsCategoryId::Plugins)
            .unwrap();
        assert_eq!(plugins.children[0].title, "安装插件");

        let catalog = vec![
            InstalledPlugin {
                id: "com.example.account".into(),
                name: "Account".into(),
                version: "1.0.0".into(),
                user_installed: false,
                load_error: None,
            },
            InstalledPlugin {
                id: "com.example.other".into(),
                name: "Other".into(),
                version: "0.1.0".into(),
                user_installed: true,
                load_error: None,
            },
        ];
        let nav = settings_nav(&catalog);
        assert_eq!(
            child_titles(&nav, SettingsCategoryId::Plugins),
            ["安装插件", "Account", "Other"]
        );
        let plugins = nav
            .iter()
            .find(|category| category.id == SettingsCategoryId::Plugins)
            .unwrap();
        assert_eq!(
            plugins.children[1].section,
            SettingsSection::Plugin {
                id: "com.example.account".into()
            }
        );
    }

    #[test]
    fn search_filters_visible_entries_without_changing_page_structure() {
        let nav = settings_nav(&[]);
        let filtered = filter_settings_nav(&nav, "守护");
        assert_ne!(
            filtered[0].id, nav[0].id,
            "搜索会让分类换位，折叠状态必须按 SettingsCategoryId 记账，不能按下标"
        );
        assert_eq!(filtered[0].id, SettingsCategoryId::System);
        assert_eq!(titles(&filtered), ["系统"]);
        assert_eq!(
            child_titles(&filtered, SettingsCategoryId::System),
            ["后台服务"]
        );

        let by_category = filter_settings_nav(&nav, "通用");
        assert_eq!(
            child_titles(&by_category, SettingsCategoryId::General).len(),
            4
        );
    }

    #[test]
    fn search_understands_user_tasks_and_multiple_terms() {
        let nav = settings_nav(&[]);

        assert_eq!(
            child_titles(
                &filter_settings_nav(&nav, "模型"),
                SettingsCategoryId::Agent,
            ),
            ["DeepSeek Harness"],
            "分类标题里的一个宽泛词不能把整组结果都留下"
        );
        assert_eq!(
            titles(&filter_settings_nav(&nav, "API key")),
            ["模型与 Agent"]
        );
        assert_eq!(
            child_titles(
                &filter_settings_nav(&nav, "API key"),
                SettingsCategoryId::Agent,
            ),
            ["DeepSeek Harness"]
        );
        assert_eq!(
            child_titles(
                &filter_settings_nav(&nav, "agent 通知"),
                SettingsCategoryId::Agent,
            ),
            ["通知", "Agent Hooks"]
        );
        assert_eq!(
            child_titles(
                &filter_settings_nav(&nav, "agent通知"),
                SettingsCategoryId::Agent,
            ),
            ["通知"]
        );
        assert_eq!(
            child_titles(
                &filter_settings_nav(&nav, "后台服务"),
                SettingsCategoryId::System,
            ),
            ["后台服务"]
        );
        assert_eq!(
            child_titles(
                &filter_settings_nav(&nav, "清缓存"),
                SettingsCategoryId::System,
            ),
            ["存储清理"]
        );
        assert_eq!(
            child_titles(
                &filter_settings_nav(&nav, "预设 prompt"),
                SettingsCategoryId::Agent,
            ),
            ["对话与工作区"]
        );

        for (query, expected) in [
            ("任务完成通知", SettingsSection::AgentNotify),
            ("继承主仓库未跟踪文件", SettingsSection::Worktree),
            ("Relay 地址", SettingsSection::CollaborationRemote),
            ("自动下载安装", SettingsSection::MaintenanceUpdate),
        ] {
            assert_eq!(matching_sections(&nav, query), [expected], "query: {query}");
        }
    }

    #[test]
    fn agent_scope_owns_the_pi_pages_and_the_main_window_does_not_show_them_twice() {
        let agent_titles = child_titles(&agent_setup_nav(), SettingsCategoryId::Agent);
        assert_eq!(agent_titles, ["模型", "插件"]);

        // 两页只在智能体窗口出现：留在通用设置里就成了两处入口、两处维护。
        let main_sections = settings_nav(&[])
            .iter()
            .flat_map(|category| category.children.iter())
            .map(|child| child.section.clone())
            .collect::<Vec<_>>();
        assert!(!main_sections.contains(&SettingsSection::PiModel));
        assert!(!main_sections.contains(&SettingsSection::PiPlugins));

        assert!(SettingsScope::AgentSetup.contains(&SettingsSection::PiModel));
        assert!(SettingsScope::AgentSetup.contains(&SettingsSection::PiPlugins));
        assert!(!SettingsScope::AgentSetup.contains(&SettingsSection::AppearanceTheme));
        assert!(SettingsScope::All.contains(&SettingsSection::AppearanceTheme));
        assert!(!SettingsScope::All.contains(&SettingsSection::PiModel));

        // 兜底页必须是本窗口自己有的页，否则回退会画出侧栏里没有的内容。
        for scope in [SettingsScope::All, SettingsScope::AgentSetup] {
            assert!(scope.contains(&scope.default_section()), "{scope:?}");
        }
    }

    #[test]
    fn every_destination_is_a_second_level_leaf() {
        let nav = settings_nav(&[]);
        assert!(nav.iter().all(|category| !category.children.is_empty()));
        let visible_destinations = nav
            .iter()
            .flat_map(|category| category.children.iter())
            .map(|child| child.section.clone())
            .collect::<Vec<_>>();

        assert_eq!(visible_destinations.len(), 16);
        assert!(visible_destinations.contains(&SettingsSection::DshModel));
        assert!(visible_destinations.contains(&SettingsSection::Shortcuts));
        assert!(visible_destinations.contains(&SettingsSection::Worktree));
    }

    #[test]
    fn deep_links_point_at_content_sections() {
        assert_eq!(
            SettingsSection::appearance(),
            SettingsSection::AppearanceTheme
        );
        assert_eq!(
            SettingsSection::maintenance_update(),
            SettingsSection::MaintenanceUpdate
        );
    }
}
