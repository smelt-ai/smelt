//! 可见数据的刷新时机：选中、切页、数据变化。没有定时的保底：缓存只被事件标旧
//! （运行时的 `project.changed`、自己做完的操作、重连、窗口回到前台），这里只取看得见
//! 又被标旧了的那些。
//!
//! `Workspace::render` 每帧都会整段重跑。目录列表、git、历史扫描和窗口材质
//! 不能挂在那里——侧栏一个转圈就会让这些活儿跟着每秒跑很多次。真正开销在
//! `refresh_visible_caches`：事件里排队，绘制前跑完，render 只读已经放好的缓存。

use std::time::{Duration, Instant};

use gpui::*;

use crate::settings::Appearance;
use crate::{SessionKind, Workspace, liquid_glass, settings, should_auto_resume_active_acp};

/// 同一项目两次自动 fetch 至少隔这么久。
const GIT_AUTOFETCH_INTERVAL: Duration = Duration::from_secs(60);

/// 离下一次自动 fetch 还有多久；`ZERO` 是现在就该 fetch。
pub(crate) fn git_autofetch_wait(last: Option<Instant>, now: Instant) -> Duration {
    last.map_or(Duration::ZERO, |at| {
        GIT_AUTOFETCH_INTERVAL.saturating_sub(now.saturating_duration_since(at))
    })
}

impl Workspace {
    /// 绘制前唯一还留在帧上的准备。
    ///
    /// 拖拽是否还活着，只有这一帧的 `has_active_drag` 知道，侧栏渲染又要求
    /// 进来时瞬态状态已经清干净。窗口是否在前台同样只有每帧问一次系统才准：
    /// 全屏切 Space 时窗口自己的激活位会撒谎。这两件事都不碰 SQLite、文件或进程。
    /// 目录、git、历史、窗口材质和路由修正都在 `refresh_visible_caches`。
    pub(crate) fn prepare_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.note_window_activation(window, cx);

