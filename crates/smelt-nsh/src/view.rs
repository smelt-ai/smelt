//! `smelt.view`：宿主登记的视图（一格终端、一段对话……）作 NSH 的原生组件嵌进来。方案 2 里
//! 重组件都这样进 NSH 写的界面。
//!
//! JS 给 `spec = { view, args, reuseKey? }`：按 id 从视图注册表建一份，`open(args)`，交出它的 GPUI 视图。
//! NSH 每帧都 render 原生组件，交的是视图，GPUI Fast 就按视图缓存，视图自己没变不重画。
//! `spec` 变了整份重建；尺寸由 NSH 按容器给，不用报。
//!
//! 支持：
//! - `safeArea`：可选显式安全区 JSON（不传则按视图 id 从主壳安全区表查），写入 `api::set_safe_area`；
//! - `focusSeq`：递增时把键盘焦点交给内部视图（用于切分屏/跳转会话）；
//! - `onFocusIn`：焦点进入该视图或其任意子焦点（点击或键盘 Tab）时通知 JS；
//! - `spec.reuseKey`：可选实例复用键；当分屏树重构导致同一格在同帧内卸载又挂载时无缝接管活体 `ViewInstance`，
//!   不重连、不丢终端屏与输入草稿；若本帧未被认领则在下一事件轮次立即释放；
//! - `ends`：这一格是摆它的一方新开的终端会话，组件放掉时让本机守护结束它，不留没人看的会话。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{
    AnyElement, AnyView, App, AppContext as _, AsyncApp, Context, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, StyleRefinement,
    Styled as _, Subscription, Window, div,
};
use nsh_platform_gpui::{ComponentContext, NativeComponent};
use serde_json::{Value, json};
use smelt_plugin_api::client::DEFAULT_REQUEST_TIMEOUT;
use smelt_plugin_api::client::protocol::daemon::DaemonOperation;
use smelt_plugin_api::{self as api, Runtime, ViewHandle};
use smelt_ui::window_bar::SafeArea;

pub(crate) const NAME: &str = "smelt.view";

pub(crate) type SafeAreaLookup = Rc<dyn Fn(&str) -> SafeArea>;

static REUSE_SEQ: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static REUSE_POOL: RefCell<HashMap<String, (u64, Entity<ViewHost>)>> = RefCell::new(HashMap::new());
}

fn park_host(key: String, host: Entity<ViewHost>, app: Option<&AsyncApp>) {
    let token = REUSE_SEQ.fetch_add(1, Ordering::Relaxed);
    REUSE_POOL.with(|pool| {
        pool.borrow_mut().insert(key.clone(), (token, host));
    });
    if let Some(app) = app {
        app.spawn(async move |cx| {
            cx.update(|cx| {
                cx.defer(move |_| {
                    REUSE_POOL.with(|pool| {
                        let mut pool = pool.borrow_mut();
                        if pool.get(&key).is_some_and(|(t, _)| *t == token) {
                            pool.remove(&key);
                        }
                    });
                });
            });
        })
        .detach();
    } else {
        REUSE_POOL.with(|pool| {
            pool.borrow_mut().remove(&key);
        });
    }
}

fn claim_host(key: &str) -> Option<Entity<ViewHost>> {
    REUSE_POOL.with(|pool| pool.borrow_mut().remove(key).map(|(_, host)| host))
}

/// 包裹宿主视图实例或单例句柄的 GPUI 容器：负责焦点跟踪（`onFocusIn` 事件）和点击空白转交焦点，
/// 避免直接挂在外部时丢失子焦点（如对话里的提问输入栏）。
struct ViewHost {
    ctx: Rc<RefCell<ComponentContext>>,
    focus: FocusHandle,
    handle: Option<Rc<dyn ViewHandle>>,
    instance: Option<api::ViewInstance>,
    _focus_sub: Subscription,
}

impl ViewHost {
    fn new(ctx: ComponentContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ctx = Rc::new(RefCell::new(ctx));
        let emit_ctx = ctx.clone();
        let focus = cx.focus_handle();
        let focus_sub = cx.on_focus_in(&focus, window, move |_, _, _| {
            emit_ctx.borrow().emit("focusIn", json!({}));
        });
        Self {
            ctx,
            focus,
            handle: None,
            instance: None,
            _focus_sub: focus_sub,
        }
    }

    fn rebind_ctx(&self, next: ComponentContext) {
        *self.ctx.borrow_mut() = next;
    }

    fn has_view(&self) -> bool {
        self.instance.is_some() || self.handle.is_some()
    }

