//! 给 JS 插件的 `smelt-plugin-api` 模块：本 crate 接口的薄映射，不另造一套。
//!
//! 宿主给每个插件建一份 [`JsPlugin`]，把它的 [`JsPlugin::module`] 装进那个插件自己的
//! gpui-shell Policy：模块里的函数由此知道是哪个插件在调。挂载成功后宿主调
//! [`JsPlugin::attach`] 交回插件视图。
//!
//! gpui-shell 的宿主不能回调脚本，宿主事件做成脚本 await 的 `next_event()`：每个插件
//! 一条队列，从挂载完起开始记。插件视图释放时队列和它登记的页面一起撤掉，所以插件卸载、
//! 重新加载不用宿主另外收拾。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use futures_channel::oneshot;
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, WeakEntity, Window, div, rgb,
};
use gpui_component::v_flex;
use gpui_shell::{HostArguments, HostError, HostModule, HostObject, HostValue, ScriptView};
use smelt_ui::ui_theme;

use crate::{HostEvent, Page, PageBuilder, PageHandle, PageSpec, PathRequest, RuntimeKey};

/// 模块名，和 Rust 这边的 crate 同名。
pub const MODULE: &str = "smelt-plugin-api";

const DECLARATIONS: &str = r#"
/** 一台机器：本机是 null，远程机器是它的 runtime id。 */
export type Machine = string | null;
export interface PageOptions {
  /** 路由里记的 id。"agents"、"automations"、"desktop"、"session" 是宿主的，不能用。 */
  id: string;
  title: string;
  /** lucide 图标名，如 "sparkles"。 */
  icon?: string;
  /** 进侧栏「智能体」那一组。 */
  sidebar?: boolean;
}
/**
 * 把这个插件登记成工作台的一页，通常在 View 的 init 里调。同 id 的页面被替换。
 * 登记了页面的插件画在舞台上，宿主给它页头（标题）和底色，不再占工具面板的 tab。
 */
export function register_page(options: PageOptions): null;
/** 工作台的当前项目，只算这台电脑上的；远程项目或没有项目时为 null。 */
export function active_project(): string | null;
export interface PathRequest {
  /** 选择框标题。 */
  title: string;
  /** 确认按钮上的动词，如「导入」。 */
  confirm: string;
  /** 除了目录还能选文件。 */
  files?: boolean;
  multiple?: boolean;
}
/** 在那台机器上选路径。取消时为 null。 */
export function pick_paths(machine: Machine, request: PathRequest): Promise<string[] | null>;
export type HostEvent =
  | { type: "pi_config_changed"; machine: Machine }
  | { type: "reconnected"; machine: Machine }
  | { type: "machines_changed" }
  | { type: "active_project_changed" }
  | { type: "pages_changed" };
/** 等宿主的下一条事件。从插件挂载完起开始记，没取走的同样一条只留一份。同时只能有一个在等。 */
export function next_event(): Promise<HostEvent>;
"#;

/// 一个 JS 插件在宿主这边的身份。
#[derive(Clone, Default)]
pub struct JsPlugin(Rc<RefCell<PluginState>>);

#[derive(Default)]
struct PluginState {
    /// 挂载成功后才有。
    view: Option<WeakEntity<ScriptView>>,
    /// 挂载完之前（init 里）登记的页面，挂载完再交给注册表。
    pending: Vec<PageOptions>,
    /// 登记了的页面和当时的建视图函数：撤之前比一下，已经被别人替换的不撤。
    pages: Vec<(SharedString, PageBuilder)>,
    queue: VecDeque<HostEvent>,
    waiter: Option<oneshot::Sender<HostEvent>>,
    _events: Option<Subscription>,
    _release: Option<Subscription>,
}

#[derive(Clone)]
struct PageOptions {
    id: SharedString,
    title: SharedString,
    icon: SharedString,
    sidebar: bool,
}

