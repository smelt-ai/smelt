//! 列表的动画零件（[`smelt_ui::motion`]）。进度在每帧的物化里按时间取，脚本不用每帧重跑：
//! 脚本只说要什么状态（开着还是合着、第几次重排、第几次落进来），动画由这里播完。
//!
//! - `SmeltFold.new(key, collapsed)`：一组的子行，开合时按弹簧量高裁剪、淡入淡出；合上停稳
//!   后子行不画。`SmeltFoldChevron.new(key, collapsed)` 是同一组标题上的箭头，跟着同一个
//!   进度从朝右转到朝下。`key` 在视图里跟着那一组的身份走。
//! - `SmeltFlip.new(key, generation)`：重排时这一项从上一帧画的位置滑到新位置。重排了就把
//!   `generation` 加一；`from_top(top)` 让这一次从那个高度落下（拖着松手时的牌子上沿）。
//! - `SmeltArrival.new(key, serial)`：新的一行撑开落进来。`serial` 头一回见或变了就播一次。
//! - `SmeltWash.new(key, serial)`：铺一层底色淡入、停一下、退掉，让眼睛找到这一行。放在行里
//!   第一个子元素，压在图标和字下面。减弱动态效果时照播（不动位置）。
//! - `SmeltGlowIcon.new(path, key, serial)`：运行色的图标，刚开始跑时亮一下再回到运行色。
//!   用户看不见（应用、窗口在后台）或减弱动态效果时不播，叫停的那一次不再接着播。
//!
//! 样式：`SmeltFold`、`SmeltFlip`、`SmeltArrival` 落在包着子元素的那层上，`SmeltWash` 落在底色
//! 那层上（默认铺满、行的圆角、正文色），`SmeltFoldChevron`、`SmeltGlowIcon` 落在图标上
//! （尺寸；颜色由动画定的不收）。

use std::sync::Arc;

use gpui::{
    AnyElement, ElementId, IntoElement as _, ParentElement as _, Refineable as _, Styled as _,
    Transformation, div, px, radians, rgb, svg,
};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, MethodDescriptor, RegistryError, anyhow, gpui,
};
use smelt_ui::IconName;
use smelt_ui::motion::{
    ARRIVAL_OPEN, RUNNING_GLOW, WASH_END, ambient_application_active, ambient_motion_enabled,
    arrival_open, disclosure_open, disclosure_settled_closed, flip_slot, fold_body, replay_elapsed,
    wash_alpha,
};
use smelt_ui::ui_theme;

use super::ops;

/// 一组开合的弹簧挂在哪：标题上的箭头和子行取同一个。
fn fold_id(key: &str) -> ElementId {
    ElementId::Name(format!("smelt-fold:{key}").into())
}

// ---- SmeltFold / SmeltFoldChevron

#[derive(Clone)]
struct Fold {
    key: String,
    collapsed: bool,
}

fn fold_constructor(owner: &'static str) -> ConstructorDescriptor {
    ConstructorDescriptor::new(
        owner,
        vec![
            ArgumentDescriptor::new("key", ArgumentSchema::String),
            ArgumentDescriptor::new("collapsed", ArgumentSchema::Boolean),
        ],
        move |args| match args {
            [
                ComponentArgument::String(key),
                ComponentArgument::Boolean(collapsed),
            ] if !key.is_empty() => Ok(ComponentPayload::new(Fold {
                key: key.clone(),
                collapsed: *collapsed,
            })),
            _ => Err(format!(
                "{owner}.new(key, collapsed) expects a non-empty string and a boolean"
            )),
        },
    )
}

fn fold_payload(owner: &str, request: &MaterializeRequest<'_>) -> anyhow::Result<Fold> {
    request
        .payload()
        .downcast_ref::<Fold>()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{owner} received an incompatible payload"))
}

struct FoldMaterializer;

