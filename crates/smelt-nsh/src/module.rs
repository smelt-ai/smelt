//! `smelt/pilot`：给试点面板的宿主模块，JS 里 `nativeModule("smelt/pilot")` 拿 `Smelt`。
//!
//! 模块函数跑在 NSH 的引擎线程，不能碰 GPUI（宿主的东西都在主线程）。所以两头各留一个
//! 通道，由主线程这头（[`serve`]）接：
//! - 登记的视图放在一个 `watch` 里，主线程在视图登记变了时换一份，`Smelt.on("views")`
//!   在引擎线程上等它变；
//! - `Smelt.openView` 把请求发到主线程，那头跑宿主的「打开视图」命令，结果原路回来。

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{LazyLock, OnceLock},
};

use gpui::App;
use nsh_core::prelude::*;
use smelt_plugin_api::{self as api, HostEvent, OpenView};
use tokio::sync::{mpsc, oneshot, watch};

/// 登记过的一个视图。
#[derive(Clone, Debug, PartialEq, JsObject)]
pub struct ViewEntry {
    pub id: String,
    pub title: String,
}

static VIEWS: LazyLock<watch::Sender<Vec<ViewEntry>>> =
    LazyLock::new(|| watch::Sender::new(Vec::new()));

/// 要主线程打开的视图，和回结果的地方。
struct OpenRequest {
    view: String,
    answer: oneshot::Sender<bool>,
}

static OPENS: OnceLock<mpsc::UnboundedSender<OpenRequest>> = OnceLock::new();

/// 主线程这头：现在的视图放进 `VIEWS`，登记一变再放；接 `openView` 的请求。只接一次。
pub(crate) fn serve(cx: &mut App) {
    let (sender, mut requests) = mpsc::unbounded_channel();
    if OPENS.set(sender).is_err() {
        return;
    }
    publish(cx);
    cx.subscribe(&api::events(cx), |_, event, cx| {
        if matches!(event, HostEvent::ViewsChanged) {
            publish(cx);
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Some(OpenRequest { view, answer }) = requests.recv().await {
            let opened = cx.update(|cx| {
                api::OPEN_VIEW.run(
                    &OpenView {
                        view,
                        args: serde_json::Value::Null,
                    },
                    cx,
                )
            });
            let _ = answer.send(opened.await.unwrap_or(false));
        }
    })
    .detach();
}

fn publish(cx: &App) {
    let views = api::views(cx)
        .into_iter()
        .filter(|view| view.settings.is_none() && !view.instances)
        .map(|view| ViewEntry {
            id: view.id.to_string(),
            title: view.title.get().to_string(),
        })
        .collect();
    VIEWS.send_replace(views);
}

#[module]
pub struct Smelt;

#[module]
impl Smelt {
    /// 在宿主里打开 `id`：经主线程跑宿主的打开视图命令。打不开是 false。
    pub async fn open_view(id: String) -> bool {
        let Some(opens) = OPENS.get() else {
            return false;
        };
        let (answer, answered) = oneshot::channel();
        if opens.send(OpenRequest { view: id, answer }).is_err() {
            return false;
        }
        answered.await.unwrap_or(false)
    }

    /// 现在登记的视图交给 `listener`，之后每变一次再交一次（事件只有 `views`），直到调返回的
    /// 函数或调用方没了。
    pub fn on(
        event: String,
        listener: Callback<Vec<ViewEntry>>,
        cx: CallCx<'_>,
    ) -> Result<Stop, String> {
        if event != "views" {
            return Err(format!("没有 Smelt 事件 `{event}`"));
        }
        let listener = Rc::new(RefCell::new(Some(listener)));
        let held = listener.clone();
        cx.start(async move {
            let mut views = VIEWS.subscribe();
            loop {
                let Some(listener) = held.borrow().clone() else {
                    return;
                };
                listener.post(views.borrow_and_update().clone());
                if views.changed().await.is_err() {
                    return;
                }
            }
        });
        Ok(Stop::new(move || drop(listener.take())))
    }
}

/// 交给 `set_ui_sets` 的那组。
pub(crate) fn registry() -> Registry {
    let mut registry = Registry::ui_set("smelt/pilot");
    registry.module::<Smelt>();
    registry
}
