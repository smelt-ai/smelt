//! 工作台每帧的数据准备：刷新缓存、懒建控件、把副作用送出去。
//!
//! `Workspace::render` 只组页面，不在这里写业务状态。页面模块（`render_*`）
//! 读这些已经准备好的缓存；真正的用户操作（提交、删文件、改设置）走各自的
//! `impl Workspace` 事件方法，不塞进绘制。

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::*;

use crate::settings::Appearance;
use crate::{
    SessionKind, Workspace, liquid_glass, mem_usage, settings, should_auto_resume_active_acp,
    tool_panel,
};

impl Workspace {
    /// 组页面前的数据层。允许 `&mut self`：拉缓存、建输入框、同步窗口材质。
    /// 不构造任何页面元素。
    pub(crate) fn prepare_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_session_ui(window, cx);
        self.note_window_activation(window, cx);

        // 拖拽生命周期属于帧准备，不属于元素树投影。统一在这里清理瞬态状态并
        // 设置系统光标后，侧栏渲染入口可以保持 `&Workspace -> Div` 的只读契约。
        if !cx.has_active_drag() {
            self.sidebar_drag = None;
            self.sess_drop_hint = None;
            self.proj_drop_hint = None;
            self.stop_sidebar_drag_scroll();
        } else if self.sidebar_drag.is_some() {
            cx.set_active_drag_cursor_style(CursorStyle::ClosedHand, window);
        }

        // 应用级事件观察器负责实时同步；这里只作会话新增/改名等普通 UI 变更的兜底。
        self.sync_notification_surfaces(cx);
        self.prewarm_ide_catalog_after_idle(window, cx);
        self.refresh_history_titles(cx);

        // 侧栏 GIT 角标全页面实时：保证当前项目的文件监听已建立。角标常驻显示，但
        // git_status 数据原本只在 Files/Git 页 render 时刷新，切到终端等页面就冻结。
        // ensure_git_watch 建监听后，仓库一有改动就主动重拉 git_status，角标在任何页面
        // 都即时跟手——事件驱动，文件不变时零开销，不搞轮询。
        // 内部按 root 去重，每帧调只是一次 HashMap 查找。
        if let Some(root) = self.active_project_root(cx) {
            self.ensure_git_watch(root, cx);
        }

        // 侧栏项目分组：后台刷新每个会话 cwd 的仓库身份（是不是 worktree + 分支名），
        // 让 worktree 的会话能跟主仓库聚在一起显示、标签带上分支名。侧栏一直显示
        // 全部项目，不像 git status 那样只关心当前打开的那个，所以对
        // self.sessions 里出现过的所有 cwd 都要探测，而不是只探测 self.cur()。
        let repo_cwds: HashSet<String> = self.sessions.iter().filter_map(|s| s.cwd(cx)).collect();
        for cwd in repo_cwds {
            self.ensure_repo_info(cwd, cx);
        }

