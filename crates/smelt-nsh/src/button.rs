//! `smelt.button`：smelt-ui 的按钮，登记成 NSH 的原生组件，JS 里 `nativeComponent` 摆。
//!
//! NSH 不量原生组件的大小，组件自己量好报上去（[`ComponentContext::set_size`]）。按钮是
//! 小号（24 高、两边 12），字按 smelt-ui 小号按钮的字阶量。

use gpui::{
    AnyElement, App, IntoElement, ParentElement as _, SharedString, Styled as _, TextRun, Window,
    div, px,
};
use nsh_platform_gpui::{ComponentContext, NativeComponent};
use serde_json::{Value, json};
use smelt_ui::kit::TYPE;

pub(crate) const NAME: &str = "smelt.button";

const HEIGHT: f32 = 24.;
const PADDING_X: f32 = 12.;

pub(crate) struct SmeltButton {
    cx: ComponentContext,
    label: SharedString,
    reported: (f32, f32),
}

impl SmeltButton {
    pub(crate) fn make(cx: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(Self {
            cx,
            label: SharedString::default(),
            reported: (0., 0.),
        })
    }
}

impl NativeComponent for SmeltButton {
    fn render(&mut self, window: &mut Window, _cx: &mut App) -> Option<AnyElement> {
        let style = TYPE.ui_medium;
        let mut font = window.text_style().font();
        font.weight = style.weight;
        let run = TextRun {
            len: self.label.len(),
            font,
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line =
            window
                .text_system()
                .shape_line(self.label.clone(), px(style.size), &[run], None);
        let wanted = ((f32::from(line.width) + 2. * PADDING_X).ceil(), HEIGHT);
        if wanted != self.reported {
            self.reported = wanted;
            self.cx.set_size(wanted.0, wanted.1);
        }
        let events = self.cx.clone();
        Some(
            div()
                .size_full()
                .child(
                    smelt_ui::Button::new("nsh-smelt-button")
                        .small()
                        .label(self.label.clone())
                        .on_click(move |_, _, _| events.emit("press", json!({}))),
                )
                .into_any_element(),
        )
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, _: &mut Window, _: &mut App) {
        if name == "label" {
            self.label = value
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
                .into();
            self.cx.redraw();
        }
    }
}
