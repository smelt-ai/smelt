//! 给 JS 插件的 `smelt-ui` 模块：把 smelt 自己的控件登记进 gpui-shell 的组件目录，脚本
//! `import { SmeltButton } from "smelt-ui"`。
//!
//! 是 [`crate::ui`] 的 JS 映射，同一套控件、同一套变体和状态，不照搬别的组件库的属性名。
//! 只登记插件真用到的控件，缺了再加；每个控件一个子模块。别的插件按实例提供的视图（终端、
//! 对话）经 `SmeltViewInstance` 嵌进来，不按功能各开一个组件。
//!
//! 弹层（菜单、确认框）比画出它的那次绘制活得久，脚本回调却只在产生它的那次 render 里
//! 有效：脚本重绘后旧回调就作废。所以弹层不攥着打开时的回调，每次绘制把最新的回调写进
//! 按控件 id 保存的槽里（[`latest`]），点下去时再从槽里取。

// 每个控件模块只经 `register` 接进 [`components`]：漏调了整个模块就没人用，在这里报错，
// 不让控件悄悄从目录里少掉。
#[deny(dead_code)]
mod alert;
#[deny(dead_code)]
mod badge;
#[deny(dead_code)]
mod button;
mod carrier;
#[deny(dead_code)]
mod choice;
#[deny(dead_code)]
mod confirm;
#[deny(dead_code)]
mod dialog;
#[deny(dead_code)]
mod drag_item;
#[deny(dead_code)]
mod field;
#[deny(dead_code)]
mod keybind;
#[deny(dead_code)]
mod list_row;
#[deny(dead_code)]
mod menu;
#[deny(dead_code)]
mod motion;
#[deny(dead_code)]
mod popover;
#[deny(dead_code)]
mod scroll_view;
#[deny(dead_code)]
mod select;
#[deny(dead_code)]
mod settings;
#[deny(dead_code)]
mod sticky;
#[deny(dead_code)]
mod tooltip;
#[deny(dead_code)]
mod truncated_line;
#[deny(dead_code)]
mod view;
#[deny(dead_code)]
mod window_bar;

use gpui::{App, ElementId, Entity, IntoElement, Refineable as _, Styled, Window};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, COMPONENT_REGISTRY_API_VERSION, ComponentArgument,
    ComponentPayload, ComponentRegistry, ConstructorDescriptor, FrozenComponentRegistry,
    MaterializeRequest, MethodDescriptor, RegistryError, anyhow,
};

/// 脚本里 `import` 的模块名。
pub const MODULE: &str = "smelt-ui";

/// 插件运行时用的组件目录。控件要的全局件由应用启动时的 `smelt_ui::init` 装好，目录不另带
/// 初始化函数。
pub fn components() -> Result<FrozenComponentRegistry, RegistryError> {
    let mut registry = ComponentRegistry::new(COMPONENT_REGISTRY_API_VERSION, MODULE)?;
    button::register(&mut registry)?;
    menu::register(&mut registry)?;
    confirm::register(&mut registry)?;
    choice::register(&mut registry)?;
    field::register(&mut registry)?;
    alert::register(&mut registry)?;
    badge::register(&mut registry)?;
    view::register(&mut registry)?;
    window_bar::register(&mut registry)?;
    list_row::register(&mut registry)?;
    sticky::register(&mut registry)?;
    truncated_line::register(&mut registry)?;
    keybind::register(&mut registry)?;
    tooltip::register(&mut registry)?;
    select::register(&mut registry)?;
    popover::register(&mut registry)?;
    settings::register(&mut registry)?;
    dialog::register(&mut registry)?;
    scroll_view::register(&mut registry)?;
    drag_item::register(&mut registry)?;
    motion::register(&mut registry)?;
    registry.freeze()
}

