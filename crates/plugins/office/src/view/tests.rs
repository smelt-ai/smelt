use std::cell::RefCell;
use std::sync::Arc;

use api::model::daemon_state::{DaemonPhase, DaemonSessionState, RuntimePresence};
use api::model::runtime_path::PathStyle;
use api::model::workspace_directory::{WorkspaceAcp, WorkspaceConversation, WorkspaceDirectory};
use gpui::{
    App, AppContext as _, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, TestAppContext, point,
};

use super::*;

struct State {
    directory: Option<WorkspaceDirectory>,
    states: Vec<DaemonSessionState>,
    online: bool,
    shown: bool,
    present: bool,
    unavailable: Option<String>,
}

fn state() -> Rc<RefCell<State>> {
    Rc::new(RefCell::new(State {
        directory: Some(WorkspaceDirectory {
            conversations: vec![WorkspaceConversation {
                id: "conversation-1".into(),
                custom_title: Some("Implement office".into()),
                acp: Some(WorkspaceAcp {
                    sid: "runtime-1".into(),
                    agent: "codex".into(),
                    cwd: Some("/repo".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        }),
        states: Vec::new(),
        online: true,
        shown: false,
        present: true,
        unavailable: None,
    }))
}

struct Host(Rc<RefCell<State>>);

fn many_state(count: usize) -> Rc<RefCell<State>> {
    let state = state();
    let original = state.borrow().directory.as_ref().unwrap().conversations[0].clone();
    state.borrow_mut().directory.as_mut().unwrap().conversations = (0..count)
        .map(|index| {
            let mut conversation = original.clone();
            conversation.id = format!("conversation-{index}");
            conversation.acp.as_mut().unwrap().sid = format!("runtime-{index}");
            conversation
        })
        .collect();
    state
}

#[gpui::test]
fn hundred_sessions_only_construct_visible_workers(cx: &mut TestAppContext) {
    let state = many_state(100);
    cx.update(|cx| {
        gpui_base::init(cx);
        api::ui::init(cx);
        api::install(Host(state), cx);
    });
    let (view, window) = cx.add_window_view(|_, cx| Office::new(cx));
    window.update(|window, cx| window.draw(cx).clear(cx));
    window.run_until_parked();
    window.update(|window, cx| window.draw(cx).clear(cx));
    window.update(|_, cx| {
        let count = view.read(cx).built_workers.get();
        assert!(count > 0, "测试必须实际构造工位");
        assert!(count <= 32, "100 个会话不应全部构造，实际构造 {count} 个");
    });
}

impl api::Host for Host {
    fn machines(&self, _: &App) -> Vec<api::Machine> {
        if !self.0.borrow().present {
            return Vec::new();
        }
        vec![api::Machine {
            key: api::RuntimeKey::Remote("server".into()),
            name: "Server".into(),
            unavailable: self.0.borrow().unavailable.clone(),
        }]
    }
    fn client(&self, _: &api::RuntimeKey, _: &App) -> Option<api::client::RuntimeClient> {
        None
    }
    fn connector(&self, _: &api::RuntimeKey, _: &App) -> Option<Arc<dyn api::client::Connector>> {
        None
    }
    fn machine_online(&self, _: &api::RuntimeKey, _: &App) -> bool {
        self.0.borrow().online
    }
    fn connections(&self, _: &App) -> api::Connections {
        Default::default()
    }
    fn view_shown(&self, id: &str, _: &App) -> bool {
        id == VIEW && self.0.borrow().shown
    }
    fn view_laid_out(&self, _: &str, _: &App) -> bool {
        false
    }
    fn main_window(&self, _: &App) -> Option<gpui::AnyWindowHandle> {
        None
    }
    fn window_active(&self, _: &App) -> bool {
        true
    }
    fn overlay_open(&self, _: &App) -> bool {
        false
    }
    fn path_style(&self, _: &api::RuntimeKey, _: &App) -> PathStyle {
        PathStyle::Posix
    }
    fn session_states(&self, _: &api::RuntimeKey, _: &App) -> Vec<DaemonSessionState> {
        self.0.borrow().states.clone()
    }
    fn session_presence(&self, _: &api::RuntimeKey, _: &str, _: &App) -> Option<RuntimePresence> {
        None
    }
    fn workspace_directory(&self, _: &api::RuntimeKey, _: &App) -> Option<WorkspaceDirectory> {
        self.0.borrow().directory.clone()
    }
    fn notification_permission(&self, _: &App) -> api::NotificationPermission {
        api::NotificationPermission::Unsupported
    }
}

#[gpui::test]
fn status_visibility_disconnect_and_removal_follow_host_events(cx: &mut TestAppContext) {
    let state = state();
    let view = cx.update(|cx| {
        api::install(Host(state.clone()), cx);
        cx.new(Office::new)
    });
    cx.update(|cx| assert_eq!(view.read(cx).rooms[0].workers[0].activity, Activity::Idle));
    state.borrow_mut().states = vec![DaemonSessionState {
        id: "runtime-1".into(),
        phase: DaemonPhase::Thinking,
        turn_events: true,
        ..Default::default()
    }];
    cx.update(|cx| api::emit(HostEvent::SessionsChanged, cx));
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).rooms[0].workers[0].activity,
            Activity::Thinking
        )
    });
    state.borrow_mut().shown = true;
    cx.update(|cx| api::emit(HostEvent::ShownViewsChanged, cx));
    cx.update(|cx| assert!(view.read(cx).visible));
    state.borrow_mut().online = false;
    cx.update(|cx| api::emit(HostEvent::MachinesChanged, cx));
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).rooms[0].workers[0].activity,
            Activity::Offline
        )
    });
    state.borrow_mut().present = false;
    cx.update(|cx| api::emit(HostEvent::MachinesChanged, cx));
    cx.update(|cx| assert!(view.read(cx).rooms.is_empty()));
}

