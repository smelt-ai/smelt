//! 给 JS 插件的 `smelt-plugin-api` 模块：本 crate 接口的薄映射，不另造一套。
//!
//! 宿主给每个插件建一份 [`JsPlugin`]，把它的 [`JsPlugin::module`] 装进那个插件自己的
//! gpui-shell Policy：模块里的函数由此知道是哪个插件在调。挂载成功后宿主调
//! [`JsPlugin::attach`] 交回插件视图。
//!
//! gpui-shell 的宿主不能回调脚本，宿主事件做成脚本 await 的 `next_event()`：每个插件
//! 一条队列，从挂载完起开始记。插件视图释放时队列一起撤掉，所以插件卸载、重新加载不用
//! 宿主另外收拾。
//!
//! 插件摆在哪不由插件定，这里没有登记位置的函数（见 `docs/app-plugins.md`）。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use futures_channel::oneshot;
use gpui::{App, Entity, Subscription};
use gpui_shell::{HostArguments, HostError, HostModule, HostObject, HostValue, ScriptView};

use crate::{HostEvent, PathRequest, RuntimeKey};

/// 模块名，和 Rust 这边的 crate 同名。
pub const MODULE: &str = "smelt-plugin-api";

const DECLARATIONS: &str = r#"
/** 一台机器：本机是 null，远程机器是它的 runtime id。 */
export type Machine = string | null;
/** 工作台的当前项目，只算这台电脑上的；远程项目或没有项目时为 null。 */
export function active_project(): string | null;
/** 发送系统通知；显示受系统通知授权和设置影响。 */
export function notify(subtitle: string, body: string): null;
/** 替换本插件由脚本登记的主题，每项是 gpui-kit ThemeSet JSON。空数组撤下全部。 */
export function register_themes(themeSets: string[]): null;
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
  | { type: "runtime_topic"; machine: Machine; topic: string }
  | { type: "reconnected"; machine: Machine }
  | { type: "machines_changed" }
  | { type: "active_project_changed" }
  | { type: "views_changed" }
  | { type: "themes_changed" };
/** 等宿主的下一条事件。从插件挂载完起开始记，没取走的同样一条只留一份。同时只能有一个在等。 */
export function next_event(): Promise<HostEvent>;
"#;

/// 一个 JS 插件在宿主这边的身份。
#[derive(Clone, Default)]
pub struct JsPlugin(Rc<RefCell<PluginState>>);

#[derive(Default)]
struct PluginState {
    theme_owner: String,
    queue: VecDeque<HostEvent>,
    waiter: Option<oneshot::Sender<HostEvent>>,
    _events: Option<Subscription>,
    _release: Option<Subscription>,
}

