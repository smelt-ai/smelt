//! 设置页注册表：插件往设置窗口里加的页。
//!
//! 和 JetBrains 的 Configurable、Obsidian 的 `addSettingTab` 一样是命令式的：插件给标题、
//! 关键词和怎么建，宿主管侧栏、分组、搜索；页面在设置窗口里第一次点开时才建，设置窗口
//! 关了就放掉，下次打开重建。所以页面打开时要从插件自己的存储里读当前值，改了马上写回，
//! 不靠页面活着保存状态。
//!
//! 按来源整组登记，id 是 `<来源>/<名字>`，一个插件换不掉别人的页。

use std::rc::Rc;

use gpui::{AnyView, App, ElementId, Global, SharedString, Window, px};
use gpui_component::group_box::GroupBoxVariant;
use gpui_component::setting::{SettingPage, Settings};

use crate::HostEvent;

/// 设置侧栏的分组。分组归宿主，插件只挑放进哪一组，组内排在宿主自己的页后面、按登记
/// 先后。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsGroup {
    /// 「通用」：外观、通知、快捷键这类整个 App 的偏好。
    General,
    /// 「Agent 与远程」：智能体、运行时、别的机器。
    Agents,
    /// 「系统」：资源、维护、关于。
    System,
    /// 「插件」：插件自己的偏好。
    #[default]
    Plugins,
}

/// 登记一个设置页要给的东西。
#[derive(Clone)]
pub struct SettingsPageSpec {
    /// 来源内的名字，不含 `/`。
    pub name: SharedString,
    /// 侧栏上的名字。
    pub title: SharedString,
    /// 侧栏图标的资源路径，如 `IconName::Bell.path()`。
    pub icon: SharedString,
    pub group: SettingsGroup,
    /// 搜索时除了标题还能按这些词找到这一页：页里各项的名字、同义词。
    pub keywords: Vec<SharedString>,
    /// 设置窗口里第一次点开这一页时建，`window` 是设置窗口。
    pub build: SettingsPageBuilder,
}

/// 建设置页的函数。
pub type SettingsPageBuilder = Rc<dyn Fn(&mut Window, &mut App) -> AnyView>;

/// 登记过的一个设置页。
#[derive(Clone)]
pub struct SettingsPage {
    /// `<来源>/<名字>`。
    pub id: SharedString,
    pub title: SharedString,
    pub icon: SharedString,
    pub group: SettingsGroup,
    pub keywords: Vec<SharedString>,
    /// 宿主拿它建页，也拿它认出登记换过了（`Rc::ptr_eq`），建好的旧页要放掉。
    pub build: SettingsPageBuilder,
}

struct Entry {
    owner: String,
    page: SettingsPage,
}

#[derive(Default)]
struct SettingsRegistry {
    pages: Vec<Entry>,
}

impl Global for SettingsRegistry {}

/// 按来源整组登记设置页：这个来源原来的页全换成这一组，空的就是全撤下。一页不合法整组
/// 不登记，原来的不动。来源第一次登记排在最后，再登记留在原来的位置。
pub fn register_settings_pages(
    owner: &str,
    specs: Vec<SettingsPageSpec>,
    cx: &mut App,
) -> Result<(), String> {
    if owner.is_empty() || owner.contains('/') {
        return Err(format!("设置页来源「{owner}」不能为空，也不能含 `/`"));
    }
    let mut entries: Vec<Entry> = Vec::with_capacity(specs.len());
    for spec in specs {
        if spec.name.is_empty() || spec.name.contains('/') {
            return Err(format!("设置页名「{}」不能为空，也不能含 `/`", spec.name));
        }
        let id: SharedString = format!("{owner}/{}", spec.name).into();
        if entries.iter().any(|entry| entry.page.id == id) {
            return Err(format!("设置页「{id}」登记了两次"));
        }
        if spec.title.trim().is_empty() {
            return Err(format!("设置页「{id}」没有标题"));
        }
        entries.push(Entry {
            owner: owner.to_string(),
            page: SettingsPage {
                id,
                title: spec.title,
                icon: spec.icon,
                group: spec.group,
                keywords: spec.keywords,
                build: spec.build,
            },
        });
    }
    let pages = &mut cx.default_global::<SettingsRegistry>().pages;
    let at = pages
        .iter()
        .position(|entry| entry.owner == owner)
        .unwrap_or(pages.len());
    let had = pages.len();
    pages.retain(|entry| entry.owner != owner);
    if had == pages.len() && entries.is_empty() {
        return Ok(());
    }
    let at = at.min(pages.len());
    pages.splice(at..at, entries);
    crate::emit(HostEvent::SettingsPagesChanged, cx);
    Ok(())
}

