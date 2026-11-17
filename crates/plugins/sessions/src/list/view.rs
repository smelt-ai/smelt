//! 会话列表视图：画 [`Sessions::sidebar`]，自己只留界面上的临时状态（行内改名、拖拽落点）。
//! 从访达拖进来的文件夹在本机打开成项目。

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, DragMoveEvent, Entity, ExternalPaths, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, Render, ScrollHandle,
    Styled as _, Subscription, Window, div, point, px,
};
use gpui_base::AutoScroll;
use gpui_base::input::{Escape, InputEvent, InputState};
use smelt_plugin_api::model::agent_status::AgentStatus;
use smelt_plugin_api::model::control_api::{WorkspaceRename, WorkspaceWorksetRename};
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceRenameParams, WorkspaceWorksetRenameParams,
};
use smelt_plugin_api::ui::motion::{ambient_application_active, ambient_motion_enabled};
use smelt_plugin_api::ui::scrollbar::{Scrollable, ScrollbarAxis};
use smelt_plugin_api::ui::ui_theme;
use smelt_plugin_api::ui::window_bar::window_bar;
use smelt_plugin_api::{self as api, PluginView, RuntimeKey, ViewHandle};

use super::actions;
use super::buddy::Stage;
use super::motion::ListMotion;
use super::rows::{self, ProjectDrag, RowCtx, SessionDrag};
use crate::contract::ListArgs;
use crate::model::{ConversationKey, Selection, Sessions};

/// 建会话列表视图（视图 [`crate::contract::LIST`]，`register` 登记）。只建一份。
pub(crate) fn build_view(_: &mut Window, cx: &mut App) -> Rc<dyn ViewHandle> {
    Rc::new(cx.new(ListView::new))
}

pub(super) struct ListView {
    pub(super) focus: FocusHandle,
    scroll: ScrollHandle,
    /// 正在行内改名的那一行。
    pub(super) rename: Option<Rename>,
    /// 拖着走时落点在哪（哪一行的上沿或下沿）。只在拖着时画；拖完、放下时清掉。
    pub(super) drop: Option<DropHint>,
    /// 拖着会话、项目停在列表上下边缘时自动滚，好拖到看不见的位置。
    auto_scroll: AutoScroll,
    /// 重排滑动、新会话落进列表、开始跑时图标亮一下。
    pub(super) motion: ListMotion,
    /// 行首小人各在演什么（干活、喊你、欢呼、打招呼）。
    buddies: Stage,
    _subscriptions: Vec<Subscription>,
}

/// 侧栏里所有会话（折起来的也算）和它们的状态，给小人比前后。
fn session_statuses(
    sidebar: &super::Sidebar,
) -> impl Iterator<Item = (&ConversationKey, AgentStatus)> {
    sidebar.session_rows().map(|row| (&row.key, row.status))
}

fn selected_session(selection: &Selection) -> Option<&ConversationKey> {
    match selection {
        Selection::Session(key) => Some(key),
        _ => None,
    }
}

/// 行内改名。
pub(super) struct Rename {
    pub(super) target: RenameTarget,
    /// 改之前的名字：没改就不发调用。
    original: String,
    pub(super) input: Entity<InputState>,
    _subscription: Subscription,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RenameTarget {
    Conversation(ConversationKey),
    Workset { machine: RuntimeKey, id: String },
}

/// 落点：放在 `target` 的前面（`before`）还是后面。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DropHint {
    pub(super) target: DropTarget,
    pub(super) before: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DropTarget {
    Project { machine: RuntimeKey, root: String },
    Session(ConversationKey),
}

impl ListView {
    fn new(cx: &mut Context<Self>) -> Self {
        let sessions = Sessions::global(cx);
        let motion = ListMotion::new(&sessions.read(cx).sidebar);
        let buddies = {
            let sessions = sessions.read(cx);
            Stage::new(
                session_statuses(&sessions.sidebar),
                selected_session(&sessions.selection),
                cx.background_executor().now(),
            )
        };
        let _subscriptions = vec![
            // 侧栏重算、选中变了都在 `Sessions` 上 notify。侧栏换了一份就比一下前后：重排、
            // 刚开始跑的会话、刚在这里新建的会话露面了，各起各的动画。
            cx.observe(&sessions, |this, sessions, cx| {
                this.sidebar_changed(&sessions, cx);
                cx.notify();
            }),
        ];
        Self {
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            rename: None,
            drop: None,
            auto_scroll: AutoScroll::default(),
            motion,
            buddies,
            _subscriptions,
        }
    }

