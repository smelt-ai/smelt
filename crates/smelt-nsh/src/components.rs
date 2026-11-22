//! 给插件的原生零件里 NSH 拼不出的几样：小人、随包带的线性图标、会拖窗口的标题栏那一条。
//!
//! 原生组件一律吃掉自己那块的指针（NSH 不按 `pointerEvents` 放行）：小人、图标这些只画的摆在能点
//! 的行里时，TSX 那头得在上面盖一层透明的 View，行才点得到。

use gpui::{
    AnyElement, App, Edges, EntityId, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, Window, div, rgb, svg,
};
use nsh_platform_gpui::{ComponentContext, NativeComponent};
use serde_json::Value;
use smelt_buddy::{Act, BuddyFigure};
use smelt_plugin_api as api;
use smelt_ui::motion::{ambient_application_active, ambient_motion_enabled};

pub(crate) const BUDDY: &str = "smelt.buddy";
pub(crate) const ICON: &str = "smelt.icon";
pub(crate) const WINDOW_BAR: &str = "smelt.window-bar";

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
}

impl Buddy {
    pub(crate) fn make(cx: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(Self {
            cx,
            seed: SharedString::default(),
            act: Act::Rest,
            since_ms: 0.,
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
        Some(div().flex().child(figure).into_any_element())
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, _: &mut Window, _: &mut App) {
        match name {
            "seed" => self.seed = text(value),
            "act" => match Act::from_name(&text(value)) {
                Some(act) => self.act = act,
                None => eprintln!("[nsh-pilot] {BUDDY} 没有这一出：{value:?}"),
            },
            "since" => {
                self.since_ms = value
                    .and_then(Value::as_f64)
                    .filter(|since| since.is_finite())
                    .unwrap_or(0.);
            }
            _ => return,
        }
        self.cx.redraw();
    }
}

pub(crate) struct Icon {
    cx: ComponentContext,
    path: SharedString,
    color: SharedString,
}

impl Icon {
    pub(crate) fn make(cx: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(Self {
            cx,
            path: SharedString::default(),
            color: SharedString::default(),
        })
    }
}

impl NativeComponent for Icon {
    fn render(&mut self, _: &mut Window, _: &mut App) -> Option<AnyElement> {
        let color = api::parse_hex_color(&self.color).ok()?;
        (!self.path.is_empty()).then(|| {
            svg()
                .path(self.path.clone())
                .size_full()
                .text_color(rgb(color))
                .into_any_element()
        })
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, _: &mut Window, _: &mut App) {
        match name {
            "path" => self.path = text(value),
            "color" => self.color = text(value),
            _ => return,
        }
        self.cx.redraw();
    }
}

/// `edges`：这一条所在的那块碰到面板的哪几条边，safe area 按它分。不贴标题栏时什么都不画。
pub(crate) struct WindowBar {
    cx: ComponentContext,
    pilot: EntityId,
    edges: Edges<bool>,
    height: Option<f32>,
}

impl WindowBar {
    pub(crate) fn make(cx: ComponentContext, pilot: EntityId) -> Box<dyn NativeComponent> {
        // 先报零高：不贴标题栏是常态，也免得没报尺寸时占着地方。
        cx.set_size(0., 0.);
        Box::new(Self {
            cx,
            pilot,
            edges: Edges::default(),
            height: None,
        })
    }
}

impl NativeComponent for WindowBar {
    fn render(&mut self, _: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let area = api::safe_area(self.pilot, cx).child(self.edges);
        let height = if area.title_bar {
            f32::from(smelt_ui::window_bar::TOP_BAR_HEIGHT)
        } else {
            0.
        };
        if self.height != Some(height) {
            self.height = Some(height);
            self.cx.set_size(0., height);
        }
        area.title_bar
            .then(|| smelt_ui::window_bar::window_bar("nsh-window-bar", &area).into_any_element())
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, _: &mut Window, _: &mut App) {
        if name == "edges" {
            let touches = |edge: &str| {
                value
                    .and_then(Value::as_array)
                    .is_some_and(|edges| edges.iter().any(|value| value == edge))
            };
            self.edges = Edges {
                top: touches("top"),
                right: touches("right"),
                bottom: touches("bottom"),
                left: touches("left"),
            };
            self.cx.redraw();
        }
    }
}
