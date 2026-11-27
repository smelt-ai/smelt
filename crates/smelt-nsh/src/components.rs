//! 给插件的原生零件里 NSH 拼不出的几样：会拖窗口的标题栏带、毛玻璃材质坐标。
//!
//! 图标与按钮已全部走 NSH 一等原生节点（`smelt/host` 的 `<Icon>` 与 `nsh/base` 的 `<Button>`），
//! 不再绕 GPUI `NativeComponent`。

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, IntoElement as _, Pixels, Styled as _, Window, canvas, div, px, size,
};
use nsh_platform_gpui::{ComponentContext, NativeComponent};
use serde_json::Value;
use smelt_ui::window_bar::{SafeArea, TOP_BAR_HEIGHT, with_window_drag};

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
            height: Some(0.),
        })
    }

    fn sync_size(&mut self) {
        let height = if self.area.title_bar {
            f32::from(TOP_BAR_HEIGHT)
        } else {
            0.
        };
        if self.height != Some(height) {
            self.height = Some(height);
            self.cx.set_size(0., height);
        }
    }
}

impl NativeComponent for WindowBar {
    fn render(&mut self, _: &mut Window, _: &mut App) -> Option<AnyElement> {
        self.sync_size();
        self.area.title_bar.then(|| {
            smelt_ui::window_bar::window_bar("nsh-window-bar", &self.area).into_any_element()
        })
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, _: &mut Window, _: &mut App) {
        match name {
            "safeArea" => {
                self.area = value.and_then(parse_safe_area).unwrap_or_default();
                self.sync_size();
                self.cx.redraw();
            }
            "titleBar" => {
                self.area.title_bar = value.and_then(Value::as_bool).unwrap_or(false);
                self.sync_size();
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