/// 撤下一个来源的全部设置页。
pub fn unregister_settings_pages(owner: &str, cx: &mut App) {
    if !cx.has_global::<SettingsRegistry>() {
        return;
    }
    let pages = &mut cx.global_mut::<SettingsRegistry>().pages;
    let before = pages.len();
    pages.retain(|entry| entry.owner != owner);
    if pages.len() != before {
        crate::emit(HostEvent::SettingsPagesChanged, cx);
    }
}

/// 登记过的设置页，按登记顺序。只读内存，render 里可以调。
pub fn settings_pages(cx: &App) -> Vec<SettingsPage> {
    cx.try_global::<SettingsRegistry>()
        .map(|registry| {
            registry
                .pages
                .iter()
                .map(|entry| entry.page.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// 一个登记过的设置页；没登记是 `None`。只读内存。
pub fn settings_page(id: &str, cx: &App) -> Option<SettingsPage> {
    cx.try_global::<SettingsRegistry>()?
        .pages
        .iter()
        .find(|entry| entry.page.id.as_ref() == id)
        .map(|entry| entry.page.clone())
}

/// 设置窗口内容区里的一页，宿主自己的页也用它，各页长得一样：每节一张描边卡片，不带
/// 组件库 `Settings` 自己的侧栏。`id` 在设置窗口里要唯一。
pub fn settings_pane(id: impl Into<ElementId>, page: SettingPage) -> Settings {
    // 不用 Fill：它的底色是背景混 30% 的次级色，近黑主题上跟背景几乎分不出来；描边在
    // 各主题下都看得见。
    Settings::new(id)
        .with_group_variant(GroupBoxVariant::Outline)
        .sidebar_width(px(0.))
        .sidebar_size_range(px(0.)..px(0.))
        .pages(vec![page])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> SettingsPageSpec {
        SettingsPageSpec {
            name: name.to_string().into(),
            title: name.to_string().into(),
            icon: "icons/bell.svg".into(),
            group: SettingsGroup::General,
            keywords: Vec::new(),
            build: Rc::new(|_, _| unreachable!("登记时不建页")),
        }
    }

    fn ids(cx: &App) -> Vec<String> {
        settings_pages(cx)
            .into_iter()
            .map(|page| page.id.into())
            .collect()
    }

    #[gpui::test]
    fn pages_are_registered_per_owner_and_keep_their_place(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_settings_pages("notify", vec![spec("main")], cx).unwrap();
            register_settings_pages("trader", vec![spec("a"), spec("b")], cx).unwrap();
            assert_eq!(ids(cx), ["notify/main", "trader/a", "trader/b"]);
            // 再登记是整组替换，位置不变；换过的页建页函数也换了，宿主据此放掉旧页。
            let old = settings_page("notify/main", cx).unwrap().build;
            register_settings_pages("notify", vec![spec("main"), spec("more")], cx).unwrap();
            assert_eq!(
                ids(cx),
                ["notify/main", "notify/more", "trader/a", "trader/b"]
            );
            assert!(!Rc::ptr_eq(
                &old,
                &settings_page("notify/main", cx).unwrap().build
            ));
            unregister_settings_pages("trader", cx);
            assert_eq!(ids(cx), ["notify/main", "notify/more"]);
            register_settings_pages("notify", Vec::new(), cx).unwrap();
            assert!(settings_pages(cx).is_empty());
            assert!(settings_page("notify/main", cx).is_none());
        });
    }

    #[gpui::test]
    fn an_invalid_page_rejects_the_whole_set(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_settings_pages("trader", vec![spec("a")], cx).unwrap();
            let mut untitled = spec("b");
            untitled.title = " ".into();
            for specs in [
                vec![spec("b"), spec("b")],
                vec![spec("b"), spec("x/y")],
                vec![spec("")],
                vec![spec("b"), untitled],
            ] {
                assert!(register_settings_pages("trader", specs, cx).is_err());
                assert_eq!(ids(cx), ["trader/a"]);
            }
            for owner in ["", "a/b"] {
                assert!(register_settings_pages(owner, vec![spec("b")], cx).is_err());
            }
        });
    }
}