    /// 开始行内改名：输入框换下标题，全选原名。回车、点别处保存，Esc 取消。
    pub(super) fn start_rename(
        &mut self,
        target: RenameTarget,
        original: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |state, cx| {
            state.set_value(original.as_str(), window, cx);
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        let _subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    this.commit_rename(cx);
                    window.focus(&this.focus, cx);
                }
                InputEvent::Blur => this.commit_rename(cx),
                InputEvent::Change | InputEvent::Focus => {}
            },
        );
        self.rename = Some(Rename {
            target,
            original,
            input,
            _subscription,
        });
        cx.notify();
    }

    pub(super) fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// 存下改名。先取走状态：回车之后输入框还会失焦，第二次进来时已经没有了，不会存两遍。
    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        cx.notify();
        let value = rename.input.read(cx).value().trim().to_string();
        if value == rename.original.trim() {
            return;
        }
        match rename.target {
            // 清空是不要自己起的名字，回到运行时算的名字。
            RenameTarget::Conversation(key) => {
                let params = WorkspaceRenameParams {
                    conversation_id: key.id,
                    terminal_id: None,
                    title: (!value.is_empty()).then_some(value),
                };
                let renamed =
                    api::call::<WorkspaceRename>(&key.machine, params, api::CALL_TIMEOUT, cx);
                crate::report_failure("改名", renamed, cx);
            }
            // 工作树一定要有名字。
            RenameTarget::Workset { machine, id } => {
                if value.is_empty() {
                    return;
                }
                let params = WorkspaceWorksetRenameParams {
                    workset_id: id,
                    task: value,
                };
                let renamed =
                    api::call::<WorkspaceWorksetRename>(&machine, params, api::CALL_TIMEOUT, cx);
                crate::report_failure("重命名工作树", renamed, cx);
            }
        }
    }

    /// `Sessions` 通知了，比一下侧栏前后。选中变了也会通知，那时侧栏没变，只有小人看选中
    /// （刚选中的闲着就打个招呼）。
    fn sidebar_changed(&mut self, sessions: &Entity<Sessions>, cx: &mut Context<Self>) {
        let sidebar = sessions.read(cx).sidebar.clone();
        let now = cx.background_executor().now();
        self.motion.track(&sidebar, now);
        self.buddies.track(
            session_statuses(&sidebar),
            selected_session(&sessions.read(cx).selection),
            now,
        );
        let fresh = sessions
            .read(cx)
            .fresh
            .clone()
            .filter(|key| sidebar.has_session(key));
        if let Some(key) = fresh {
            sessions.update(cx, |sessions, _| sessions.fresh = None);
            self.buddies.greet(&key, now);
            self.motion.arrive(key, now);
        }
    }

    /// 落点变了才重画。
    pub(super) fn set_drop(&mut self, drop: Option<DropHint>, cx: &mut Context<Self>) {
        if self.drop != drop {
            self.drop = drop;
            cx.notify();
        }
    }

    /// 拖着走时指针动了：贴着列表上下边缘就开始（或接着）自动滚，离开边缘就停。
    fn drag_scroll(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let bounds = self.scroll.bounds();
        let beside = position.x >= bounds.left() && position.x <= bounds.right();
        let delta = beside
            .then(|| AutoScroll::compute_delta(position.y, bounds))
            .flatten();
        // 轮询例外：指针停在边缘不动就没有事件，滚动只能按帧推进；周期 16ms，只在拖着且停在
        // 上下边缘时跑，离开边缘、松手（下一帧看到没在拖）就停。
        self.auto_scroll.set(delta, cx, |delta, this, cx| {
            if !cx.has_active_drag() {
                this.auto_scroll.stop();
                return;
            }
            let offset = this.scroll.offset();
            let max = f32::from(this.scroll.max_offset().y);
            let next = px((f32::from(offset.y) - f32::from(delta)).clamp(-max, 0.));
            if next != offset.y {
                this.scroll.set_offset(point(offset.x, next));
                cx.notify();
            }
        });
    }
}

