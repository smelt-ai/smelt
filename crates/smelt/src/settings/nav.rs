//! 设置的信息架构：左侧按分组列出各页，每页一个独立内容页。
//!
//! 每一页是一个带 `settings` 的视图（宿主自己的外观、快捷键、面板插件也是，见
//! [`super::view_specs`]），导航只按登记的视图列，不认识具体哪一页。页内用 smelt-ui 的
//! `SettingsSection`（分组卡片）分节。

use gpui::SharedString;
use rust_i18n::t;
use smelt_plugin_api::{SettingsGroup, ViewInfo};

#[derive(Clone)]
pub struct SettingsNavLeaf {
    pub title: String,
    /// 图标的 svg 路径，登记视图时给的。
    pub icon: SharedString,
    /// 这一页的视图 id。
    pub id: SharedString,
    pub keywords: Vec<String>,
}

#[derive(Clone)]
pub struct SettingsNavCategory {
    /// 此刻语言下的组名。导航在视图登记变了、换了语言时重建。
    pub title: SharedString,
    pub keywords: &'static [&'static str],
    pub children: Vec<SettingsNavLeaf>,
}

/// 设置侧栏：四组（通用、Agent 与远程、系统、插件），每组一行小标题加若干页。组内宿主自己
/// 的页在前，插件的页按登记先后排在后面；一页都没有的组不露。`views` 是登记过的全部视图，
/// 只取设置页。
pub fn settings_nav(views: &[ViewInfo]) -> Vec<SettingsNavCategory> {
    let mut nav = vec![
        // 关键词两种语言都写上（含另一种语言的组名），用哪种语言搜都找得到。
        category(
            t!("settings.group.general"),
            &["通用", "常规", "偏好设置", "general", "preferences"],
        ),
        category(
            t!("settings.group.agents"),
            &["Agent 与远程", "运行时", "agents", "remote", "runtime"],
        ),
        category(t!("settings.group.system"), &["系统", "system"]),
        category(
            t!("settings.group.plugins"),
            &["插件", "扩展", "plugin", "plugins", "extensions"],
        ),
    ];
    // 宿主的页比插件晚登记（插件在开主窗口前装好），排序时不看登记先后。
    let host = |view: &&ViewInfo| super::is_host_view(&view.id);
    let ordered = views
        .iter()
        .filter(host)
        .chain(views.iter().filter(|view| !host(view)));
    for view in ordered {
        let Some(page) = &view.settings else {
            continue;
        };
        let index = match page.group {
            SettingsGroup::General => 0,
            SettingsGroup::Agents => 1,
            SettingsGroup::System => 2,
            SettingsGroup::Plugins => 3,
        };
        nav[index].children.push(SettingsNavLeaf {
            title: view.title.to_string(),
            icon: view.icon.clone(),
            id: view.id.clone(),
            keywords: page.keywords.iter().map(ToString::to_string).collect(),
        });
    }
    nav.retain(|category| !category.children.is_empty());
    nav
}

fn category(
    title: std::borrow::Cow<'static, str>,
    keywords: &'static [&'static str],
) -> SettingsNavCategory {
    SettingsNavCategory {
        title: title.into(),
        keywords,
        children: Vec::new(),
    }
}

