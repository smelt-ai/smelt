//! 设置页的骨架（[`smelt_ui::settings`]），宿主和插件的设置页都用它，各页长得一样：
//!
//! - `SmeltSettingsPage.new(id)`：竖着滚的一列分组，页名在设置的工具栏里，这里不画页头。
//!   样式落在那一列上（限宽、内边距、组间距）。
//! - `SmeltSettingsSection.new()`：分组，`title` / `description` 在卡片上面，子项之间自动画
//!   分隔线。样式落在整组的外框上。
//! - `SmeltSettingsRow.new(名字)`：一行设置，左边名字和说明，右边控件（子元素）。说明可能很长
//!   （报错原文）时 `clamp_description(id, 行数)` 收成几行，悬停看全文。样式落在整行的外框上。
//!
//! 不给样式就是各页共用的默认样子。

use std::sync::Arc;

use gpui::{IntoElement as _, ParentElement as _, SharedString, Styled as _};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, MethodDescriptor, RegistryError, anyhow, gpui,
};
use smelt_ui::{SettingsPage, SettingsRow, SettingsSection};

use super::{control_id, id_constructor, ops, string_method};

const PAGE: &str = "SmeltSettingsPage";
const SECTION: &str = "SmeltSettingsSection";
const ROW: &str = "SmeltSettingsRow";

#[derive(Clone)]
enum SectionOp {
    Title(String),
    Description(String),
}

#[derive(Clone)]
enum RowOp {
    Description(String),
    Note(String),
    Clamp(String, usize),
}

#[derive(Clone)]
struct RowTitle(String);

struct PageMaterializer;

impl ComponentMaterializer for PageMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let mut page = SettingsPage::new(control_id(PAGE, &request)?);
        *page.style() = request.take_style();
        Ok(page.children(request.take_children()?).into_any_element())
    }
}

struct SectionMaterializer;

impl ComponentMaterializer for SectionMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let mut section = SettingsSection::new();
        *section.style() = request.take_style();
        for op in ops::<SectionOp>(&request) {
            section = match op {
                SectionOp::Title(text) => section.title(text.clone()),
                SectionOp::Description(text) => section.description(text.clone()),
            };
        }
        Ok(section
            .children(request.take_children()?)
            .into_any_element())
    }
}

struct RowMaterializer;

impl ComponentMaterializer for RowMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let title = request
            .payload()
            .downcast_ref::<RowTitle>()
            .ok_or_else(|| anyhow::anyhow!("{ROW} received an incompatible payload"))?
            .0
            .clone();
        let mut row = SettingsRow::new(title);
        *row.style() = request.take_style();
        for op in ops::<RowOp>(&request) {
            row = match op {
                RowOp::Description(text) => row.description(text.clone()),
                RowOp::Note(text) => row.note(text.clone()),
                RowOp::Clamp(id, lines) => {
                    row.clamp_description(SharedString::from(id.clone()), *lines)
                }
            };
        }
        Ok(row.children(request.take_children()?).into_any_element())
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(PAGE, Arc::new(PageMaterializer))
            .with_constructors(vec![id_constructor(PAGE)])
            .with_documentation(
                "A settings page: a scrolling column of SmeltSettingsSection children, at the shared page width; styles apply to the column (width, padding, gap).",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new(SECTION, Arc::new(SectionMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                SECTION,
                Vec::new(),
                |_| Ok(ComponentPayload::new(())),
            )])
            .with_methods(vec![
                string_method(
                    SECTION,
                    "title",
                    "The group heading above the card.",
                    SectionOp::Title,
                ),
                string_method(
                    SECTION,
                    "description",
                    "A sentence under the heading, above the card.",
                    SectionOp::Description,
                ),
            ])
            .with_documentation(
                "A settings group: a bordered card whose children are separated by hairlines; styles apply to the group's frame.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new(ROW, Arc::new(RowMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                ROW,
                vec![ArgumentDescriptor::new("title", ArgumentSchema::String)],
                |args| match args {
                    [ComponentArgument::String(title)] if !title.trim().is_empty() => {
                        Ok(ComponentPayload::new(RowTitle(title.clone())))
                    }
                    _ => Err(format!("{ROW}.new(title) expects a non-empty string")),
                },
            )])
            .with_methods(vec![
                string_method(
                    ROW,
                    "description",
                    "What the setting does, under its name; may wrap.",
                    RowOp::Description,
                ),
                MethodDescriptor::new(
                    "clamp_description",
                    vec![
                        ArgumentDescriptor::new("id", ArgumentSchema::String),
                        ArgumentDescriptor::new("lines", ArgumentSchema::Number),
                    ],
                    |args| match args {
                        [ComponentArgument::String(id), ComponentArgument::Number(lines)]
                            if !id.is_empty() && lines.is_finite() && *lines >= 1. =>
                        {
                            Ok(ComponentPayload::new(RowOp::Clamp(
                                id.clone(),
                                *lines as usize,
                            )))
                        }
                        _ => Err(format!(
                            "{ROW}.clamp_description(id, lines) expects a non-empty id and a line count of at least 1"
                        )),
                    },
                )
                .with_documentation(
                    "Shows at most `lines` lines of the description, the rest on hover; `id` names the hover tip and must be unique on the page.",
                ),
                string_method(
                    ROW,
                    "note",
                    "A note under the description (source, who uses it, why it is unavailable); line breaks are kept.",
                    RowOp::Note,
                ),
            ])
            .with_documentation(
                "One setting: name and description on the left, its control (the children) on the right; styles apply to the row.",
            ),
    )?;
    Ok(())
}