impl ComponentMaterializer for FoldMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<AnyElement> {
        let Fold { key, collapsed } = fold_payload("SmeltFold", &request)?;
        let open = request.with_window_app(|window, cx| {
            Ok(disclosure_open(fold_id(&key), collapsed, window, cx))
        })?;
        let children = request.take_children()?;
        let mut body = div().flex().flex_col().w_full();
        body.style().refine(&request.take_style());
        if disclosure_settled_closed(collapsed, open) {
            return Ok(div().into_any_element());
        }
        let body = body.opacity(open).children(children);
        Ok(fold_body(
            ElementId::Name(format!("smelt-fold-body:{key}").into()),
            open,
            body,
        )
        .into_any_element())
    }
}

struct ChevronMaterializer;

impl ComponentMaterializer for ChevronMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<AnyElement> {
        let Fold { key, collapsed } = fold_payload("SmeltFoldChevron", &request)?;
        anyhow::ensure!(
            request.children_len() == 0,
            "SmeltFoldChevron does not accept children"
        );
        let open = request.with_window_app(|window, cx| {
            Ok(disclosure_open(fold_id(&key), collapsed, window, cx))
        })?;
        // 收起朝右 0°，展开朝下 90°。
        let mut icon = svg()
            .path(IconName::ChevronRight.path())
            .with_transformation(Transformation::rotate(radians((open * 90.).to_radians())));
        icon.style().refine(&request.take_style());
        Ok(icon.into_any_element())
    }
}

// ---- SmeltFlip

#[derive(Clone)]
struct Flip {
    key: String,
    generation: u64,
}

#[derive(Clone)]
struct FromTop(Option<f32>);

/// JS 的数是 f64；序号要是非负整数。
fn serial(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0. && value.fract() == 0. && value <= u64::MAX as f64)
        .then_some(value as u64)
}

struct FlipMaterializer;

impl ComponentMaterializer for FlipMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<AnyElement> {
        let Flip { key, generation } = request
            .payload()
            .downcast_ref::<Flip>()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("SmeltFlip received an incompatible payload"))?;
        let from_top = ops::<FromTop>(&request).last().and_then(|top| top.0);
        let children = request.take_children()?;
        let mut item = div();
        item.style().refine(&request.take_style());
        let item = item.children(children);
        Ok(flip_slot(
            ElementId::Name(format!("smelt-flip:{key}").into()),
            generation,
            item,
        )
        .from_top(from_top.map(px))
        .into_any_element())
    }
}

// ---- SmeltArrival / SmeltWash / SmeltGlowIcon

#[derive(Clone)]
struct Replay {
    key: String,
    serial: u64,
}

fn replay_constructor(owner: &'static str) -> ConstructorDescriptor {
    ConstructorDescriptor::new(
        owner,
        vec![
            ArgumentDescriptor::new("key", ArgumentSchema::String),
            ArgumentDescriptor::new("serial", ArgumentSchema::Number),
        ],
        move |args| match args {
            [
                ComponentArgument::String(key),
                ComponentArgument::Number(number),
            ] if !key.is_empty() => {
                let serial = serial(*number)
                    .ok_or_else(|| format!("{owner} serial must be a non-negative integer"))?;
                Ok(ComponentPayload::new(Replay {
                    key: key.clone(),
                    serial,
                }))
            }
            _ => Err(format!(
                "{owner}.new(key, serial) expects a non-empty string and a number"
            )),
        },
    )
}

fn replay_payload(owner: &str, request: &MaterializeRequest<'_>) -> anyhow::Result<Replay> {
    request
        .payload()
        .downcast_ref::<Replay>()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{owner} received an incompatible payload"))
}

struct ArrivalMaterializer;