impl JsPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// 这个插件的 `smelt-plugin-api` 模块，装进它自己的 Policy。
    pub fn module(&self) -> HostModule {
        let pages = self.clone();
        let events = self.clone();
        HostModule::new(MODULE)
            .declarations(DECLARATIONS)
            .function("register_page", move |arguments| {
                pages.register_page(page_options(arguments)?)
            })
            .function("active_project", |_| {
                let project = in_app("active_project", |cx| crate::host(cx).active_project(cx))?;
                Ok(project
                    .map(|path| path.to_string_lossy().into_owned())
                    .into())
            })
            .async_function("pick_paths", pick_paths)
            .async_function("next_event", move |_| Ok(events.next_event()))
    }

    /// 挂载成功后调：init 里登记的页面现在登记，开始记宿主事件，视图释放时撤掉这些。
    pub fn attach(&self, view: &Entity<ScriptView>, cx: &mut App) {
        let state = Rc::downgrade(&self.0);
        let release = cx.observe_release(view, move |_, cx| {
            if let Some(state) = state.upgrade() {
                JsPlugin(state).detach(cx);
            }
        });
        let state = Rc::downgrade(&self.0);
        let host_events = crate::events(cx);
        let events = cx.subscribe(&host_events, move |_, event: &HostEvent, _| {
            if let Some(state) = state.upgrade() {
                JsPlugin(state).deliver(event);
            }
        });
        let pending = {
            let mut state = self.0.borrow_mut();
            state.view = Some(view.downgrade());
            state._release = Some(release);
            state._events = Some(events);
            std::mem::take(&mut state.pending)
        };
        for options in pending {
            if let Err(error) = self.register(options, cx) {
                eprintln!("[plugins] {error}");
            }
        }
    }

    /// 登记过页面：它画在舞台上，宿主不再把它放进别处。
    pub fn is_page(&self) -> bool {
        let state = self.0.borrow();
        !state.pages.is_empty() || !state.pending.is_empty()
    }

    fn register_page(&self, options: PageOptions) -> Result<HostValue, HostError> {
        if self.0.borrow().view.is_none() {
            self.0.borrow_mut().pending.push(options);
            return Ok(HostValue::Null);
        }
        in_app("register_page", |cx| self.register(options, cx))?.map_err(HostError::new)?;
        Ok(HostValue::Null)
    }

    fn register(&self, options: PageOptions, cx: &mut App) -> Result<(), String> {
        let Some(view) = self.0.borrow().view.clone() else {
            return Err("插件还没挂载好".to_string());
        };
        let title = options.title.clone();
        let build: PageBuilder = Rc::new(move |_window, cx| {
            let page = ScriptPage {
                title: title.clone(),
                view: view.clone(),
            };
            Rc::new(cx.new(|_| page)) as Rc<dyn PageHandle>
        });
        crate::register_page(
            PageSpec {
                id: options.id.clone(),
                title: options.title,
                icon: options.icon,
                sidebar: options.sidebar,
                machine_menu: false,
                build: build.clone(),
            },
            cx,
        )?;
        let mut state = self.0.borrow_mut();
        state.pages.retain(|(id, _)| *id != options.id);
        state.pages.push((options.id, build));
        Ok(())
    }

    fn next_event(&self) -> impl Future<Output = Result<HostValue, HostError>> + Send + use<> {
        let (sender, receiver) = oneshot::channel();
        let mut state = self.0.borrow_mut();
        match state.queue.pop_front() {
            Some(event) => {
                let _ = sender.send(event);
            }
            // 上一个还在等的被顶掉，它那边收到 `Canceled`。
            None => state.waiter = Some(sender),
        }
        async move {
            receiver
                .await
                .map(|event| event_value(&event))
                .map_err(|_| HostError::new("next_event：被后一次 next_event 顶掉了"))
        }
    }

    fn deliver(&self, event: &HostEvent) {
        let mut state = self.0.borrow_mut();
        let event = match state.waiter.take() {
            Some(waiter) => match waiter.send(event.clone()) {
                Ok(()) => return,
                // 等的那个 Promise 已经没人要了，照常排队。
                Err(event) => event,
            },
            None => event.clone(),
        };
        if !state.queue.contains(&event) {
            state.queue.push_back(event);
        }
    }

    /// 视图释放了：扔掉队列和订阅，撤下还没被别人替换的页面。
    fn detach(&self, cx: &mut App) {
        let pages = {
            let mut state = self.0.borrow_mut();
            state.queue.clear();
            state.waiter = None;
            state._events = None;
            std::mem::take(&mut state.pages)
        };
        for (id, build) in pages {
            if crate::page(&id, cx).is_some_and(|spec| Rc::ptr_eq(&spec.build, &build)) {
                crate::unregister_page(&id, cx);
            }
        }
    }
}

fn in_app<R>(name: &str, f: impl FnOnce(&mut App) -> R) -> Result<R, HostError> {
    gpui_shell::with_current_app(f)
        .ok_or_else(|| HostError::new(format!("{name} 只能在视图的事件、任务或 init 里调用")))
}

// ---------------------------------------------------------------- 页面

/// JS 页面：宿主画页头（和 Rust 页面同一个底板）和舞台底色，脚本视图画在下面。
/// 脚本视图归加载它的宿主，这里只拿弱引用；它释放时这一页也随之撤下。
struct ScriptPage {
    title: SharedString,
    view: WeakEntity<ScriptView>,
}

impl Render for ScriptPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = crate::host(cx).page_header(window, cx).child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rgb(ui_theme::text_bright()))
                .child(self.title.clone()),
        );
        v_flex()
            .size_full()
            .bg(rgb(ui_theme::bg_stage()))
            .child(header)
            .child(div().flex_1().min_h_0().children(self.view.upgrade()))
    }
}

impl Page for ScriptPage {
    // 打开参数还没映射给脚本：JS 页面眼下不进机器菜单，没人带参数打开它。
    fn open(&mut self, _args: &serde_json::Value, _window: &mut Window, _cx: &mut Context<Self>) {}
}

