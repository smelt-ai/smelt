//! 可见数据的刷新时机：选中、切页、数据变化。没有定时的保底：缓存只被事件标旧
//! （运行时的 `project.changed`、自己做完的操作、重连、窗口回到前台），这里只取看得见
//! 又被标旧了的那些。
//!
//! `Workspace::render` 每帧都会整段重跑。目录列表、git 标记、历史扫描和窗口材质
//! 不能挂在那里——侧栏一个转圈就会让这些活儿跟着每秒跑很多次。真正开销在
//! `refresh_visible_caches`：事件里排队，绘制前跑完，render 只读已经放好的缓存。

use gpui::*;

use crate::settings::Appearance;
use crate::{SessionKind, Workspace, settings, should_auto_resume_active_acp};

impl Workspace {
    /// 绘制前唯一还留在帧上的准备。
    ///
    /// 拖拽是否还活着，只有这一帧的 `has_active_drag` 知道，侧栏渲染又要求
    /// 进来时瞬态状态已经清干净。窗口是否在前台同样只有每帧问一次系统才准：
    /// 全屏切 Space 时窗口自己的激活位会撒谎。这两件事都不碰 SQLite、文件或进程。
    /// 目录、git 标记、历史、窗口材质和路由修正都在 `refresh_visible_caches`。
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

    /// 让窗口材质的种类跟上外观、深浅跟上 smelt 的主题。都没变就不再发给系统。
    pub(crate) fn sync_window_backdrop(&mut self, window: &Window, cx: &App) {
        let material = cx.global::<Appearance>().material;
        let dark = !crate::ui_theme::is_light();
        if self.applied_backdrop != Some((material, dark)) {
            crate::window_backdrop::sync(window, material, dark);
            self.applied_backdrop = Some((material, dark));
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

    /// 把看得见的视图的缓存、输入框和窗口材质补齐。
    ///
    /// 调用方是构造、排队后的延迟，以及外观变化。不要从 `render` 里进这里。
    pub(crate) fn refresh_visible_caches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.visible_refresh_scheduled = false;
        self.visible_refresh_running = true;

        // 先把活动会话和项目对齐，后面的文件树才看得到这一次切换的结果。
        self.sync_session_ui(cx);
        self.align_restored_conversation(cx);

        self.note_active_context(cx);

        self.sync_window_backdrop(window, cx);
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

    /// 冷启动恢复到一段侧栏对话上时，右侧工具面板的上下文跟上它的工作目录。点对话时由
    /// `open_agent_conversation` 设；恢复出来的没人点，在这里补一次。已经有活动项目
    /// （人点过项目或会话）就不动。
    fn align_restored_conversation(&mut self, cx: &mut Context<Self>) {
        if self.active_project.is_some() || !self.session_restore_settled(self.active_session, cx) {
            return;
        }
        let Some(session) = self
            .sessions
            .get(self.active_session)
            .filter(|session| session.is_agent_conversation(cx))
        else {
            return;
        };
        let Some(cwd) = session.cwd(cx).filter(|cwd| !cwd.is_empty()) else {
            return;
        };
        self.active_project = Some(crate::ProjectRef {
            runtime: session.runtime(cx).key().clone(),
            root: cwd.trim_end_matches('/').to_string(),
        });
        cx.notify();
    }
}