#[gpui::test]
fn initial_sync_and_archiving_do_not_leave_phantom_characters(cx: &mut TestAppContext) {
    let state = state();
    let directory = state.borrow_mut().directory.take();
    let view = cx.update(|cx| {
        api::install(Host(state.clone()), cx);
        cx.new(Office::new)
    });
    cx.update(|cx| {
        assert!(view.read(cx).rooms.is_empty());
        assert_eq!(view.read(cx).notices.len(), 1);
    });
    state.borrow_mut().unavailable = Some("Offline".into());
    cx.update(|cx| api::emit(HostEvent::MachinesChanged, cx));
    cx.update(|cx| {
        assert!(view.read(cx).notices[0].contains("Server"));
        assert!(view.read(cx).notices[0].contains("Offline"));
    });
    state.borrow_mut().unavailable = None;
    state.borrow_mut().directory = directory;
    cx.update(|cx| api::emit(HostEvent::SessionsChanged, cx));
    cx.update(|cx| {
        assert_eq!(view.read(cx).rooms[0].workers.len(), 1);
        assert!(view.read(cx).notices.is_empty());
    });
    state.borrow_mut().directory.as_mut().unwrap().conversations[0].archived_at = Some(1);
    cx.update(|cx| api::emit(HostEvent::SessionsChanged, cx));
    cx.update(|cx| assert!(view.read(cx).rooms.is_empty()));
}

#[gpui::test]
fn pointer_and_keyboard_open_the_existing_remote_conversation(cx: &mut TestAppContext) {
    let state = state();
    let opened = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        gpui_base::init(cx);
        api::ui::init(cx);
        api::install(Host(state), cx);
        let opened = opened.clone();
        api::register_commands(
            "smelt.app",
            "App",
            vec![api::OPEN_VIEW.spec("Open", move |args, _, _| {
                opened.borrow_mut().push(args);
                Ok(true)
            })],
            cx,
        )
        .unwrap();
    });
    let (_, window) = cx.add_window_view(|_, cx| Office::new(cx));
    window.update(|window, cx| window.draw(cx).clear(cx));
    window.simulate_click(point(px(110.), px(120.)), Modifiers::default());
    window.run_until_parked();
    assert_eq!(opened.borrow().len(), 1, "点击角色必须触发跳转");
    window.update(|window, cx| {
        window.focus_next(cx);
        window.draw(cx).clear(cx);
    });
    let keystroke =
        window.update(|_, cx| Keystroke::parse_with_os("enter", cx.platform_info().os).unwrap());
    window.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    window.simulate_event(KeyUpEvent { keystroke });
    window.run_until_parked();
    let opened = opened.borrow();
    assert_eq!(opened.len(), 2, "键盘也必须能打开角色");
    for request in opened.iter() {
        assert_eq!(request.view, "smelt.sessions/session");
        assert_eq!(
            request.args,
            api::serde_json::json!({"machine":"server", "conversation_id":"conversation-1"})
        );
    }
}

