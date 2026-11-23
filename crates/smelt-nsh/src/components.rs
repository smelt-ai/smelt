//! 给插件的原生零件里 NSH 拼不出的几样：小人角色、会拖窗口的标题栏带、毛玻璃材质坐标。
//!
//! 图标与按钮已全部走 NSH 一等原生节点（`smelt/host` 的 `<Icon>` 与 `nsh/base` 的 `<Button>`），
//! 不再绕 GPUI `NativeComponent`。

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, IntoElement as _, ParentElement as _, Pixels, SharedString,
    Styled as _, Window, canvas, div, px, size,
};
use nsh_platform_gpui::{ComponentContext, NativeComponent};
use serde_json::Value;
use smelt_buddy::{Act, BuddyFigure};
use smelt_ui::motion::{ambient_application_active, ambient_motion_enabled};
use smelt_ui::window_bar::{SafeArea, TOP_BAR_HEIGHT, with_window_drag};

pub(crate) const BUDDY: &str = "smelt.buddy";
pub(crate) const WINDOW_BAR: &str = "smelt.window-bar";
pub(crate) const WINDOW_DRAG: &str = "smelt.window-drag";
pub(crate) const BACKDROP_REGION: &str = "smelt.backdrop-region";

/// 主窗口材质分块的方位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackdropPlacement {
    Center,
    Left,
    Right,
    Bottom,
}

/// 记录单块 Dock 区域坐标的回调类型（`None` 表示该区域已收起或无面积）。
pub type BackdropRecordFn = Rc<dyn Fn(BackdropPlacement, Option<Bounds<Pixels>>, &mut App)>;

/// 主窗口材质分块回调：由宿主传入，每帧 `prepaint` 内同步写入窗口根的 `Regions`。
#[derive(Clone, Default)]
pub struct BackdropHooks {
    pub reset: Option<Rc<dyn Fn()>>,
    pub record: Option<BackdropRecordFn>,
}

fn text(value: Option<&Value>) -> SharedString {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
        .into()
}

pub(crate) struct Buddy {
    cx: ComponentContext,
    seed: SharedString,
    act: Act,
    since_ms: f64,
    opacity: f32,
}

impl Buddy {
    pub(crate) fn make(cx: ComponentContext) -> Box<dyn NativeComponent> {
        cx.set_size(smelt_buddy::SIZE, smelt_buddy::SIZE);
        Box::new(Self {
            cx,
            seed: SharedString::default(),
            act: Act::Rest,
            since_ms: 0.,
            opacity: 1.,
        })
    }
}

impl NativeComponent for Buddy {
    fn render(&mut self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let ambient = ambient_motion_enabled(
            ambient_application_active(cx),
            window.is_window_active(),
            true,
        );
        let figure = BuddyFigure::new("smelt-buddy", self.seed.clone(), self.act, self.since_ms)
            .still(!ambient || cx.reduce_motion());
        Some(
            div()
                .flex()
                .size_full()
                .items_center()
                .justify_center()
                .opacity(self.opacity)
                .child(figure)
                .into_any_element(),
        )
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, _: &mut Window, _: &mut App) {
        match name {
            "seed" => self.seed = text(value),
            "act" => match Act::from_name(&text(value)) {
                Some(act) => self.act = act,
                None => eprintln!("[smelt-nsh] {BUDDY} 没有这一出：{value:?}"),
            },
            "since" => {
                self.since_ms = value
                    .and_then(Value::as_f64)
                    .filter(|since| since.is_finite())
                    .unwrap_or(0.);
            }
            "opacity" => {
                self.opacity = value
                    .and_then(Value::as_f64)
                    .filter(|alpha| alpha.is_finite())
                    .map_or(1., |alpha| (alpha as f32).clamp(0., 1.));
            }
            _ => return,
        }
        self.cx.redraw();
    }
}

/// 标题栏安全区拖曳带：传 `safeArea`（`{ title_bar, top_left, top_right }`）或 `titleBar`（布尔）；
/// 不贴标题栏时什么都不画。
pub(crate) struct WindowBar {
    cx: ComponentContext,
    area: SafeArea,
    height: Option<f32>,
}