    fn any_view(&self) -> Option<AnyView> {
        if let Some(instance) = &self.instance {
            return Some(instance.view());
        }
        self.handle.as_ref().map(|handle| handle.view())
    }

    fn focus_target(&self, cx: &App) -> Option<FocusHandle> {
        if let Some(instance) = &self.instance {
            return instance
                .focus_handle(cx)
                .or_else(|| Some(self.focus.clone()));
        }
        if let Some(handle) = &self.handle {
            return handle.focus_target(cx).or_else(|| Some(self.focus.clone()));
        }
        None
    }

    fn focus_now(&self, window: &mut Window, cx: &mut App) {
        if let Some(instance) = &self.instance {
            if !instance.focus(window, cx) {
                window.focus(&self.focus, cx);
            }
            return;
        }
        if let Some(handle) = &self.handle {
            if let Some(target) = handle.focus_target(cx) {
                window.focus(&target, cx);
            } else {
                window.focus(&self.focus, cx);
            }
            return;
        }
        window.focus(&self.focus, cx);
    }
}

impl Render for ViewHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self
            .any_view()
            .map(|view| view.cached(StyleRefinement::default().size_full()));
        div()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !window.default_prevented() {
                        this.focus_now(window, cx);
                        window.prevent_default();
                    }
                }),
            )
            .children(body)
    }
}

pub(crate) struct SmeltView {
    cx: ComponentContext,
    async_app: Option<AsyncApp>,
    safe_area: Option<SafeAreaLookup>,
    explicit_safe_area: Option<SafeArea>,
    /// 上次建视图用的 `spec`，同样的再来不重建。
    spec: Option<Value>,
    /// 当前持有的视图 id（用于单例视图热更新参数而不重建实体）。
    view_id: Option<String>,
    reuse_key: Option<String>,
    host: Option<Entity<ViewHost>>,
    focus: Option<FocusHandle>,
    focus_seq: u64,
    pending_focus: bool,
    ends: Option<String>,
}

impl SmeltView {
    pub(crate) fn make(
        cx: ComponentContext,
        safe_area: Option<SafeAreaLookup>,
    ) -> Box<dyn NativeComponent> {
        Box::new(Self {
            cx,
            async_app: None,
            safe_area,
            explicit_safe_area: None,
            spec: None,
            view_id: None,
            reuse_key: None,
            host: None,
            focus: None,
            focus_seq: 0,
            pending_focus: false,
            ends: None,
        })
    }

    fn ensure_host(&mut self, window: &mut Window, cx: &mut App) -> Entity<ViewHost> {
        if let Some(host) = &self.host {
            return host.clone();
        }
        let ctx = self.cx.clone();
        let host = cx.new(|cx| ViewHost::new(ctx, window, cx));
        self.host = Some(host.clone());
        host
    }

    fn apply_safe_area(&self, cx: &mut App) {
        let area = self.explicit_safe_area.or_else(|| {
            self.view_id
                .as_deref()
                .and_then(|id| self.safe_area.as_ref().map(|lookup| lookup(id)))
        });
        let (Some(area), Some(host)) = (area, &self.host) else {
            return;
        };
        if let Some(view) = host.read(cx).any_view() {
            api::set_safe_area(&view, area, cx);
        }
    }

    fn clear_view(&mut self, cx: &mut App) {
        self.view_id = None;
        self.reuse_key = None;
        self.focus = None;
        if let Some(host) = &self.host {
            host.update(cx, |host, cx| {
                host.handle = None;
                host.instance = None;
                cx.notify();
            });
        }
        self.cx.redraw();
    }