impl JsPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// 每个插件的脚本主题有独立的来源，不能替换其他插件或内置主题。
    pub fn for_plugin(id: &str) -> Self {
        let plugin = Self::default();
        plugin.0.borrow_mut().theme_owner = format!("script:{id}");
        plugin
    }

    /// 这个插件的 `smelt-plugin-api` 模块，装进它自己的 Policy。
    pub fn module(&self) -> HostModule {
        let events = self.clone();
        let theme_owner = self.0.borrow().theme_owner.clone();
        HostModule::new(MODULE)
            .declarations(DECLARATIONS)
            .function("active_project", |_| {
                let project = in_app("active_project", |cx| crate::host(cx).active_project(cx))?;
                Ok(project
                    .map(|path| path.to_string_lossy().into_owned())
                    .into())
            })
            .function("notify", |arguments| {
                let subtitle = arguments.string(0)?;
                let body = arguments.string(1)?;
                in_app("notify", |cx| crate::host(cx).notify(subtitle, body))?;
                Ok(HostValue::Null)
            })
            .function("register_themes", move |arguments| {
                let values = arguments
                    .value(0)?
                    .as_array()
                    .ok_or_else(|| HostError::new("主题集必须是字符串数组"))?;
                let sources = values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(|| HostError::new("主题集必须是字符串数组"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if theme_owner.is_empty() {
                    return Err(HostError::new("插件没有主题来源身份"));
                }
                in_app("register_themes", |cx| {
                    crate::register_theme_sets(&theme_owner, &sources, cx)
                })?
                .map_err(|error| HostError::new(error.to_string()))?;
                Ok(HostValue::Null)
            })
            .async_function("pick_paths", pick_paths)
            .async_function("next_event", move |_| Ok(events.next_event()))
    }

    /// 挂载成功后调：开始记宿主事件，视图释放时撤掉。
    pub fn attach(&self, view: &Entity<ScriptView>, cx: &mut App) {
        let state = Rc::downgrade(&self.0);
        let release = cx.observe_release(view, move |_, _| {
            if let Some(state) = state.upgrade() {
                JsPlugin(state).detach();
            }
        });
        let state = Rc::downgrade(&self.0);
        let host_events = crate::events(cx);
        let events = cx.subscribe(&host_events, move |_, event: &HostEvent, _| {
            if let Some(state) = state.upgrade() {
                JsPlugin(state).deliver(event);
            }
        });
        let mut state = self.0.borrow_mut();
        state._release = Some(release);
        state._events = Some(events);
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

    /// 视图释放了：扔掉队列和订阅。
    fn detach(&self) {
        let mut state = self.0.borrow_mut();
        state.queue.clear();
        state.waiter = None;
        state._events = None;
    }
}

fn in_app<R>(name: &str, f: impl FnOnce(&mut App) -> R) -> Result<R, HostError> {
    gpui_shell::with_current_app(f)
        .ok_or_else(|| HostError::new(format!("{name} 只能在视图的事件、任务或 init 里调用")))
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
        HostEvent::RuntimeTopic {
            machine: key,
            topic,
        } => HostObject::new()
            .field("type", "runtime_topic")
            .field("machine", machine(key))
            .field("topic", topic.as_str()),
        HostEvent::Reconnected(key) => HostObject::new()
            .field("type", "reconnected")
            .field("machine", machine(key)),
        HostEvent::MachinesChanged => HostObject::new().field("type", "machines_changed"),
        HostEvent::ActiveProjectChanged => {
            HostObject::new().field("type", "active_project_changed")
        }
        HostEvent::ViewsChanged => HostObject::new().field("type", "views_changed"),
        HostEvent::ThemesChanged => HostObject::new().field("type", "themes_changed"),
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
    fn notification_requires_two_strings_and_a_script_context() {
        let module = JsPlugin::new().module();
        let invalid_arguments = [
            HostArguments::default(),
            HostArguments::new([HostValue::Null, HostValue::from("正文")]),
            HostArguments::new([HostValue::from("副标题")]),
            HostArguments::new([HostValue::from("副标题"), HostValue::Null]),
        ];
        for arguments in invalid_arguments {
            assert!(module.call("notify", &arguments).is_err());
        }
        let outside_script = module
            .call(
                "notify",
                &HostArguments::new([HostValue::from("副标题"), HostValue::from("正文")]),
            )
            .unwrap_err();
        assert!(outside_script.message().contains("只能在视图"));
    }

    #[test]
    fn theme_registration_rejects_invalid_arguments_before_entering_app() {
        let module = JsPlugin::for_plugin("example").module();
        assert!(
            module
                .call("register_themes", &HostArguments::default())
                .is_err()
        );
        assert!(
            module
                .call("register_themes", &HostArguments::new([HostValue::Null]))
                .is_err()
        );
        assert!(
            module
                .call(
                    "register_themes",
                    &HostArguments::new([HostValue::Array(vec![HostValue::Null])])
                )
                .is_err()
        );
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
        plugin.deliver(&HostEvent::ViewsChanged);
        assert!(plugin.0.borrow().queue.is_empty());
        let value = smol::block_on(waiting).unwrap();
        assert_eq!(
            value.get("type").and_then(HostValue::as_str),
            Some("views_changed")
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
