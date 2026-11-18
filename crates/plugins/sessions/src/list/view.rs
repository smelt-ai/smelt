//! 会话列表视图：画 [`Sessions::sidebar`]，自己只留界面上的临时状态（行内改名、项目搜索框、
//! 拖拽落点）和填进行首插槽的头像实例。从访达拖进来的文件夹在本机打开成项目。

use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyView, App, AppContext as _, Context, DragMoveEvent, Entity, ExternalPaths, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, Render, ScrollHandle,
    SharedString, Styled as _, Subscription, Window, div, point, px,
};
use gpui_base::AutoScroll;
use gpui_base::input::{Escape, InputEvent, InputState};
use smelt_plugin_api::model::control_api::{WorkspaceRename, WorkspaceWorksetRename};
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceRenameParams, WorkspaceWorksetRenameParams,
};
use smelt_plugin_api::ui::motion::{ambient_application_active, ambient_motion_enabled};
use smelt_plugin_api::ui::scrollbar::{Scrollable, ScrollbarAxis};
use smelt_plugin_api::ui::window_bar::window_bar;
use smelt_plugin_api::ui::{ScrollAnchors, ui_theme};
use smelt_plugin_api::{self as api, HostEvent, PluginView, RuntimeKey, ViewHandle, ViewInstance};

use super::actions;
use super::motion::ListMotion;
use super::offscreen;
use super::rows::{self, ProjectDrag, RowCtx, SessionDrag};
use super::session_flip;
use crate::contract::ListArgs;
use crate::contract::{SESSION_AVATAR, SessionAvatarArgs};
use crate::model::{ConversationKey, Sessions};

/// 建会话列表视图（视图 [`crate::contract::LIST`]，`register` 登记）。只建一份。
pub(crate) fn build_view(window: &mut Window, cx: &mut App) -> Rc<dyn ViewHandle> {
    Rc::new(cx.new(|cx| {
        let mut view = ListView::new(window, cx);
        view.sync_avatars(window, cx);
        view
    }))
}