impl PluginView for ListView {
    /// 参数见 [`ListArgs`]：`{"focus": true}` 是键盘焦点交给列表（命令 `FOCUS_LIST`）。
    fn open(&mut self, args: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        let args: ListArgs = serde_json::from_value(args.clone()).unwrap_or_default();
        if args.focus {
            window.focus(&self.focus, cx);
        }
    }

    fn focus_target(&self, _: &App) -> Option<FocusHandle> {
        Some(self.focus.clone())
    }
}

impl Render for ListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ambient = ambient_motion_enabled(
            ambient_application_active(cx),
            window.is_window_active(),
            true,
        );
        let now = cx.background_executor().now();
        let (motion, animating) = self.motion.frame(now, cx.reduce_motion(), ambient);
        if animating {
            window.request_animation_frame();
        }
        let sessions = Sessions::global(cx);
        let sessions = sessions.read(cx);
        let ctx = RowCtx {
            sidebar: sessions.sidebar.clone(),
            view: cx.entity(),
            selection: sessions.selection.clone(),
            drop: cx.has_active_drag().then(|| self.drop.clone()).flatten(),
            rename: self
                .rename
                .as_ref()
                .map(|rename| (rename.target.clone(), rename.input.clone())),
            now: chrono::Utc::now().timestamp().max(0) as u64,
            motion,
            buddies: self.buddies.frame(now, ambient && !cx.reduce_motion()),
        };
        let content = div()
            .flex()
            .flex_col()
            .pb_3()
            .children(rows::sidebar(&ctx, window, cx));
        // 画出来的行里有在动的小人才要下一帧；都站着、看不见时不重绘。
        if ctx.buddies.wants_frame() {
            window.request_animation_frame();
        }
        let area = api::safe_area(cx.entity_id(), cx);
        // 滚过了才出上沿那条细线，下面还有才出下沿那条，和上下摆着的东西分开。线一直占着
        // 1px，只换颜色，出现时不跳。
        let offset = self.scroll.offset().y;
        let edge = |shown: bool| {
            div()
                .flex_none()
                .h(px(1.))
                .when(shown, |line| line.bg(ui_theme::hair()))
        };
        div()
            .id("sessions-list")
            .key_context("SessionsList")
            .track_focus(&self.focus)
            // 改名时按 Esc：输入框自己用不上就往上传，在这里取消。
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.cancel_rename(window, cx)))
            // 从访达拖文件夹进会话列表 = 在本机打开成项目。只挂在列表上，不挂整个窗口：对话
            // 输入框要自己收文件。底色铺一层选区色（焦点色带透明度），表示「放这里」。
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(ui_theme::selection()))
            .on_drop(|paths: &ExternalPaths, _, cx| {
                actions::open_dropped_paths(paths.paths().to_vec(), cx)
            })
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<SessionDrag>, _, cx| {
                this.drag_scroll(event.event.position, cx)
            }))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ProjectDrag>, _, cx| {
                this.drag_scroll(event.event.position, cx)
            }))
            .size_full()
            .flex()
            .flex_col()
            .when(area.title_bar, |this| {
                this.child(window_bar("sessions-list-bar", &area))
            })
            .child(edge(offset < px(0.)))
            .child(
                div().flex_1().min_h_0().flex().flex_col().child(Scrollable::new(
                    "sessions-list-scroll",
                    content,
                    &self.scroll,
                    ScrollbarAxis::Vertical,
                )),
            )
            .child(edge(offset > -self.scroll.max_offset().y))
    }
}