impl ComponentMaterializer for ArrivalMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<AnyElement> {
        let Replay { key, serial } = replay_payload("SmeltArrival", &request)?;
        let open = request.with_window_app(|window, cx| {
            let elapsed = replay_elapsed(
                ElementId::Name(format!("smelt-arrival:{key}").into()),
                serial,
                false,
                window,
                cx,
            )
            .unwrap_or(ARRIVAL_OPEN);
            // 减弱动态效果时直接到全高，铺底（SmeltWash）照播。
            let open = if cx.reduce_motion() {
                1.
            } else {
                arrival_open(elapsed)
            };
            if open < 1. {
                window.request_animation_frame();
            }
            Ok(open)
        })?;
        let children = request.take_children()?;
        let mut row = div().flex().flex_col().w_full();
        row.style().refine(&request.take_style());
        let row = row.children(children);
        if open >= 1. {
            return Ok(row.into_any_element());
        }
        Ok(fold_body(
            ElementId::Name(format!("smelt-arrival-body:{key}").into()),
            open,
            row.opacity(open),
        )
        .into_any_element())
    }
}

struct WashMaterializer;

impl ComponentMaterializer for WashMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<AnyElement> {
        let Replay { key, serial } = replay_payload("SmeltWash", &request)?;
        anyhow::ensure!(
            request.children_len() == 0,
            "SmeltWash does not accept children"
        );
        let alpha = request.with_window_app(|window, cx| {
            let elapsed = replay_elapsed(
                ElementId::Name(format!("smelt-wash:{key}").into()),
                serial,
                false,
                window,
                cx,
            )
            .unwrap_or(WASH_END);
            if elapsed < WASH_END {
                window.request_animation_frame();
            }
            Ok(wash_alpha(elapsed))
        })?;
        let mut layer = div()
            .absolute()
            .inset_0()
            .rounded(ui_theme::row_radius())
            .bg(rgb(ui_theme::text()));
        layer.style().refine(&request.take_style());
        Ok(layer.opacity(alpha).into_any_element())
    }
}

#[derive(Clone)]
struct GlowIcon {
    path: String,
    replay: Replay,
}

struct GlowIconMaterializer;