    fn build(&mut self, spec: Option<&Value>, window: &mut Window, cx: &mut App) {
        if self.async_app.is_none() {
            self.async_app = Some(cx.to_async());
        }
        if self.spec.as_ref() == spec {
            return;
        }
        self.spec = spec.cloned();
        let Some(spec) = spec else {
            self.clear_view(cx);
            return;
        };
        let Some(id) = spec.get("view").and_then(Value::as_str) else {
            eprintln!("[smelt-nsh] smelt.view 的 spec 没有 view：{spec}");
            return;
        };
        let args = spec.get("args").unwrap_or(&Value::Null);
        let reuse_key = spec
            .get("reuseKey")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty())
            .map(str::to_owned);
        let Some(info) = api::view(id, cx) else {
            self.clear_view(cx);
            return;
        };
        if info.instances {
            self.view_id = Some(id.to_owned());
            self.reuse_key = reuse_key.clone();
            if let Some(key) = reuse_key.as_deref()
                && let Some(reclaimed) = claim_host(key)
            {
                reclaimed.read(cx).rebind_ctx(self.cx.clone());
                self.host = Some(reclaimed.clone());
                self.focus = reclaimed.read(cx).focus_target(cx);
                self.apply_safe_area(cx);
                if self.pending_focus {
                    self.pending_focus = false;
                    reclaimed.update(cx, |host, cx| host.focus_now(window, cx));
                }
                self.cx.redraw();
                return;
            }
            let host = self.ensure_host(window, cx);
            match api::create_view(id, args, window, cx) {
                Ok(instance) => {
                    host.update(cx, |host, cx| {
                        host.handle = None;
                        host.instance = Some(instance);
                        cx.notify();
                    });
                    self.focus = host.read(cx).focus_target(cx);
                    self.apply_safe_area(cx);
                    if self.pending_focus {
                        self.pending_focus = false;
                        host.update(cx, |host, cx| host.focus_now(window, cx));
                    }
                }
                Err(error) => {
                    eprintln!("[smelt-nsh] 建视图实例 {id} 失败：{error}");
                    host.update(cx, |host, cx| {
                        host.handle = None;
                        host.instance = None;
                        cx.notify();
                    });
                    self.focus = None;
                }
            }
            self.cx.redraw();
            return;
        }

        // 单例视图走 `api::ensure_view`：同一视图只建一份，Rust 插件内部的 `api::built_view` 和
        // `api::set_safe_area` 才能找到它；参数或 `seq` 变了就原地再 `open` 一次。
        self.reuse_key = None;
        let host = self.ensure_host(window, cx);
        if let Some((handle, built)) = api::ensure_view(id, window, cx) {
            self.view_id = Some(id.to_owned());
            host.update(cx, |host, cx| {
                host.instance = None;
                host.handle = Some(handle.clone());
                cx.notify();
            });
            self.apply_safe_area(cx);
            if built || spec.get("seq").is_some() || !args.is_null() {
                handle.open(args, window, cx);
            }
            self.focus = host.read(cx).focus_target(cx);
            if self.pending_focus {
                self.pending_focus = false;
                host.update(cx, |host, cx| host.focus_now(window, cx));
            }
        } else {
            self.clear_view(cx);
            return;
        }
        self.cx.redraw();
    }
}

impl NativeComponent for SmeltView {
    fn render(&mut self, _: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let host = self.host.as_ref()?;
        host.read(cx)
            .has_view()
            .then(|| host.clone().into_any_element())
    }

    fn focus_handle(&self) -> Option<FocusHandle> {
        self.focus.clone()
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, window: &mut Window, cx: &mut App) {
        if self.async_app.is_none() {
            self.async_app = Some(cx.to_async());
        }
        match name {
            "spec" => self.build(value, window, cx),
            "safeArea" => {
                self.explicit_safe_area = value.and_then(crate::components::parse_safe_area);
                self.apply_safe_area(cx);
            }
            "focusSeq" => {
                let seq = value.and_then(Value::as_u64).unwrap_or(0);
                if seq > self.focus_seq {
                    self.focus_seq = seq;
                    if let Some(host) = &self.host
                        && host.read(cx).has_view()
                    {
                        host.update(cx, |host, cx| host.focus_now(window, cx));
                    } else {
                        self.pending_focus = true;
                    }
                }
            }
            "ends" => self.ends = value.and_then(Value::as_str).map(str::to_owned),
            _ => {}
        }
    }
}

impl Drop for SmeltView {
    fn drop(&mut self) {
        // 带 `reuseKey` 的实例先暂存一帧：分屏树同帧重构时新节点无缝认领，否则一帧后释放。
        if let (Some(key), Some(host)) = (self.reuse_key.take(), self.host.take())
            && self.ends.is_none()
        {
            park_host(key, host, self.async_app.as_ref());
            return;
        }
        // 先放掉视图（断开画面），再结束会话，免得终端看到会话没了去重连。
        self.host = None;
        let Some(id) = self.ends.take() else {
            return;
        };
        // 请求是阻塞的，丢进专用阻塞池，不占主线程或 GPUI 后台池；结果没人等。
        api::blocking::spawn_detached(move || {
            let _ = Runtime::local().client().request(
                &json!({ "op": DaemonOperation::Kill, "id": id }),
                DEFAULT_REQUEST_TIMEOUT,
            );
        });
    }
}