pub(super) struct ListView {
    pub(super) focus: FocusHandle,
    scroll: ScrollHandle,
    /// 正在行内改名的那一行。
    pub(super) rename: Option<Rename>,
    /// 「项目」标题换成的搜索框；没在搜是 `None`。框里的字是 [`Sessions::project_filter`]
    /// 的来源，边打边写过去。
    pub(super) search: Option<Search>,
    /// 拖着走时落点在哪（哪一行的上沿或下沿）。只在拖着时画；拖完、放下时清掉。
    pub(super) drop: Option<DropHint>,
    /// 拖着时拿起的是哪一项（`Sidebar::flip_order` 里的身份），原处画淡。拖拽牌子放掉时清掉。
    pub(super) lifted: Option<String>,
    /// 拖着会话、项目停在列表上下边缘时自动滚，好拖到看不见的位置。
    auto_scroll: AutoScroll,
    /// 重排滑动、新会话落进列表、开始跑时图标亮一下。
    pub(super) motion: ListMotion,
    /// 行首头像用的视图：填 [`SESSION_AVATAR`] 插槽的里按用户的选择挑一个
    /// （`model::AvatarChoice`）；没人填、选了不要是 `None`。
    avatar_view: Option<SharedString>,
    /// 侧栏里每段会话（折起来的也算）行首的头像，是 [`Self::avatar_view`] 的实例。
    avatars: HashMap<ConversationKey, ViewInstance>,
    /// [`Self::avatars`] 画出来用的那份，变了才换；没人填插槽是 `None`，行首画图标。
    avatar_views: Option<Rc<HashMap<ConversationKey, AnyView>>>,
    /// 列表里各行画在哪：可视区外的提示（[`offscreen`]）问它，选中变了让它把那行滚出来。
    anchors: ScrollAnchors,
    /// 上次看到的选中：变了就让新选中的那行露出来（从胶囊、通知、命令跳过来时它可能在可视区外）。
    selected: Option<ConversationKey>,
    _subscriptions: Vec<Subscription>,
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

/// 「项目」标题上的搜索框。
pub(super) struct Search {
    pub(super) input: Entity<InputState>,
    /// 上次写进 [`Sessions::project_filter`] 的字。那边和它对不上就是别处清掉了搜索。
    pushed: String,
    _subscription: Subscription,
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
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sessions = Sessions::global(cx);
        let motion = ListMotion::new(&sessions.read(cx).sidebar);
        // 启动时恢复的选中也露出来：列表从顶上开始，它可能在下面。
        let selected = sessions.read(cx).selection.session().cloned();
        let scroll = ScrollHandle::new();
        let anchors = ScrollAnchors::new(&scroll);
        anchors.reveal(selected.as_ref().map(|key| session_flip(key).into()));
        let _subscriptions = vec![
            // 侧栏重算、选中变了都在 `Sessions` 上 notify。侧栏换了一份就比一下前后：重排、
            // 刚开始跑的会话、刚在这里新建的会话露面了，各起各的动画；会话来了、走了，头像
            // 跟着建、放。
            cx.observe_in(&sessions, window, |this, sessions, window, cx| {
                this.sidebar_changed(&sessions, window, cx);
                cx.notify();
            }),
            // 填行首插槽的视图来了、换了、撤下了：建齐头像，或都放掉改画图标。
            cx.subscribe_in(
                &api::events(cx),
                window,
                |this, _, event: &HostEvent, window, cx| {
                    if matches!(event, HostEvent::ViewsChanged) {
                        this.sync_avatars(window, cx);
                    }
                },
            ),
        ];
        // 列表是被 `REVEAL_IN_LIST` 打开才建的：要找的会话这时才取得到。
        let mut this = Self {
            focus: cx.focus_handle(),
            scroll,
            rename: None,
            search: None,
            drop: None,
            lifted: None,
            auto_scroll: AutoScroll::default(),
            motion,
            avatar_view: None,
            avatars: HashMap::new(),
            avatar_views: None,
            anchors,
            selected,
            _subscriptions,
        };
        this.take_located(&sessions, cx);
        this
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
                crate::report_failure(t!("action.rename"), renamed, cx);
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
                crate::report_failure(t!("action.rename_workset"), renamed, cx);
            }
        }
    }

    /// 点「项目」标题上的放大镜：标题换成搜索框并拿到焦点，边打边按名字搜项目。已经在搜就
    /// 只把焦点放回去。Esc、清空后点别处收起（[`Self::close_search`]）。
    pub(super) fn start_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.search {
            search.input.update(cx, |state, cx| state.focus(window, cx));
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("list.search_projects").into_owned())
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        let _subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let query = input.read(cx).value().to_string();
                    if let Some(search) = &mut this.search {
                        search.pushed.clone_from(&query);
                    }
                    Sessions::global(cx)
                        .update(cx, |sessions, cx| sessions.set_project_filter(query, cx));
                }
                // 回车：焦点交回列表，搜索留着，好用方向键在搜到的会话里走。
                InputEvent::PressEnter { .. } => window.focus(&this.focus, cx),
                // 没字了还点别处就收起（焦点留在点到的地方）；有字留着，标题一直是搜索框，看得出
                // 列表是滤过的。
                InputEvent::Blur if input.read(cx).value().is_empty() => {
                    this.close_search(cx);
                }
                InputEvent::Blur | InputEvent::Focus => {}
            },
        );
        self.search = Some(Search {
            input,
            pushed: String::new(),
            _subscription,
        });
        cx.notify();
    }

    /// 收起搜索框，回到「项目」标题，列表回到不搜。返回原来有没有在搜。
    fn close_search(&mut self, cx: &mut Context<Self>) -> bool {
        if self.search.take().is_none() {
            return false;
        }
        Sessions::global(cx).update(cx, |sessions, cx| {
            sessions.set_project_filter(String::new(), cx)
        });
        cx.notify();
        true
    }

    /// Esc：在改名就取消改名，在搜就收起搜索，焦点交回列表。
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.is_some() {
            self.cancel_rename(window, cx);
        } else if self.close_search(cx) {
            window.focus(&self.focus, cx);
        }
    }

    /// `Sessions` 通知了，比一下侧栏前后。选中变了也会通知，那时侧栏没变。
    fn sidebar_changed(
        &mut self,
        sessions: &Entity<Sessions>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 搜索在别处被清掉了（从通知跳到滤掉的项目里的会话，见 `Sessions::reveal`）：框也收起。
        let filter = &sessions.read(cx).project_filter;
        if self
            .search
            .as_ref()
            .is_some_and(|search| search.pushed != *filter)
        {
            self.search = None;
        }
        let sidebar = sessions.read(cx).sidebar.clone();
        let selected = sessions.read(cx).selection.session().cloned();
        if selected != self.selected {
            let reveal = selected.as_ref().map(|key| session_flip(key).into());
            self.anchors.reveal(reveal);
            self.selected = selected;
        }
        let now = cx.background_executor().now();
        self.motion.track(&sidebar, now);
        let fresh = sessions
            .read(cx)
            .fresh
            .clone()
            .filter(|key| sidebar.has_session(key));
        if let Some(key) = fresh {
            sessions.update(cx, |sessions, _| sessions.fresh = None);
            self.motion.arrive(key, now);
        }
        self.take_located(sessions, cx);
        self.sync_avatars(window, cx);
    }

    /// 有要找出来的会话（`REVEAL_IN_LIST`）就取走：滚过去、闪一下。
    fn take_located(&mut self, sessions: &Entity<Sessions>, cx: &mut Context<Self>) {
        let Some(key) = sessions.update(cx, |sessions, _| sessions.located.take()) else {
            return;
        };
        self.anchors.reveal(Some(session_flip(&key).into()));
        self.motion.locate(key, cx.background_executor().now());
    }

    /// 侧栏里每段会话备一个行首头像，没了的放掉。用的视图换了（填插槽的变了、用户换了选择）
    /// 就全换；没人填、选了不要就都放掉，行首画图标。
    fn sync_avatars(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let filled: Vec<SharedString> = api::slot_views(SESSION_AVATAR, cx)
            .into_iter()
            .map(|view| view.id)
            .collect();
        let filled = Sessions::global(cx).read(cx).avatar.pick(&filled).cloned();
        if filled != self.avatar_view {
            self.avatar_view = filled;
            self.avatars.clear();
            self.avatar_views = None;
            cx.notify();
        }
        let Some(view_id) = self.avatar_view.clone() else {
            return;
        };
        let sidebar = Sessions::global(cx).read(cx).sidebar.clone();
        let keys: HashSet<&ConversationKey> = sidebar.session_rows().map(|row| &row.key).collect();
        let before = self.avatars.len();
        self.avatars.retain(|key, _| keys.contains(key));
        let mut changed = self.avatar_views.is_none() || self.avatars.len() != before;
        for key in keys {
            if self.avatars.contains_key(key) {
                continue;
            }
            let args = SessionAvatarArgs {
                machine: key.machine.clone(),
                conversation: key.id.clone(),
            };
            let created = api::serde_json::to_value(&args)
                .map_err(|error| error.to_string())
                .and_then(|args| api::create_view(&view_id, &args, window, cx));
            match created {
                Ok(avatar) => {
                    self.avatars.insert(key.clone(), avatar);
                    changed = true;
                }
                Err(error) => eprintln!("[sessions] 建不了「{}」的行首头像：{error}", key.id),
            }
        }
        if changed {
            let views = self
                .avatars
                .iter()
                .map(|(key, avatar)| (key.clone(), avatar.view()))
                .collect();
            self.avatar_views = Some(Rc::new(views));
            cx.notify();
        }
    }

    /// 列表里一行有多宽：列表宽度去掉行两边的外边距（`mx_2`）。拖拽牌子照它的宽画。
    pub(super) fn row_width(&self) -> Pixels {
        (self.scroll.bounds().size.width - px(16.)).max(px(120.))
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
        let notices = sessions
            .sidebar
            .notices_besides(sessions.selection.session());
        let hint = offscreen::hint(&notices, |key| self.anchors.placement(key));
        let ctx = RowCtx {
            sidebar: sessions.sidebar.clone(),
            view: cx.entity(),
            selection: sessions.selection.clone(),
            drop: cx.has_active_drag().then(|| self.drop.clone()).flatten(),
            // 松手时 GPUI 立刻重画，牌子却可能晚一帧才放掉：看有没有在拖，原处当场亮回来。
            lifted: cx.has_active_drag().then(|| self.lifted.clone()).flatten(),
            rename: self
                .rename
                .as_ref()
                .map(|rename| (rename.target.clone(), rename.input.clone())),
            search: self.search.as_ref().map(|search| search.input.clone()),
            now: chrono::Utc::now().timestamp().max(0) as u64,
            motion,
            avatars: self.avatar_views.clone(),
            anchors: self.anchors.clone(),
        };
        let rows = div()
            .flex()
            .flex_col()
            .children(rows::sidebar(&ctx, window, cx));
        let content = div()
            .flex()
            .flex_col()
            .pb_3()
            .child(self.anchors.root(rows));
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
            // 改名、搜索时按 Esc：输入框自己用不上就往上传，在这里取消改名或收起搜索。
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.escape(window, cx)))
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
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(Scrollable::new(
                        "sessions-list-scroll",
                        content,
                        &self.scroll,
                        ScrollbarAxis::Vertical,
                    ))
                    .children(offscreen::pills(hint, self.anchors.top_inset())),
            )
            .child(edge(offset > -self.scroll.max_offset().y))
    }
}