impl WindowBar {
    pub(crate) fn make(cx: ComponentContext) -> Box<dyn NativeComponent> {
        // 先报零高：不贴标题栏是常态，也免得没报尺寸时占着地方。
        cx.set_size(0., 0.);
        Box::new(Self {
            cx,
            area: SafeArea::default(),
            height: None,
        })
    }
}

impl NativeComponent for WindowBar {
    fn render(&mut self, _: &mut Window, _: &mut App) -> Option<AnyElement> {
        let height = if self.area.title_bar {
            f32::from(TOP_BAR_HEIGHT)
        } else {
            0.
        };
        if self.height != Some(height) {
            self.height = Some(height);
            self.cx.set_size(0., height);
        }
        self.area.title_bar.then(|| {
            smelt_ui::window_bar::window_bar("nsh-window-bar", &self.area).into_any_element()
        })
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, _: &mut Window, _: &mut App) {
        match name {
            "safeArea" => {
                self.area = value.and_then(parse_safe_area).unwrap_or_default();
                self.cx.redraw();
            }
            "titleBar" => {
                self.area.title_bar = value.and_then(Value::as_bool).unwrap_or(false);
                self.cx.redraw();
            }
            _ => {}
        }
    }
}

pub(crate) fn parse_safe_area(value: &Value) -> Option<SafeArea> {
    let obj = value.as_object()?;
    let title_bar = obj
        .get("title_bar")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let num = |key: &str| {
        obj.get(key)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.)
            .unwrap_or(0.) as f32
    };
    let mut area = SafeArea {
        title_bar,
        ..SafeArea::default()
    };
    let tl = num("top_left");
    let tr = num("top_right");
    let bl = num("bottom_left");
    let br = num("bottom_right");
    if tl > 0. {
        area.corners.top_left = size(px(tl), TOP_BAR_HEIGHT);
    }
    if tr > 0. {
        area.corners.top_right = size(px(tr), TOP_BAR_HEIGHT);
    }
    if bl > 0. {
        area.corners.bottom_left = size(px(bl), px(bl));
    }
    if br > 0. {
        area.corners.bottom_right = size(px(br), px(br));
    }
    Some(area)
}

/// 透明的原生拖窗口带：空白处拖窗口、双击缩放，走 `smelt_ui::window_bar::with_window_drag`。
pub(crate) struct WindowDrag;

impl WindowDrag {
    pub(crate) fn make(_cx: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(Self)
    }
}

impl NativeComponent for WindowDrag {
    fn render(&mut self, _: &mut Window, _: &mut App) -> Option<AnyElement> {
        Some(with_window_drag(div().size_full()).into_any_element())
    }

    fn set_prop(&mut self, _: &str, _: Option<&Value>, _: &mut Window, _: &mut App) {}
}

/// 记下某块 Dock 区域在当前帧 prepaint 后的窗口坐标，供 `window_backdrop::paint` 零帧延迟对齐毛玻璃。
pub(crate) struct BackdropRegion {
    cx: ComponentContext,
    hooks: Rc<RefCell<BackdropHooks>>,
    placement: Option<BackdropPlacement>,
}

impl BackdropRegion {
    pub(crate) fn make(
        cx: ComponentContext,
        hooks: Rc<RefCell<BackdropHooks>>,
    ) -> Box<dyn NativeComponent> {
        Box::new(Self {
            cx,
            hooks,
            placement: None,
        })
    }
}

impl NativeComponent for BackdropRegion {
    fn render(&mut self, _: &mut Window, _: &mut App) -> Option<AnyElement> {
        let placement = self.placement?;
        let record = self.hooks.borrow().record.clone();
        Some(
            canvas(
                move |bounds, _, cx| {
                    if let Some(record) = record.as_ref() {
                        let visible = (bounds.size.width > px(0.) && bounds.size.height > px(0.))
                            .then_some(bounds);
                        record(placement, visible, cx);
                    }
                },
                |_, _, _, _| {},
            )
            .size_full()
            .into_any_element(),
        )
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, _: &mut Window, _: &mut App) {
        if name == "placement" {
            self.placement = match value.and_then(Value::as_str) {
                Some("left") => Some(BackdropPlacement::Left),
                Some("right") => Some(BackdropPlacement::Right),
                Some("bottom") => Some(BackdropPlacement::Bottom),
                Some("center") => Some(BackdropPlacement::Center),
                _ => None,
            };
            self.cx.redraw();
        }
    }
}