fn page_options(arguments: &HostArguments) -> Result<PageOptions, HostError> {
    let options = arguments.value(0)?;
    let text = |name: &str| -> Result<SharedString, HostError> {
        options
            .get(name)
            .and_then(HostValue::as_str)
            .map(|text| SharedString::from(text.to_owned()))
            .ok_or_else(|| HostError::new(format!("register_page：{name} 要是字符串")))
    };
    let icon = match options.get("icon").filter(|icon| !icon.is_null()) {
        Some(_) => format!("icons/{}.svg", text("icon")?),
        None => "icons/puzzle.svg".to_string(),
    };
    Ok(PageOptions {
        id: text("id")?,
        title: text("title")?,
        icon: icon.into(),
        sidebar: options
            .get("sidebar")
            .and_then(HostValue::as_bool)
            .unwrap_or(false),
    })
}

// ---------------------------------------------------------------- 选路径

fn pick_paths(
    arguments: &HostArguments,
) -> Result<impl Future<Output = Result<HostValue, HostError>> + Send + use<>, HostError> {
    let machine = match arguments.value(0)? {
        HostValue::Null => RuntimeKey::Local,
        HostValue::Str(id) => RuntimeKey::Remote(id.clone()),
        other => {
            return Err(HostError::new(format!(
                "pick_paths：机器要是 null 或字符串，收到 {}",
                other.describe()
            )));
        }
    };
    let request = arguments.value(1)?;
    let text = |name: &str| {
        request
            .get(name)
            .and_then(HostValue::as_str)
            .map(str::to_owned)
            .ok_or_else(|| HostError::new(format!("pick_paths：{name} 要是字符串")))
    };
    let flag = |name: &str| {
        request
            .get(name)
            .and_then(HostValue::as_bool)
            .unwrap_or(false)
    };
    let request = PathRequest {
        title: text("title")?,
        confirm: text("confirm")?,
        files: flag("files"),
        multiple: flag("multiple"),
    };
    let (sender, receiver) = oneshot::channel::<Vec<String>>();
    in_app("pick_paths", |cx| {
        let host = crate::host(cx);
        // 脚本可能正嵌在宿主别的更新里调它；选择框要进工作台，推到这一轮之后。
        cx.defer(move |cx| {
            let done = Box::new(move |paths: Vec<String>, _: &mut App| {
                let _ = sender.send(paths);
            });
            host.pick_paths(&machine, request, done, cx);
        });
    })?;
    // 取消了宿主不调 `done`，它被扔掉，这边就收到 `Canceled`。
    Ok(async move {
        Ok(match receiver.await {
            Ok(paths) => paths.into(),
            Err(oneshot::Canceled) => HostValue::Null,
        })
    })
}

// ---------------------------------------------------------------- 事件

fn event_value(event: &HostEvent) -> HostValue {
    let machine = |key: &RuntimeKey| match key {
        RuntimeKey::Local => HostValue::Null,
        RuntimeKey::Remote(id) => HostValue::Str(id.clone()),
    };
    let object = match event {
        HostEvent::PiConfigChanged(key) => HostObject::new()
            .field("type", "pi_config_changed")
            .field("machine", machine(key)),
        HostEvent::Reconnected(key) => HostObject::new()
            .field("type", "reconnected")
            .field("machine", machine(key)),
        HostEvent::MachinesChanged => HostObject::new().field("type", "machines_changed"),
        HostEvent::ActiveProjectChanged => {
            HostObject::new().field("type", "active_project_changed")
        }
        HostEvent::PagesChanged => HostObject::new().field("type", "pages_changed"),
    };
    object.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_declarations_match_its_functions() {
        JsPlugin::new()
            .module()
            .validate()
            .expect("声明和函数对得上");
    }

    #[test]
    fn queued_events_are_not_duplicated() {
        let plugin = JsPlugin::new();
        plugin.deliver(&HostEvent::ActiveProjectChanged);
        plugin.deliver(&HostEvent::ActiveProjectChanged);
        plugin.deliver(&HostEvent::MachinesChanged);
        assert_eq!(
            plugin.0.borrow().queue,
            [HostEvent::ActiveProjectChanged, HostEvent::MachinesChanged]
        );
    }

    #[test]
    fn a_waiting_next_event_takes_the_event_directly() {
        let plugin = JsPlugin::new();
        let waiting = plugin.next_event();
        plugin.deliver(&HostEvent::PagesChanged);
        assert!(plugin.0.borrow().queue.is_empty());
        let value = smol::block_on(waiting).unwrap();
        assert_eq!(
            value.get("type").and_then(HostValue::as_str),
            Some("pages_changed")
        );
    }

    #[test]
    fn a_queued_event_is_handed_out_first() {
        let plugin = JsPlugin::new();
        plugin.deliver(&HostEvent::MachinesChanged);
        let value = smol::block_on(plugin.next_event()).unwrap();
        assert_eq!(
            value.get("type").and_then(HostValue::as_str),
            Some("machines_changed")
        );
        assert!(plugin.0.borrow().queue.is_empty());
    }
}
