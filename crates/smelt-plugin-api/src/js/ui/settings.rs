//! 设置页的骨架（[`smelt_ui::settings`]），宿主和插件的设置页都用它，各页长得一样：
//!
//! - `SmeltSettingsPage.new(id)`：竖着滚的一列分组，页名在设置的工具栏里，这里不画页头。
//! - `SmeltSettingsSection.new()`：分组，`title` / `description` 在卡片上面，子项之间自动画
//!   分隔线。
//! - `SmeltSettingsRow.new(名字)`：一行设置，左边名字和说明，右边控件（子元素）。
//!
//! 外观归骨架，不收样式。

use std::sync::Arc;

use gpui::{IntoElement as _, ParentElement as _, StyleRefinement};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, RegistryError, anyhow, gpui,
};
use smelt_ui::{SettingsPage, SettingsRow, SettingsSection};

use super::{control_id, id_constructor, ops, string_method};

const PAGE: &str = "SmeltSettingsPage";
const SECTION: &str = "SmeltSettingsSection";
const ROW: &str = "SmeltSettingsRow";

#[derive(Clone)]
enum TextOp {
    Title(String),
    Description(String),
    Note(String),
}

#[derive(Clone)]
struct RowTitle(String);

fn no_style(owner: &str, request: &mut MaterializeRequest<'_>) -> anyhow::Result<()> {
    anyhow::ensure!(
        request.take_style() == StyleRefinement::default(),
        "{owner} draws its own layout and takes no styles"
    );
    Ok(())
}

struct PageMaterializer;

impl ComponentMaterializer for PageMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        no_style(PAGE, &mut request)?;
        let page = SettingsPage::new(control_id(PAGE, &request)?);
        Ok(page.children(request.take_children()?).into_any_element())
    }
}

struct SectionMaterializer;

impl ComponentMaterializer for SectionMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        no_style(SECTION, &mut request)?;
        let mut section = SettingsSection::new();
        for op in ops::<TextOp>(&request) {
            section = match op {
                TextOp::Title(text) => section.title(text.clone()),
                TextOp::Description(text) => section.description(text.clone()),
                TextOp::Note(_) => section,
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
        no_style(ROW, &mut request)?;
        let title = request
            .payload()
            .downcast_ref::<RowTitle>()
            .ok_or_else(|| anyhow::anyhow!("{ROW} received an incompatible payload"))?
            .0
            .clone();
        let mut row = SettingsRow::new(title);
        for op in ops::<TextOp>(&request) {
            row = match op {
                TextOp::Description(text) => row.description(text.clone()),
                TextOp::Note(text) => row.note(text.clone()),
                TextOp::Title(_) => row,
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
                "A settings page: a scrolling column of SmeltSettingsSection children, at the shared page width.",
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
                    TextOp::Title,
                ),
                string_method(
                    SECTION,
                    "description",
                    "A sentence under the heading, above the card.",
                    TextOp::Description,
                ),
            ])
            .with_documentation(
                "A settings group: a bordered card whose children are separated by hairlines.",
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
                    TextOp::Description,
                ),
                string_method(
                    ROW,
                    "note",
                    "A note under the description (source, who uses it, why it is unavailable); line breaks are kept.",
                    TextOp::Note,
                ),
            ])
            .with_documentation(
                "One setting: name and description on the left, its control (the children) on the right.",
            ),
    )?;
    Ok(())
}