        if !cx.has_active_drag() {
            self.sidebar_drag = None;
            self.sess_drop_hint = None;
            self.proj_drop_hint = None;
            self.sidebar_split_drag = None;
            self.stop_sidebar_drag_scroll();
        } else if self.sidebar_drag.is_some() {
            cx.set_active_drag_cursor_style(CursorStyle::ClosedHand, window);
        }
    }

    /// 后台失败当时就送出系统通知。不再等下一帧来取，调用方也就不用持有 Window。
    pub(crate) fn report_background_error(&mut self, message: impl Into<String>) {
        crate::status_item::notify_error(message.into());
    }

    /// 把一次可见数据刷新排到当前更新结束之后、下一帧绘制之前。
    ///
    /// 没有 `&mut Window` 的变更（关会话、切页签）靠构造时留下的窗口句柄
    /// 重新进入窗口。同一轮里多次调用合并成一次；刷新自己内部的切会话不再排队。
    pub(crate) fn schedule_visible_refresh(&mut self, cx: &mut Context<Self>) {
        if self.visible_refresh_scheduled || self.visible_refresh_running {
            return;
        }
        self.visible_refresh_scheduled = true;
        let handle = self.window_handle;
        let view = cx.weak_entity();
        cx.defer(move |cx| {
            // 弱引用的 update 在工作区还在时返回 Ok。窗口已经关了就把排队标记清掉，
            // 否则后面的变更会一直以为刷新已经排上。
            let refreshed = cx
                .update_window(handle, |_root, window, cx| {
                    view.update(cx, |this, cx| {
                        this.refresh_visible_caches(window, cx);
                    })
                    .is_ok()
                })
                .unwrap_or(false);
            if !refreshed {
                let _ = view.update(cx, |this, _cx| {
                    this.visible_refresh_scheduled = false;
                });
            }
        });
    }

    /// 按当前路由把可见页的缓存、输入框和窗口材质补齐。
    ///
    /// 调用方是构造、排队后的延迟，以及外观变化。不要从 `render` 里进这里。
    pub(crate) fn refresh_visible_caches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.visible_refresh_scheduled = false;
        self.visible_refresh_running = true;

        // 先把活动和路由对齐，后面的 git / 文件树才看得到这一次切换的结果。
        // 对齐里的 activate 会再请求刷新；running 让那次请求直接丢掉，
        // 因为这次刷新还会继续往下走。
        self.sync_session_ui(window, cx);
        self.align_visible_route(window, cx);
        self.sync_notification_surfaces(cx);
        self.prewarm_ide_catalog_after_idle(window, cx);

        if let Some(project) = self.active_project_ref(cx) {
            self.ensure_watch(project.clone(), cx);
            // 变更角标在任何页面都显示，活动项目各仓库的 status 一直备着。
            self.ensure_git_repos(project, cx);
        }
        self.note_active_project(cx);
        self.note_shown_views(cx);

        if self.git_page_visible()
            && let Some(root) = self.active_project_ref(cx)
        {
            if self.workspace_window_active {
                self.ensure_git_autofetch(cx);
            }
            let git_repo = self.active_git_repo_root(&root);
            self.ensure_branches(git_repo, cx);
            self.prepare_git_panel(window, cx);
        }

        if self.files_view_visible() {
            if self.file_filter.is_none() {
                use gpui_component::input::{InputEvent, InputState};
                let state =
                    cx.new(|cx| InputState::new(window, cx).placeholder("搜索文件名 / 内容…"));
                self._file_filter_sub =
                    Some(cx.subscribe(&state, |this, _, ev: &InputEvent, cx| {
                        if matches!(ev, InputEvent::Change) {
                            this.schedule_visible_refresh(cx);
                            cx.notify();
                        }
                    }));
                self.file_filter = Some(state);
            }
            let query = self
                .file_filter
                .as_ref()
                .map(|s| s.read(cx).value().trim().to_string())
                .unwrap_or_default();
            let runtime = self.file_tree_runtime(cx);
            let roots: Vec<crate::ProjectRef> = self
                .workspace_roots(cx)
                .into_iter()
                .map(|root| crate::ProjectRef {
                    runtime: runtime.clone(),
                    root,
                })
                .collect();
            if query.is_empty() {
                self.search_results = None;
                self.cancel_file_search();
                for root in &roots {
                    // 目录列表靠运行时的变化通知失效，每个挂出来的根都要有监听。
                    self.ensure_watch(root.clone(), cx);
                    self.ensure_git_status(root.clone(), cx);
                    self.ensure_dir_listing(root.clone(), cx);
                }
                for dir in self.expanded.clone() {
                    self.ensure_dir_listing(
                        crate::ProjectRef {
                            runtime: runtime.clone(),
                            root: dir,
                        },
                        cx,
                    );
                }
                self.try_flush_file_tree_reveal(cx);
            } else if let Some(root) = self.active_project_ref(cx) {
                self.ensure_git_status(root.clone(), cx);
                self.ensure_search(root, query, cx);
            }
        }

        let want_bg = cx.global::<Appearance>().window_bg();
        let glass_style = cx.global::<Appearance>().glass_style;
        if self.applied_window_bg != Some(want_bg) || self.applied_glass_style != Some(glass_style)
        {
            window.set_background_appearance(want_bg);
            liquid_glass::sync(window, glass_style);
            self.applied_window_bg = Some(want_bg);
            self.applied_glass_style = Some(glass_style);
        }
        let window_opacity = cx.global::<Appearance>().window_opacity();
        if self.applied_window_opacity != Some(window_opacity) {
            settings::apply_window_opacity(window, window_opacity);
            self.applied_window_opacity = Some(window_opacity);
        }
        let ui_font_px = cx.global::<Appearance>().ui_font_px;
        if self.applied_ui_font_px != Some(ui_font_px) {
            window.set_rem_size(px(ui_font_px as f32));
            self.applied_ui_font_px = Some(ui_font_px);
        }

        if should_auto_resume_active_acp(self.session_restore_settled(self.active_session, cx))
            && let Some(SessionKind::Conversation(view)) =
                self.sessions.get(self.active_session).map(|s| &s.kind)
        {
            let view = view.clone();
            view.update(cx, |v, cx| v.maybe_auto_resume(window, cx));
        }

        self.visible_refresh_running = false;
    }

    /// 冷恢复后把智能体对话带回对话页，并守住「对话不能占项目舞台」。
    ///
    /// 补位只做一次：每帧都做会把用户点侧栏「智能体」返回目录的动作顶回来。
    /// 项目舞台上的对话则是不变量，恢复完成后一直守着。
    fn align_visible_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_is_agent_conversation = self
            .sessions
            .get(self.active_session)
            .is_some_and(|session| session.is_agent_conversation(cx));
        if crate::should_open_restored_agent_conversation(
            self.session_restore_settled(self.active_session, cx),
            self.restored_agent_conversation_route,
            active_is_agent_conversation,
            self.active_tab(),
        ) {
            self.restored_agent_conversation_route = true;
            self.activate(self.active_session, window, cx);
        } else if self.session_restore_settled(self.active_session, cx) {
            self.restored_agent_conversation_route = true;
        }

        if crate::should_correct_agent_conversation_on_project_stage(
            self.session_restore_settled(self.active_session, cx),
            self.active_tab(),
            active_is_agent_conversation,
        ) {
            self.activate(self.active_session, window, cx);
        }
    }

    fn git_page_visible(&self) -> bool {
        self.view_shown(crate::builtin_views::Builtin::Git.id())
    }

    /// Git 页开着、窗口在前台时，活动项目每 60 秒自动 fetch 一次，刷新 ahead/behind。
    ///
    /// 一个任务按到期时刻睡，醒来时页关了、窗口到后台了就结束，可见刷新下次再起。
    fn ensure_git_autofetch(&mut self, cx: &mut Context<Self>) {
        if self.git_autofetch_running {
            return;
        }
        self.git_autofetch_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                let wait = this
                    .update(cx, |this, cx| this.git_autofetch_tick(cx))
                    .ok()
                    .flatten();
                let Some(wait) = wait else {
                    break;
                };
                // 轮询例外：远端仓库变了没有事件可等；周期 60 秒，上限是 Git 页开着且窗口在
                // 前台的这段时间，离开后最多再醒一次就结束。
                cx.background_executor().timer(wait).await;
            }
            let _ = this.update(cx, |this, _| this.git_autofetch_running = false);
        })
        .detach();
    }

    /// 到期就 fetch，返回离下一次还有多久；不该再跑了返回 None。
    fn git_autofetch_tick(&mut self, cx: &mut Context<Self>) -> Option<Duration> {
        if !self.git_page_visible() || !self.workspace_window_active {
            return None;
        }
        let root = self.active_project_ref(cx)?;
        let now = Instant::now();
        let wait = git_autofetch_wait(self.git_autofetch_at.get(&root).copied(), now);
        if !wait.is_zero() {
            return Some(wait);
        }
        self.git_autofetch_at.insert(root, now);
        self.git_fetch_silent(cx);
        Some(GIT_AUTOFETCH_INTERVAL)
    }
}

#[cfg(test)]
mod tests {
    use super::git_autofetch_wait;
    use std::time::{Duration, Instant};

    #[test]
    fn git_autofetch_runs_at_once_the_first_time_then_every_sixty_seconds() {
        let now = Instant::now();
        assert_eq!(git_autofetch_wait(None, now), Duration::ZERO);
        assert_eq!(git_autofetch_wait(Some(now), now), Duration::from_secs(60));
        let recent = now
            .checked_sub(Duration::from_secs(59))
            .expect("test clock");
        assert_eq!(
            git_autofetch_wait(Some(recent), now),
            Duration::from_secs(1)
        );
        let due = now
            .checked_sub(Duration::from_secs(61))
            .expect("test clock");
        assert_eq!(git_autofetch_wait(Some(due), now), Duration::ZERO);
    }
}