        // 各类后台操作（建/删 worktree、生成 commit message）失败时，错误信息暂存在
        // 这个字段（后台任务里没有 Window），组页面前取走改走系统通知。
        if let Some(msg) = self.background_error.take() {
            crate::status_item::notify_error(msg);
        }
        // Git 页：后台刷新改动列表 + 分支列表（git status/for-each-ref 慢，绝不在
        // 页面绘制里同步跑）。
        if (self.tool_panel_stage_active(tool_panel::ToolPanelTab::Git)
            || (self.tool_panel_open && self.tool_panel_tab == tool_panel::ToolPanelTab::Git))
            && let Some(root) = self.active_project_root(cx)
        {
            // 进 Git 页主动 fetch 一次，让 ahead/behind 反映远端最新。每帧都会
            // 进这个分支，靠 git_autofetch_at 去抖——同一仓库 60s 内只自动 fetch 一次，
            // 避免每帧狂发网络请求。fetch 成功后 run_git_op 会 invalidate_git_status，
            // 顺带把 ahead/behind 重算出来。
            let fetch_due = self
                .git_autofetch_at
                .get(&root)
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(60));
            if fetch_due {
                self.git_autofetch_at.insert(root.clone(), Instant::now());
                self.git_fetch_silent(cx);
            }
            self.ensure_git_watch(root.clone(), cx);
            // 多仓工作区：发现项目里的所有仓库，并为每个拉自己的 status。
            self.ensure_git_repos(root.clone(), cx);
            // 分支列表（历史页的分支树、切分支下拉）属于当前选中的仓库，
            // 不是项目根——多仓时后者只是其中一个仓库。
            let git_repo = self.active_git_repo_root(&root);
            self.ensure_branches(git_repo, cx);
            self.prepare_git_panel(window, cx);
        }

        // 历史会话页：后台刷新当前项目的会话列表。
        if self.tool_panel_stage_active(tool_panel::ToolPanelTab::History)
            || (self.tool_panel_open && self.tool_panel_tab == tool_panel::ToolPanelTab::History)
        {
            if self.history_filter.is_none() {
                use gpui_component::input::{InputEvent, InputState};
                let state =
                    cx.new(|cx| InputState::new(window, cx).placeholder("搜索名称或原始标题…"));
                self._history_filter_sub =
                    Some(cx.subscribe(&state, |_, _, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            cx.notify();
                        }
                    }));
                self.history_filter = Some(state);
            }
            let cwd = self.active_project_root(cx);
            if self.history_project_root.as_ref() != cwd.as_ref() {
                // 历史详情按项目索引；项目从侧栏或状态栏切换后，
                // 先丢弃旧项目的选择与异步详情，再投影当前项目的数据。
                self.history_project_root = cwd.clone();
                self.session_detail_gen = self.session_detail_gen.wrapping_add(1);
                self.session_detail = None;
                self.history_detail_list_state.reset(0);
            }
            if let Some(root) = cwd {
                // 引擎/profile 必须和渲染侧取同一个答案，否则智能体上下文里会按
                // 上一个项目选中的引擎去扫 space，列表永远是空的。
                let (agent, pid) = self.history_source(Some(root.as_str()), cx);
                self.ensure_session_list(agent, pid, root, cx);
            }
        }

        // 文件树页：后台刷新根目录 + 所有已展开目录的直接子项列表（fs::read_dir 绝不
        // 在页面绘制里同步跑）。展开新目录时它会先落空，下一帧缓存到位后自动出现。
        if self.files_view_visible() {
            // 搜索输入框懒创建（需要 window）：键入即 notify，触发文件名 + 内容搜索。
            if self.file_filter.is_none() {
                use gpui_component::input::{InputEvent, InputState};
                let state =
                    cx.new(|cx| InputState::new(window, cx).placeholder("搜索文件名 / 内容…"));
                self._file_filter_sub = Some(cx.subscribe(&state, |_, _, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) {
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
            // 多根工作区：文件树同时挂着所有项目根，后台刷新要覆盖每个根。改动文件
            // M/A/D 标要用 git status；不强制用户先去过 Git 页才有数据，Files 页自己
            // 也确保各根缓存新鲜（ensure_git_status 内部已有 TTL）。
            let roots = self.workspace_roots(cx);
            if query.is_empty() {
                // 无查询：正常树形浏览，清空上一次搜索结果。
                self.search_results = None;
                for root in &roots {
                    self.ensure_git_status(root.clone(), cx);
                    self.ensure_dir_listing(root.clone(), cx);
                }
                // 展开的子目录是绝对路径、跟属于哪个根无关，一次性全刷。
                for dir in self.expanded.clone() {
                    self.ensure_dir_listing(dir, cx);
                }
                self.try_flush_file_tree_reveal(cx);
            } else if let Some(root) = self.active_project_root(cx) {
                // 有查询：搜索先只在当前会话根做（跨根搜索留作后续）；顺带刷一份该根的
                // git status 给结果视图用。
                self.ensure_git_status(root.clone(), cx);
                self.ensure_search(root, query, cx);
            }
        }

        // 顶部系统玻璃和整扇窗口 alpha 都必须通过 Window/AppKit 设置：
        // 元素 `.opacity()` 只能淡化 GPUI 内容，无法让内容下的 AppKit 背景材质变透明。
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

        // 调试 HUD：开启时用 request_animation_frame 驱动连续渲染，测真实帧率
        // （连续重绘会重跑整窗布局/绘制，diff 面板卡不卡直接反映到帧耗时上）。
        if self.debug_hud {
            let now = Instant::now();
            if let Some(prev) = self.last_frame {
                let dt = now.duration_since(prev).as_secs_f32();
                if dt > 0.0 {
                    let inst = 1.0 / dt;
                    self.fps_ema = if self.fps_ema <= 0.0 {
                        inst
                    } else {
                        self.fps_ema * 0.9 + inst * 0.1
                    };
                }
            }
            self.last_frame = Some(now);
            let mem_due = self
                .debug_mem_sampled_at
                .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1));
            if mem_due {
                self.debug_mem_rss = mem_usage::current_rss_bytes();
                self.debug_mem_sampled_at = Some(now);
            }
            window.request_animation_frame();
        } else {
            self.last_frame = None;
            self.debug_mem_rss = None;
            self.debug_mem_sampled_at = None;
        }

        // 冷恢复后若活动项是智能体对话，把工作台带回那段对话（子页不落盘，恢复出来
        // 的是根目录页）。只做一次：每帧都做会把用户点侧栏「智能体」返回目录的动作
        // 在下一帧顶回来，表现为点了没反应。
        let active_is_agent_conversation = self
            .sessions
            .get(self.active_session)
            .is_some_and(|session| session.is_agent_conversation(cx));
        if crate::should_open_restored_agent_conversation(
            self.sessions_restored,
            self.restored_agent_conversation_route,
            active_is_agent_conversation,
            self.active_tab(),
        ) {
            self.restored_agent_conversation_route = true;
            self.activate(self.active_session, window, cx);
        } else if self.sessions_restored {
            self.restored_agent_conversation_route = true;
        }

        // 不变量：智能体对话不属于项目会话列表，不能占着项目舞台。这条得一直守着，
        // 和上面那次性补位不是一回事。恢复完成前不要看 `active_session` 下标——
        // ACP 先插入时，存档下标会对到「对话」里的会话。
        if crate::should_correct_agent_conversation_on_project_stage(
            self.sessions_restored,
            self.active_tab(),
            active_is_agent_conversation,
        ) {
            self.activate(self.active_session, window, cx);
        }

        // ACP 冷恢复会话「上屏即续接」：挂在数据准备而不是 activate()——冷启动
        // 后停在哪个会话上，那个会话压根不会收到 activate 调用，只挂那边的话
        // 「重开 GUI 后当前这个 ACP 会话仍要手点重新开始」。maybe_auto_resume
        // 自带一次性闸门，每帧调无副作用。
        if should_auto_resume_active_acp(self.sessions_restored)
            && let Some(SessionKind::Conversation(view)) =
                self.sessions.get(self.active_session).map(|s| &s.kind)
        {
            let view = view.clone();
            view.update(cx, |v, cx| v.maybe_auto_resume(window, cx));
        }
    }
}