/// 停在哪一页：选过的那页还在导航里就是它，不然是导航里的第一页；一页都没有是 `None`。
pub fn resolve_page<'a>(
    nav: &'a [SettingsNavCategory],
    chosen: Option<&str>,
) -> Option<&'a SettingsNavLeaf> {
    let leaves = || nav.iter().flat_map(|category| category.children.iter());
    chosen
        .and_then(|chosen| leaves().find(|leaf| leaf.id.as_ref() == chosen))
        .or_else(|| leaves().next())
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
                            std::iter::once(category.title.as_ref())
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
    std::iter::once(category.title.as_ref())
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
        nav.iter().map(|category| category.title.as_ref()).collect()
    }

    fn child_titles(nav: &[SettingsNavCategory], title: &str) -> Vec<String> {
        nav.iter()
            .find(|category| category.title.as_ref() == title)
            .map(|category| {
                category
                    .children
                    .iter()
                    .map(|child| child.title.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn matching_pages(nav: &[SettingsNavCategory], query: &str) -> Vec<SharedString> {
        filter_settings_nav(nav, query)
            .into_iter()
            .flat_map(|category| category.children)
            .map(|child| child.id)
            .collect()
    }

    fn view(id: &str, title: &str, settings: Option<smelt_plugin_api::SettingsEntry>) -> ViewInfo {
        ViewInfo {
            id: id.to_string().into(),
            title: title.to_string().into(),
            icon: "icons/bell.svg".into(),
            draws_top_bar: false,
            instances: false,
            fills: Vec::new(),
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

    /// 宿主自己的三页，和 [`super::super::view_specs`] 登记的是同一份。
    fn host_pages() -> Vec<ViewInfo> {
        super::super::HOST_PAGES
            .iter()
            .map(|page| {
                view(
                    &format!("{}/{}", crate::host_views::HOST_OWNER, page.name),
                    &(page.title)().get(),
                    Some(page.entry()),
                )
            })
            .collect()
    }

    #[test]
    fn without_plugins_only_the_host_pages_show() {
        let nav = settings_nav(&host_pages());
        assert_eq!(titles(&nav), ["General"]);
        assert_eq!(
            child_titles(&nav, "General"),
            ["Appearance", "Shortcuts", "Panel Plugins"]
        );
        assert!(settings_nav(&[]).is_empty(), "没有设置页就没有分组");
    }

    #[test]
    fn search_keeps_only_matching_pages_under_their_group() {
        let mut views = host_pages();
        views.push(plugin_page(
            "machines/remote",
            "远程",
            SettingsGroup::Agents,
            &["relay"],
        ));
        let nav = settings_nav(&views);
        let filtered = filter_settings_nav(&nav, "relay");
        assert_eq!(titles(&filtered), ["Agents & Remote"]);
        assert_eq!(child_titles(&filtered, "Agents & Remote"), ["远程"]);

        // 组名用哪种语言搜都行。
        for query in ["通用", "General"] {
            let by_group = filter_settings_nav(&nav, query);
            assert_eq!(child_titles(&by_group, "General").len(), 3);
        }
    }

    #[test]
    fn search_understands_user_tasks_and_multiple_terms() {
        let nav = settings_nav(&host_pages());
        for (query, expected) in [
            ("导入插件", "smelt/panel-plugins"),
            ("等宽字体", "smelt/appearance"),
            ("壁纸", "smelt/appearance"),
            ("背景图", "smelt/appearance"),
            ("cmd keymap", "smelt/shortcuts"),
        ] {
            assert_eq!(matching_pages(&nav, query), [expected], "query: {query}");
        }
    }

    #[test]
    fn host_pages_go_first_and_plugin_pages_follow_in_their_group() {
        // 插件比宿主先登记，宿主的页照样排在组里最前面。
        let mut pages = vec![
            plugin_page("notify/main", "提醒", SettingsGroup::General, &["系统通知"]),
            plugin_page("trader/prefs", "行情", SettingsGroup::Plugins, &["股票"]),
            // 不是设置页的视图不进设置。
            view("trader/main", "交易", None),
            plugin_page("probe/disk", "磁盘", SettingsGroup::System, &[]),
        ];
        pages.extend(host_pages());
        let nav = settings_nav(&pages);
        assert_eq!(titles(&nav), ["General", "System", "Plugins"]);
        assert_eq!(
            child_titles(&nav, "General"),
            ["Appearance", "Shortcuts", "Panel Plugins", "提醒"]
        );
        assert_eq!(child_titles(&nav, "System"), ["磁盘"]);
        assert_eq!(child_titles(&nav, "Plugins"), ["行情"]);
        // 一页都没有的组不露。
        assert_eq!(titles(&settings_nav(&pages[..1])), ["General"]);

        assert_eq!(matching_pages(&nav, "股票"), ["trader/prefs"]);
    }

    #[test]
    fn the_chosen_page_falls_back_to_the_first_one() {
        let nav = settings_nav(&host_pages());
        let page = |chosen| resolve_page(&nav, chosen).map(|leaf| leaf.id.to_string());
        assert_eq!(
            page(Some("smelt/shortcuts")).as_deref(),
            Some("smelt/shortcuts")
        );
        // 没选过、选的那页撤下了，都停在第一页。
        assert_eq!(page(None).as_deref(), Some("smelt/appearance"));
        assert_eq!(page(Some("gone/page")).as_deref(), Some("smelt/appearance"));
        assert!(resolve_page(&[], None).is_none());
    }
}
