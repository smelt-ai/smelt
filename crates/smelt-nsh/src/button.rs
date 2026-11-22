//! `smelt.button`：smelt-ui 的按钮，登记成 NSH 的原生组件，JS 里 `nativeComponent` 摆。
//!
//! NSH 不量原生组件的大小，组件自己报（[`ComponentContext::set_size`]）：照同样的内容排一个
//! 不画的，量它的自然大小。JS 给了宽（`fill` 时占满）就按给的，字放不下截短。

use gpui::{
    AnyElement, App, AvailableSpace, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div, size,
};
use nsh_platform_gpui::{ComponentContext, NativeComponent};
use serde_json::{Value, json};
use smelt_plugin_api::RunCommand;

pub(crate) const NAME: &str = "smelt.button";

pub(crate) struct SmeltButton {
    cx: ComponentContext,
    label: SharedString,
    icon: Option<SharedString>,
    tooltip: Option<SharedString>,
    /// 悬停提示带上这条命令绑的键。
    command: Option<SharedString>,
    quiet: bool,
    small: bool,
    fill: bool,
    reported: (f32, f32),
}

impl SmeltButton {
    pub(crate) fn make(cx: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(Self {
            cx,
            label: SharedString::default(),
            icon: None,
            tooltip: None,
            command: None,
            quiet: false,
            small: false,
            fill: false,
            reported: (0., 0.),
        })
    }

    /// 样子：量的和画的是同一份。
    fn look(&self, id: &'static str) -> smelt_ui::Button {
        let mut button = smelt_ui::Button::new(id)
            .accessibility_label(self.label.clone())
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .child(smelt_ui::truncated_line::truncated_line(self.label.clone())),
            );
        if self.quiet {
            button = button.quiet();
        }
        if self.small {
            button = button.small();
        }
        if let Some(icon) = &self.icon {
            button = button.icon_path(icon.clone());
        }
        button
    }
}

impl NativeComponent for SmeltButton {
    fn render(&mut self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let natural = div()
            .flex()
            .child(self.look("nsh-smelt-button-size"))
            .into_any_element()
            .layout_as_root(
                size(AvailableSpace::MaxContent, AvailableSpace::MaxContent),
                window,
                cx,
            );
        let wanted = (
            f32::from(natural.width).ceil(),
            f32::from(natural.height).ceil(),
        );
        if wanted != self.reported {
            self.reported = wanted;
            self.cx.set_size(wanted.0, wanted.1);
        }
        let events = self.cx.clone();
        let mut button = self
            .look("nsh-smelt-button")
            .on_click(move |_, _, _| events.emit("press", json!({})));
        if self.fill {
            button = button.w_full().min_w_0();
        }
        if let Some(text) = self.tooltip.clone() {
            button = match &self.command {
                Some(command) => {
                    button.tooltip_with_action(text, &RunCommand::new(command.clone()), None)
                }
                None => button.tooltip(text),
            };
        }
        Some(
            div()
                .size_full()
                .flex()
                .items_center()
                .child(button)
                .into_any_element(),
        )
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, _: &mut Window, _: &mut App) {
        let text = || {
            value
                .and_then(Value::as_str)
                .map(|text| SharedString::from(text.to_owned()))
        };
        let flag = || value.and_then(Value::as_bool).unwrap_or(false);
        match name {
            "label" => self.label = text().unwrap_or_default(),
            "icon" => self.icon = text(),
            "tooltip" => self.tooltip = text(),
            "command" => self.command = text(),
            "quiet" => self.quiet = flag(),
            "small" => self.small = flag(),
            "fill" => self.fill = flag(),
            _ => return,
        }
        self.cx.redraw();
    }
}
