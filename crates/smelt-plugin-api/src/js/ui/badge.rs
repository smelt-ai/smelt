//! `SmeltBadge`：小标记（[`smelt_ui::Badge`]），子元素是它的字。默认灰；状态色只给状态：
//! `ok()` 正常、`need()` 要你处理、`bad()` 出错。

use std::sync::Arc;

use gpui_shell::{
    ComponentDescriptor, ComponentMaterializer, ComponentPayload, ComponentRegistry,
    ConstructorDescriptor, MaterializeRequest, RegistryError, anyhow, gpui,
};
use smelt_ui::Badge;

use super::{flag_method, ops};

const OWNER: &str = "SmeltBadge";

#[derive(Clone, Copy)]
enum Tone {
    Ok,
    Need,
    Bad,
}

struct BadgeMaterializer;

impl ComponentMaterializer for BadgeMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let badge = match ops::<Tone>(&request).last() {
            None => Badge::new(),
            Some(Tone::Ok) => Badge::new().ok(),
            Some(Tone::Need) => Badge::new().need(),
            Some(Tone::Bad) => Badge::new().bad(),
        };
        request.finish(badge)
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(BadgeMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                Vec::new(),
                |_| Ok(ComponentPayload::new(())),
            )])
            .with_methods(vec![
                flag_method("ok", Tone::Ok, "Marks a healthy state, such as configured or available."),
                flag_method("need", Tone::Need, "Marks something the user needs to act on."),
                flag_method("bad", Tone::Bad, "Marks an error or something unusable."),
            ])
            .with_documentation(
                "A Smelt badge whose children are its text; neutral gray unless a status tone is set.",
            ),
    )?;
    Ok(())
}
