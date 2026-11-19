//! `SmeltStickySection`：吸顶区段（[`smelt_ui::StickySection`]）。两个子元素，第一个是标题、
//! 第二个是内容；标题滚过可视区顶边时停在顶上，区段快滚完时被下一段顶走。按最近的滚动
//! 区域吸，只管一层。

use std::sync::Arc;

use gpui::{ParentElement as _, div};
use gpui_shell::{
    ComponentDescriptor, ComponentMaterializer, ComponentPayload, ComponentRegistry,
    ConstructorDescriptor, MaterializeRequest, RegistryError, anyhow, gpui,
};
use smelt_ui::StickySection;

const OWNER: &str = "SmeltStickySection";

struct StickyMaterializer;

impl ComponentMaterializer for StickyMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let mut children = request.take_children()?.into_iter();
        let (Some(header), Some(body), None) = (children.next(), children.next(), children.next())
        else {
            anyhow::bail!("{OWNER} takes exactly two children: the header, then the body");
        };
        // 区段本身不收样式，外边距这类落在外面这层；这层不裁剪，标题照样按滚动区域吸。
        request.finish(div().child(StickySection::new(header, body)))
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(StickyMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                Vec::new(),
                |_| Ok(ComponentPayload::new(())),
            )])
            .with_documentation(
                "A section whose header (first child) sticks to the top of the nearest scroll area while its body (second child) scrolls under it.",
            ),
    )?;
    Ok(())
}
