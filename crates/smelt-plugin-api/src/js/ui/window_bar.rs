//! `SmeltWindowBar`：自己画顶栏的视图（登记时 `draws_top_bar: true`）顶上那一条，是
//! [`smelt_ui::window_bar::window_bar`] 的 JS 映射。脚本给自己视图的 id，子元素是条里的控件。
//!
//! 按那个视图的 safe area 画：顶上是窗口标题栏时画 34 高的一条，两头给角上的控件让位，空白
//! 处拖窗口、双击缩放（Windows 用系统标题栏，不当拖动区）；顶上不是标题栏（摆在下面、并排在
//! 别的视图之后）时什么都不画。按 id 找得到的只有不按实例建的视图。

use std::sync::Arc;

use gpui::{
    AnyElement, App, ElementId, IntoElement, ParentElement as _, Refineable as _, RenderOnce,
    StyleRefinement, Styled, Window, div,
};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, RegistryError, anyhow,
};

const OWNER: &str = "SmeltWindowBar";

/// 构造时给的视图 id。
#[derive(Clone)]
struct ViewId(String);

struct WindowBarMaterializer;

impl ComponentMaterializer for WindowBarMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<AnyElement> {
        let ViewId(view) = request
            .payload()
            .downcast_ref::<ViewId>()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?;
        Ok(Bar {
            id: request.element_id().clone(),
            view,
            style: request.take_style(),
            children: request.take_children()?,
        }
        .into_any_element())
    }
}

#[derive(IntoElement)]
struct Bar {
    id: ElementId,
    view: String,
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl RenderOnce for Bar {
    /// safe area 只读内存，绘制时取最新的：视图挪了位置下一帧就跟上。
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let area = crate::built_view(&self.view, cx)
            .map(|view| crate::safe_area(view.view().entity_id(), cx))
            .unwrap_or_default();
        if !area.title_bar {
            return div().into_any_element();
        }
        let mut bar = smelt_ui::window_bar::window_bar(self.id, &area);
        bar.style().refine(&self.style);
        bar.children(self.children).into_any_element()
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(WindowBarMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                vec![ArgumentDescriptor::new("view", ArgumentSchema::String)],
                |args| match args {
                    [ComponentArgument::String(view)] if !view.trim().is_empty() => {
                        Ok(ComponentPayload::new(ViewId(view.clone())))
                    }
                    [ComponentArgument::String(_)] => {
                        Err(format!("{OWNER} view id must not be empty"))
                    }
                    _ => Err(format!("{OWNER}(view) expects one string")),
                },
            )])
            .with_documentation(
                "The top bar of a view registered with draws_top_bar: true, given that view's id \
                 (\"<plugin id>/<name>\"). Children are the bar's controls. When the view's top \
                 edge is the window's title bar it is a 34px row that leaves room for the corner \
                 controls, drags the window from its empty space and zooms on double-click; \
                 otherwise it draws nothing.",
            ),
    )?;
    Ok(())
}