impl ComponentMaterializer for GlowIconMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<AnyElement> {
        let GlowIcon { path, replay } = request
            .payload()
            .downcast_ref::<GlowIcon>()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("SmeltGlowIcon received an incompatible payload"))?;
        anyhow::ensure!(
            request.children_len() == 0,
            "SmeltGlowIcon does not accept children"
        );
        let color = request.with_window_app(|window, cx| {
            // 装饰：用户看不见、或减弱动态效果时不播，这一次也不再接着播。
            let ambient = ambient_motion_enabled(
                ambient_application_active(cx),
                window.is_window_active(),
                true,
            );
            let elapsed = replay_elapsed(
                ElementId::Name(format!("smelt-glow:{}", replay.key).into()),
                replay.serial,
                !ambient || cx.reduce_motion(),
                window,
                cx,
            );
            Ok(match elapsed {
                Some(elapsed) if elapsed < RUNNING_GLOW => {
                    window.request_animation_frame();
                    ui_theme::running_glow_color(elapsed.as_secs_f32() / RUNNING_GLOW.as_secs_f32())
                }
                _ => rgb(ui_theme::run()),
            })
        })?;
        let mut icon = svg().path(path);
        icon.style().refine(&request.take_style());
        Ok(icon.text_color(color).into_any_element())
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new("SmeltFold", Arc::new(FoldMaterializer))
            .with_constructors(vec![fold_constructor("SmeltFold")])
            .with_documentation(
                "The rows of a group that opens and closes: on a change of `collapsed` they spring open or shut, clipped to their measured height and fading; once shut they are not drawn. `key` follows the group's identity within the view.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new("SmeltFoldChevron", Arc::new(ChevronMaterializer))
            .with_constructors(vec![fold_constructor("SmeltFoldChevron")])
            .with_documentation(
                "The disclosure chevron of the group with the same `key`: it turns from right (closed) to down (open) with the group's rows. Styles apply to the icon.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new("SmeltFlip", Arc::new(FlipMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                "SmeltFlip",
                vec![
                    ArgumentDescriptor::new("key", ArgumentSchema::String),
                    ArgumentDescriptor::new("generation", ArgumentSchema::Number),
                ],
                |args| match args {
                    [ComponentArgument::String(key), ComponentArgument::Number(number)]
                        if !key.is_empty() =>
                    {
                        let generation = serial(*number).ok_or_else(|| {
                            "SmeltFlip generation must be a non-negative integer".to_string()
                        })?;
                        Ok(ComponentPayload::new(Flip {
                            key: key.clone(),
                            generation,
                        }))
                    }
                    _ => Err(
                        "SmeltFlip.new(key, generation) expects a non-empty string and a number"
                            .into(),
                    ),
                },
            )])
            .with_methods(vec![
                MethodDescriptor::new(
                    "from_top",
                    vec![ArgumentDescriptor::new(
                        "top",
                        ArgumentSchema::Optional(Box::new(ArgumentSchema::Number)),
                    )],
                    |args| match args {
                        [ComponentArgument::Optional(None)] => {
                            Ok(ComponentPayload::new(FromTop(None)))
                        }
                        [ComponentArgument::Optional(Some(value))] => match **value {
                            ComponentArgument::Number(top) if top.is_finite() => {
                                Ok(ComponentPayload::new(FromTop(Some(top as f32))))
                            }
                            _ => Err("SmeltFlip.from_top expects a finite number or null".into()),
                        },
                        _ => Err("SmeltFlip.from_top expects a number or null".into()),
                    },
                )
                .with_documentation(
                    "The window y this item starts from on the next rearrangement, instead of where it was last drawn: the drag preview's top edge on a drop.",
                ),
            ])
            .with_documentation(
                "An item that slides from where it was drawn to its new place when its list is rearranged: bump `generation` on every rearrangement. `key` follows the item's identity, never its index.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new("SmeltArrival", Arc::new(ArrivalMaterializer))
            .with_constructors(vec![replay_constructor("SmeltArrival")])
            .with_documentation(
                "A row that opens into the list: its height and opacity grow once whenever `serial` is first seen or changes. Under reduced motion it is at full height at once.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new("SmeltWash", Arc::new(WashMaterializer))
            .with_constructors(vec![replay_constructor("SmeltWash")])
            .with_documentation(
                "A layer that fades in under a row, holds and fades out once whenever `serial` is first seen or changes, so the eye finds the row. Put it first inside the row. Styles apply to the layer, which fills its relative parent with the row radius and text color by default.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new("SmeltGlowIcon", Arc::new(GlowIconMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                "SmeltGlowIcon",
                vec![
                    ArgumentDescriptor::new("path", ArgumentSchema::String),
                    ArgumentDescriptor::new("key", ArgumentSchema::String),
                    ArgumentDescriptor::new("serial", ArgumentSchema::Number),
                ],
                |args| match args {
                    [
                        ComponentArgument::String(path),
                        ComponentArgument::String(key),
                        ComponentArgument::Number(number),
                    ] if !path.is_empty() && !key.is_empty() => {
                        let serial = serial(*number).ok_or_else(|| {
                            "SmeltGlowIcon serial must be a non-negative integer".to_string()
                        })?;
                        Ok(ComponentPayload::new(GlowIcon {
                            path: path.clone(),
                            replay: Replay {
                                key: key.clone(),
                                serial,
                            },
                        }))
                    }
                    _ => Err(
                        "SmeltGlowIcon.new(path, key, serial) expects two non-empty strings and a number"
                            .into(),
                    ),
                },
            )])
            .with_documentation(
                "An icon in the running color that brightens once and settles back whenever `serial` is first seen or changes; not played while the user cannot see it or under reduced motion. Styles apply to the icon; its color is the animation's.",
            ),
    )?;
    Ok(())
}