#[gpui::test]
fn missing_session_view_reports_navigation_failure(cx: &mut TestAppContext) {
    let view = cx.update(|cx| {
        api::install(Host(state()), cx);
        cx.new(Office::new)
    });
    view.update(cx, |office, cx| {
        office.activate(
            &api::RuntimeKey::Remote("server".into()),
            "conversation-1",
            cx,
        )
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(view.read(cx).error.is_some());
        assert_eq!(view.read(cx).rooms[0].workers.len(), 1);
    });
}

#[gpui::test]
fn scrolling_resizing_and_status_updates_keep_the_last_conversation_clickable(
    cx: &mut TestAppContext,
) {
    let state = many_state(100);
    let opened = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        gpui_base::init(cx);
        api::ui::init(cx);
        api::install(Host(state.clone()), cx);
        let opened = opened.clone();
        api::register_commands(
            "smelt.app",
            "App",
            vec![api::OPEN_VIEW.spec("Open", move |args, _, _| {
                opened.borrow_mut().push(args);
                Ok(true)
            })],
            cx,
        )
        .unwrap();
    });
    let (view, window) = cx.add_window_view(|_, cx| Office::new(cx));
    window.update(|window, cx| window.draw(cx).clear(cx));
    window.run_until_parked();
    window.update(|window, cx| {
        view.update(cx, |office, cx| {
            office.list.scroll_to_end();
            cx.notify();
        });
        window.draw(cx).clear(cx);
    });
    window.run_until_parked();
    let bounds = window
        .debug_bounds("office-worker-conversation-99")
        .expect("最后的工位必须可见");
    window.simulate_click(bounds.center(), Modifiers::default());
    window.run_until_parked();
    assert_eq!(
        opened.borrow().last().unwrap().args["conversation_id"],
        "conversation-99"
    );
    let anchor = window.update(|_, cx| view.read(cx).list.logical_scroll_top());
    state.borrow_mut().states = vec![DaemonSessionState {
        id: "runtime-99".into(),
        phase: DaemonPhase::Thinking,
        turn_events: true,
        ..Default::default()
    }];
    window.update(|window, cx| {
        api::emit(HostEvent::SessionsChanged, cx);
        window.draw(cx).clear(cx);
    });
    window.run_until_parked();
    window.update(|_, cx| {
        assert_eq!(
            view.read(cx).list.logical_scroll_top().item_ix,
            anchor.item_ix,
            "状态变化不能把列表拉回顶部"
        )
    });
    window.simulate_resize(gpui::size(px(380.), px(600.)));
    window.run_until_parked();
    window.update(|window, cx| {
        view.update(cx, |office, cx| {
            office.list.scroll_to_end();
            cx.notify();
        });
        window.draw(cx).clear(cx);
    });
    window.run_until_parked();
    window.update(|_, cx| assert_eq!(view.read(cx).columns, 1));
    let bounds = window
        .debug_bounds("office-worker-conversation-99")
        .expect("窄窗口也能滚到最后一个工位");
    assert!(bounds.right() <= px(380.));
    window.simulate_click(bounds.center(), Modifiers::default());
    window.run_until_parked();
    assert_eq!(opened.borrow().len(), 2);
    assert_eq!(
        opened.borrow().last().unwrap().args["conversation_id"],
        "conversation-99"
    );
    let assets = window.update(|_, cx| view.read(cx).assets.clone());
    window.update(|window, cx| {
        window.set_rem_size(px(24.));
        window.refresh();
        window.draw(cx).clear(cx);
    });
    window.run_until_parked();
    window.update(|window, cx| {
        view.update(cx, |office, cx| {
            office.list.scroll_to_end();
            cx.notify();
        });
        window.draw(cx).clear(cx);
    });
    window.run_until_parked();
    let enlarged = window
        .debug_bounds("office-worker-conversation-99")
        .unwrap();
    assert!(
        enlarged.size.height > bounds.size.height,
        "字号变大后必须重测行高"
    );
    window.update(|_, cx| {
        assert!(
            Rc::ptr_eq(&assets, &view.read(cx).assets),
            "状态、窗口和字号变化不能重建静态纹理"
        )
    });
    state.borrow_mut().directory.as_mut().unwrap().conversations[99].archived_at = Some(1);
    window.update(|window, cx| {
        api::emit(HostEvent::SessionsChanged, cx);
        window.draw(cx).clear(cx);
    });
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("office-worker-conversation-99")
            .is_none(),
        "归档后不能保留旧的点击目标"
    );
}
