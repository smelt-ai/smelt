//! 设置页：带 [`SettingsEntry`] 的视图（见 [`crate::register_views`]）。
//!
//! 和 JetBrains 的 Configurable、Obsidian 的 `addSettingTab` 一样是命令式的：插件给标题、
//! 关键词和怎么建，宿主管侧栏、分组、搜索。页面打开时要从插件自己的存储里读当前值，改了
//! 马上写回，不靠页面活着保存状态。

use gpui::{
    Display, Div, ElementId, ParentElement as _, Pixels, SharedString, StyleRefinement,
    Styled as _, div, px,
};
use gpui_component::group_box::GroupBoxVariant;
use gpui_component::setting::{SettingPage, Settings};

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

/// 视图是一页设置时多给的：放进哪一组、按哪些词能搜到（见 [`crate::ViewSpec::settings`]）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsEntry {
    pub group: SettingsGroup,
    /// 搜索时除了标题还能按这些词找到这一页：页里各项的名字、同义词。
    pub keywords: Vec<SharedString>,
}

/// 设置内容区里的一页，宿主自己的页也用它，各页长得一样：每节一张描边卡片，不带
/// 组件库 `Settings` 自己的侧栏，也不带它的页头——页名在设置的工具栏里，取自导航。
/// 所以页级的说明、标题后缀都不会显示，要说的话写进分组。`id` 在设置里要唯一。
pub fn settings_pane(id: impl Into<ElementId>, page: SettingPage) -> Div {
    let no_header = StyleRefinement {
        display: Some(Display::None),
        ..Default::default()
    };
    // 不用 Fill：它的底色是背景混 30% 的次级色，近黑主题上跟背景几乎分不出来；描边在
    // 各主题下都看得见。
    let settings = Settings::new(id)
        .with_group_variant(GroupBoxVariant::Outline)
        .sidebar_width(px(0.))
        .sidebar_size_range(px(0.)..px(0.))
        .pages(vec![page.header_style(&no_header)]);
    // `Settings` 总是左右两栏，侧栏宽 0 也照样在内容左沿画一条能拖的分栏线（发丝加两边各
    // 4px 的拖动带），拖不动却显示成分隔线和拖动光标。往左挪出这一截再裁掉。
    // TODO: 上游 `Settings` 支持不要侧栏后删掉这层。
    const SPLITTER_BAND: Pixels = px(9.);
    div().relative().size_full().overflow_hidden().child(
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .left(-SPLITTER_BAND)
            .child(settings),
    )
}