/// 只收一个非空字符串 id 的构造函数：`Owner.new(id)`。
fn id_constructor(owner: &'static str) -> ConstructorDescriptor {
    ConstructorDescriptor::new(
        owner,
        vec![ArgumentDescriptor::new("id", ArgumentSchema::String)],
        move |args| match args {
            [ComponentArgument::String(id)] if !id.is_empty() => {
                Ok(ComponentPayload::new(ControlId(id.clone())))
            }
            [ComponentArgument::String(_)] => Err(format!("{owner} id must not be empty")),
            _ => Err(format!("{owner}(id) expects one string")),
        },
    )
}

/// [`id_constructor`] 记下的 id。
#[derive(Clone)]
struct ControlId(String);

fn control_id(owner: &str, request: &MaterializeRequest<'_>) -> anyhow::Result<String> {
    request
        .payload()
        .downcast_ref::<ControlId>()
        .map(|id| id.0.clone())
        .ok_or_else(|| anyhow::anyhow!("{owner} received an incompatible payload"))
}

/// 收一个字符串的方法，记成 `make(值)`。
fn string_method<T: Send + Sync + 'static>(
    owner: &'static str,
    name: &'static str,
    docs: &'static str,
    make: fn(String) -> T,
) -> MethodDescriptor {
    MethodDescriptor::new(
        name,
        vec![ArgumentDescriptor::new(name, ArgumentSchema::String)],
        move |args| match args {
            [ComponentArgument::String(value)] => Ok(ComponentPayload::new(make(value.clone()))),
            _ => Err(format!("{owner}.{name} expects one string")),
        },
    )
    .with_documentation(docs)
}

/// 收一个布尔的方法，记成 `make(值)`。
fn bool_method<T: Send + Sync + 'static>(
    owner: &'static str,
    name: &'static str,
    docs: &'static str,
    make: fn(bool) -> T,
) -> MethodDescriptor {
    MethodDescriptor::new(
        name,
        vec![ArgumentDescriptor::new(name, ArgumentSchema::Boolean)],
        move |args| match args {
            [ComponentArgument::Boolean(value)] => Ok(ComponentPayload::new(make(*value))),
            _ => Err(format!("{owner}.{name} expects one boolean")),
        },
    )
    .with_documentation(docs)
}

/// 弹层贴着触发器的哪个角的方法（下拉菜单、弹出卡片）：`top_left`（默认，从下面左对齐打开）、
/// `top_right`（从下面右对齐，靠右的按钮用）、`bottom_*`（往上开），记成 `make(角)`。
fn corner_method<T: Send + Sync + 'static>(
    owner: &'static str,
    name: &'static str,
    docs: &'static str,
    make: fn(gpui::Anchor) -> T,
) -> MethodDescriptor {
    MethodDescriptor::new(
        name,
        vec![ArgumentDescriptor::new(
            "corner",
            ArgumentSchema::Enum(&["top_left", "top_right", "bottom_left", "bottom_right"]),
        )],
        move |args| {
            let anchor = match args {
                [ComponentArgument::Enum(corner)] => match corner.as_str() {
                    "top_left" => gpui::Anchor::TopLeft,
                    "top_right" => gpui::Anchor::TopRight,
                    "bottom_left" => gpui::Anchor::BottomLeft,
                    "bottom_right" => gpui::Anchor::BottomRight,
                    _ => return Err(format!("{owner}.{name}: unknown corner {corner:?}")),
                },
                _ => return Err(format!("{owner}.{name}(corner) expects a corner")),
            };
            Ok(ComponentPayload::new(make(anchor)))
        },
    )
    .with_documentation(docs)
}

/// 不带参数的方法（变体、尺寸），记成 `op`。
fn flag_method<T: Clone + Send + Sync + 'static>(
    name: &'static str,
    op: T,
    docs: &'static str,
) -> MethodDescriptor {
    MethodDescriptor::new(name, Vec::new(), move |_| {
        Ok(ComponentPayload::new(op.clone()))
    })
    .with_documentation(docs)
}

