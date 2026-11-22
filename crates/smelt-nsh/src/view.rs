//! `smelt.view`：宿主登记的视图（一格终端、一段对话……）作 NSH 的原生组件嵌进来。方案 2 里
//! 重组件都这样进 NSH 写的界面。
//!
//! JS 给 `spec = { view, args }`：按 id 从视图注册表建一份，`open(args)`，交出它的 GPUI 视图。
//! NSH 每帧都 render 原生组件，交的是视图，GPUI Fast 就按视图缓存，视图自己没变不重画。
//! `spec` 变了整份重建；尺寸由 NSH 按容器给，不用报。
//!
//! `ends`：这一格是摆它的一方新开的终端会话（试点里「开终端」那格），组件放掉时让本机守护
//! 结束它，不留没人看的会话。

use std::rc::Rc;

use gpui::{AnyElement, App, FocusHandle, IntoElement as _, Window};
use nsh_platform_gpui::{ComponentContext, NativeComponent};
use serde_json::{Value, json};
use smelt_plugin_api::client::DEFAULT_REQUEST_TIMEOUT;
use smelt_plugin_api::client::protocol::daemon::DaemonOperation;
use smelt_plugin_api::{self as api, Runtime, ViewHandle};

pub(crate) const NAME: &str = "smelt.view";

pub(crate) struct SmeltView {
    cx: ComponentContext,
    /// 上次建视图用的 `spec`，同样的再来不重建。
    spec: Option<Value>,
    handle: Option<Rc<dyn ViewHandle>>,
    focus: Option<FocusHandle>,
    ends: Option<String>,
}

impl SmeltView {
    pub(crate) fn make(cx: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(Self {
            cx,
            spec: None,
            handle: None,
            focus: None,
            ends: None,
        })
    }

    fn build(&mut self, spec: Option<&Value>, window: &mut Window, cx: &mut App) {
        if self.spec.as_ref() == spec {
            return;
        }
        self.spec = spec.cloned();
        self.handle = None;
        self.focus = None;
        self.cx.redraw();
        let Some(spec) = spec else {
            return;
        };
        let Some(id) = spec.get("view").and_then(Value::as_str) else {
            eprintln!("[nsh-pilot] smelt.view 的 spec 没有 view：{spec}");
            return;
        };
        let Some(info) = api::views(cx).into_iter().find(|info| info.id == id) else {
            eprintln!("[nsh-pilot] 没有登记视图 {id}");
            return;
        };
        let handle = (info.build)(window, cx);
        handle.open(spec.get("args").unwrap_or(&Value::Null), window, cx);
        self.focus = handle.focus_target(cx);
        self.handle = Some(handle);
    }
}

impl NativeComponent for SmeltView {
    fn render(&mut self, _: &mut Window, _: &mut App) -> Option<AnyElement> {
        self.handle
            .as_ref()
            .map(|handle| handle.view().into_any_element())
    }

    fn focus_handle(&self) -> Option<FocusHandle> {
        self.focus.clone()
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, window: &mut Window, cx: &mut App) {
        match name {
            "spec" => self.build(value, window, cx),
            "ends" => self.ends = value.and_then(Value::as_str).map(str::to_owned),
            _ => {}
        }
    }
}

impl Drop for SmeltView {
    fn drop(&mut self) {
        // 先放掉视图（断开画面），再结束会话，免得终端看到会话没了去重连。
        self.handle = None;
        let Some(id) = self.ends.take() else {
            return;
        };
        // 请求是阻塞的，不占主线程；结果没人等，失败了会话留着，侧栏看不到但守护里还在。
        std::thread::spawn(move || {
            let _ = Runtime::local().client().request(
                &json!({ "op": DaemonOperation::Kill, "id": id }),
                DEFAULT_REQUEST_TIMEOUT,
            );
        });
    }
}