/// 收一个回调的方法，回调原样记成 `make(回调)`，物化时再解析。
fn callback_method<T: Send + Sync + 'static>(
    owner: &'static str,
    name: &'static str,
    signature: &'static str,
    docs: &'static str,
    make: fn(ComponentArgument) -> T,
) -> MethodDescriptor {
    MethodDescriptor::new(
        name,
        vec![ArgumentDescriptor::new(
            "callback",
            ArgumentSchema::Callback(signature),
        )],
        move |args| match args {
            [callback @ ComponentArgument::Callback(_)] => {
                Ok(ComponentPayload::new(make(callback.clone())))
            }
            _ => Err(format!("{owner}.{name} expects one callback")),
        },
    )
    .with_documentation(docs)
}

/// `disabled(bool)`：Shell 自己读（[`MaterializeRequest::disabled`]），描述符仍要声明，
/// 脚本才能调。
fn disabled_method(owner: &'static str) -> MethodDescriptor {
    MethodDescriptor::new(
        "disabled",
        vec![ArgumentDescriptor::new("disabled", ArgumentSchema::Boolean)],
        move |args| match args {
            [ComponentArgument::Boolean(_)] => Ok(ComponentPayload::new(())),
            _ => Err(format!("{owner}.disabled expects one boolean")),
        },
    )
    .with_documentation("Controls whether the control accepts interaction.")
}

/// 空格分开的几步快捷键（`cmd-k cmd-s`），每步的写法同快捷键设置。`secondary-` 按 `os` 解：
/// 构造时还拿不到平台，只查写法（随便给一个平台），画的时候再按真平台解一遍。
fn parse_keys(keys: &str, os: gpui::OperatingSystem) -> Result<Vec<gpui::Keystroke>, String> {
    let steps = keys
        .split_whitespace()
        .map(|step| {
            gpui::Keystroke::parse_with_os(step, os).map_err(|error| format!("{step:?}: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if steps.is_empty() {
        return Err("expected at least one keystroke".into());
    }
    Ok(steps)
}

/// `selected(bool)`：同 [`disabled_method`]，Shell 自己读（[`MaterializeRequest::selected`]）。
fn selected_method(owner: &'static str) -> MethodDescriptor {
    MethodDescriptor::new(
        "selected",
        vec![ArgumentDescriptor::new("selected", ArgumentSchema::Boolean)],
        move |args| match args {
            [ComponentArgument::Boolean(_)] => Ok(ComponentPayload::new(())),
            _ => Err(format!("{owner}.selected expects one boolean")),
        },
    )
    .with_documentation("Marks the control as the current selection.")
}

/// 记下的某一类操作，按脚本调用的顺序。
fn ops<'r, T: 'static>(request: &'r MaterializeRequest<'_>) -> impl Iterator<Item = &'r T> {
    request
        .methods()
        .filter_map(|method| method.payload().downcast_ref::<T>())
}

/// 不收子元素的控件：套上脚本给的样式。
fn finish_leaf(
    owner: &str,
    mut request: MaterializeRequest<'_>,
    mut element: impl Styled + IntoElement + 'static,
) -> anyhow::Result<gpui::AnyElement> {
    anyhow::ensure!(
        request.children_len() == 0,
        "{owner} does not accept children"
    );
    element.style().refine(&request.take_style());
    Ok(element.into_any_element())
}

/// 按 `id` 存一份每次绘制都覆盖的值，弹层在用户操作时读它，拿到的总是最近一次绘制的。
/// 只写不通知：值变了不需要重画谁。在元素的 `render` 里调。
fn latest<T: 'static>(id: &ElementId, value: T, window: &mut Window, cx: &mut App) -> Entity<T> {
    let mut value = Some(value);
    let slot = window.use_keyed_state((id.clone(), "smelt-ui-latest"), cx, |_, _| {
        value.take().expect("初值只取一次")
    });
    if let Some(value) = value {
        slot.update(cx, |slot, _| *slot = value);
    }
    slot
}

#[cfg(test)]
mod bench;
#[cfg(test)]
mod tests;
