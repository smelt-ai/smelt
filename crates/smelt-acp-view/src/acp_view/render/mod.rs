//! ACP 会话消息流的 GPUI 渲染实现。
//!
//! `AcpView` 的状态变更与协议交互仍在父模块；本文件只编排各块元素。
//! 消息列表、审批卡、输入栏分别在 sibling 模块。

use super::*;
use smelt_core::daemon_state::DaemonPhase;

use super::ConversationTranscript;

mod cards;
mod composer;
mod conversation;

impl Render for AcpView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _timing = smelt_ui::frame_stats::RenderTiming::start("AcpView");
        self.ensure_elicitation_inputs(window, cx);
        self.apply_pending_composer_restore(window, cx);
        self.settle_revealed_prompt();
        // 技能目录一次就要数毫秒到几十毫秒。过期必须在启动规格变化时刷新，
        // 不能留到绘制里扫盘。
        debug_assert!(
            !matches!(
                loaded_skills_refresh(
                    self.agent,
                    &self.launch,
                    &self.cwd,
                    self.loaded_skills.as_ref(),
                ),
                LoadedSkillsRefresh::Rescan
            ),
            "技能目录过期却留到了绘制路径"
        );
        let composer_focused = self
            .input
            .as_ref()
            .is_some_and(|input| input.focus_handle(cx).is_focused(window));
        self.settle_composer_motion(cx.reduce_motion());
        // 每个会话一条曲线。同一个窗口里换会话时不能接着上一条的焦点相位。
        let focus_id: gpui::ElementId = ("acp-composer-focus", cx.entity_id()).into();
        let composer_focus = unit_transition(
            focus_id,
            if composer_focused { 1.0 } else { 0.0 },
            window,
            cx,
        );
        if !cx.reduce_motion() && !self.pending_image_enter.is_empty() {
            window.request_animation_frame();
        }
        self.view(
            composer_focus,
            ambient_motion_enabled(
                ambient_application_active(cx),
                window.is_window_active(),
                true,
            ),
            cx,
        )
    }
}

impl AcpView {
    /// ACP UI 的只读投影：`AcpView state -> element tree`。事件闭包只派发 action，
    /// 实际状态变更仍由父模块的方法集中处理。
    fn view(
        &self,
        composer_focus: f32,
        animate_ambient: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let t = cx.theme();
        let muted = t.muted_foreground;
        let acp_surface: gpui::Hsla = gpui::transparent_black();
        let non_fresh_starting =
            matches!(self.phase, DaemonPhase::Connecting) && !self.is_fresh_conversation_start();
        let starting_copy = non_fresh_starting.then(|| {
            let waited_seconds = self
                .starting_since
                .map(|started| started.elapsed().as_secs())
                .unwrap_or(0);
            starting_status_copy(
                self.status_line.as_deref(),
                // history_session_id 可能只是本次 session/load 的候选值。
                // 在 provider 返回 Ready 前没有 runtime id，显示“启动中”更
                // 准确；真正恢复成功后通常已经离开 Starting，且历史会回放。
                self.history_session_id.is_some() && self.acp_session_id.is_some(),
                self.agent.label(),
                waited_seconds,
            )
        });
        let show_starting_placeholder = should_show_starting_placeholder(
            &self.phase,
            self.entries.is_empty(),
            self.history_session_id.is_some(),
            self.pending_initial_prompt.is_some(),
        );
        let show_ended_placeholder =
            matches!(self.phase, DaemonPhase::Dead) && self.entries.is_empty();
        let history_error = (!matches!(self.phase, DaemonPhase::Connecting))
            .then(|| self.history_error.clone())
            .flatten();
        let show_empty_conversation_state = should_show_empty_conversation_state(
            &self.phase,
            self.entries.is_empty(),
            self.history_session_id.is_some(),
            self.pending_initial_prompt.is_some(),
            self.queued_prompts.is_empty()
                && self.queued_steering.is_empty()
                && self.queued_follow_up.is_empty(),
            self.prompt_dispatch_pending,
        );

        // 新建空白会话在后台静默准备，用户可以立刻输入；续接/失败仍明确展示状态。
        let banner: Option<gpui::AnyElement> = match (&self.phase, starting_copy.as_ref()) {
            (DaemonPhase::Connecting, Some((_, detail, elapsed))) if !show_starting_placeholder => {
                Some(
                    h_flex()
                        .w_full()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(t.border)
                        .bg(gpui::rgb(ui_theme::bg_bar()))
                        .child(ambient_spinner(
                            "acp-starting-banner-spinner",
                            gpui::rgb(ui_theme::text_muted()).into(),
                            animate_ambient,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(muted)
                                .child(detail.clone()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted)
                                .child(elapsed.clone()),
                        )
                        .into_any_element(),
                )
            }
            (DaemonPhase::Connecting, _) => None,
            (DaemonPhase::Dead, _) if show_ended_placeholder => None,
            (DaemonPhase::Dead, _) => Some(
                v_flex()
                    .w_full()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(t.border)
                    .bg(gpui::rgb(ui_theme::bg_bar()))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::new(IconName::CircleX)
                                    .size(px(16.))
                                    .text_color(t.danger),
                            )
                            .child(div().text_sm().text_color(t.danger).child("会话已结束"))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .id("acp-restart")
                                    .h(ui_theme::height::sm())
                                    .px_3()
                                    .rounded_full()
                                    .bg(ui_theme::overlay(0x18))
                                    .flex()
                                    .items_center()
                                    .text_xs()
                                    .font_medium()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(ui_theme::overlay(0x28)))
                                    .child("重新开始")
                                    .on_click(cx.listener(|this, _ev, window, cx| {
                                        this.restart(window, cx);
                                    })),
                            ),
                    )
                    .when(!self.end_reason.is_empty(), |d| {
                        d.child(
                            div()
                                .pl_6()
                                .text_xs()
                                .text_color(muted)
                                .font_family(smelt_core::font_config::font_family())
                                .child(self.end_reason.clone()),
                        )
                    })
                    .into_any_element(),
            ),
            _ => None,
        };
        let starting_placeholder = show_starting_placeholder.then(|| {
            let (title, detail, elapsed) = starting_copy
                .as_ref()
                .expect("non-fresh startup always has loading copy");
            let indicator = ambient_spinner(
                "acp-starting-placeholder-spinner",
                gpui::rgb(ui_theme::text_muted()).into(),
                animate_ambient,
            )
            .into_any_element();
            v_flex()
                .absolute()
                .top(px(ui_theme::CHROME_ISLAND_CLEARANCE_PX))
                .right_0()
                .bottom_0()
                .left_0()
                .items_center()
                .justify_center()
                .p_4()
                .child(
                    v_flex()
                        .id("acp-starting-placeholder")
                        .w_full()
                        .max_w(px(400.))
                        .items_center()
                        .gap_3()
                        .child(indicator)
                        .child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .text_color(t.foreground)
                                .text_center()
                                .child(title.clone()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .text_center()
                                .child(detail.clone()),
                        )
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded_full()
                                .bg(ui_theme::overlay(0x14))
                                .text_xs()
                                .font_features(ui_theme::tabular_figures())
                                .text_color(muted)
                                .child(elapsed.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .text_center()
                                .child("可以先在下方输入，连接完成后会自动发送"),
                        ),
                )
        });
        let ended_placeholder = show_ended_placeholder.then(|| {
            let message = if self.end_reason.trim().is_empty() {
                "与 smeltd 的连接已断开".to_string()
            } else {
                self.end_reason.clone()
            };
            let action_label = if self.history_session_id.is_some() {
                "恢复会话"
            } else {
                "新建会话"
            };
            v_flex()
                .absolute()
                .top(px(ui_theme::CHROME_ISLAND_CLEARANCE_PX))
                .right_0()
                .bottom_0()
                .left_0()
                .items_center()
                .justify_center()
                .p_4()
                .child(
                    v_flex()
                        .id("acp-ended-placeholder")
                        .w_full()
                        .max_w(px(420.))
                        .items_center()
                        .gap_3()
                        .child(
                            h_flex()
                                .size(px(48.))
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .border_1()
                                .border_color(ui_theme::tint(ui_theme::red(), 0x48))
                                .bg(ui_theme::tint(ui_theme::red(), 0x14))
                                .child(
                                    Icon::new(IconName::CircleX)
                                        .size(px(24.))
                                        .text_color(gpui::rgb(ui_theme::red())),
                                ),
                        )
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
                                .text_color(t.foreground)
                                .text_center()
                                .child("会话已结束"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .text_center()
                                .child(message),
                        )
                        .child(
                            div()
                                .id("acp-restart")
                                .h(ui_theme::height::lg())
                                .px_5()
                                .rounded_full()
                                .bg(gpui::rgb(ui_theme::action_fill()))
                                .text_sm()
                                .font_semibold()
                                .text_color(gpui::rgb(ui_theme::action_on()))
                                .flex()
                                .items_center()
                                .cursor_pointer()
                                .hover(|d| d.opacity(0.88))
                                .child(action_label)
                                .on_click(cx.listener(|this, _ev, window, cx| {
                                    this.restart(window, cx);
                                })),
                        ),
                )
        });
        let empty_conversation_state = show_empty_conversation_state.then(|| {
            let workspace_label = self.cwd.as_deref().and_then(|cwd| {
                let path = std::path::Path::new(cwd);
                if smelt_core::agent_definition_store::is_workbench_conversation_workspace(path) {
                    return Some("工作台".to_string());
                }
                path.file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .map(|name| format!("当前工作区 · {name}"))
            });
            let quick_prompts = [
                (
                    "了解这个项目",
                    "梳理结构、入口和运行方式",
                    "请快速梳理这个项目的结构、关键入口和运行方式。",
                ),
                (
                    "检查当前改动",
                    "概述变更并指出潜在风险",
                    "请检查当前工作区的改动，概述内容并指出潜在风险。",
                ),
                (
                    "定位一个问题",
                    "先补充现象，再一起排查",
                    "请帮我定位这个问题：",
                ),
                (
                    "补充测试",
                    "先给出方案，再实现验证",
                    "请为当前相关模块补充测试，并先说明测试方案。",
                ),
            ];
            // Grok 空状态是「大标题 + 一排安静的胶囊」，不是带编号的功能卡片墙。
            let mut prompt_chips = h_flex().w_full().justify_center().gap_2().flex_wrap();
            for (prompt_index, (title, _detail, prompt)) in quick_prompts.iter().enumerate() {
                let prompt = (*prompt).to_string();
                prompt_chips = prompt_chips.child(
                    div()
                        .id(("acp-empty-prompt", prompt_index))
                        .px_3()
                        .py_1p5()
                        .rounded_full()
                        .bg(ui_theme::overlay(0x14))
                        .text_sm()
                        .text_color(t.foreground)
                        .cursor_pointer()
                        .hover(|chip| chip.bg(ui_theme::overlay(0x22)))
                        .child((*title).to_string())
                        .on_click(cx.listener(move |this, _ev, window, cx| {
                            this.insert_prompt_text(&prompt, window, cx);
                        })),
                );
            }

            v_flex()
                .absolute()
                .top(px(ui_theme::CHROME_ISLAND_CLEARANCE_PX))
                .right_0()
                .bottom_0()
                .left_0()
                .items_center()
                .justify_center()
                .px_4()
                .pb(px(56.))
                .child(
                    v_flex()
                        .id("acp-empty-conversation")
                        .w_full()
                        .max_w(px(560.))
                        .items_center()
                        .gap_4()
                        .child(
                            div()
                                .text_size(px(28.))
                                .font_semibold()
                                .text_color(t.foreground)
                                .child(self.agent.short_label().to_string()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .text_center()
                                .child("想让它做什么？"),
                        )
                        .children(workspace_label.map(|label| {
                            h_flex()
                                .px_2()
                                .py_1()
                                .gap_1()
                                .items_center()
                                .rounded_full()
                                .bg(ui_theme::overlay(0x14))
                                .text_xs()
                                .text_color(muted)
                                .child(Icon::new(IconName::File).size(px(12.)))
                                .child(label)
                        }))
                        .children((self.agent == ConversationAgentKind::Pi).then(|| {
                            let count = self.loaded_skills().len();
                            div().text_xs().text_color(muted).child(if count == 0 {
                                "未加载技能".to_string()
                            } else {
                                format!("已加载 {count} 个技能")
                            })
                        }))
                        .child(prompt_chips),
                )
        });
        let jump_to_latest = self.viewing_history.then(|| {
            h_flex()
                .absolute()
                .bottom(px(14.))
                .left_0()
                .right_0()
                .justify_center()
                .child(
                    h_flex()
                        .id("acp-jump-to-latest")
                        .h(ui_theme::height::lg())
                        .px_4()
                        .gap_2()
                        .items_center()
                        .rounded_full()
                        .bg(ui_theme::glass_floating())
                        .shadow_sm()
                        .shadow_sm()
                        .cursor_pointer()
                        .hover(|button| button.bg(gpui::rgb(ui_theme::bg_hover())))
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(gpui::rgb(ui_theme::text_mid()))
                                .child("回到最新"),
                        )
                        .child(div().text_sm().text_color(muted).child("↓"))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.viewing_history = false;
                            this.list_state.set_follow_mode(FollowMode::Tail);
                            cx.notify();
                        }))
                        .with_animation(
                            "acp-jump-to-latest-enter",
                            Animation::new(std::time::Duration::from_millis(160)),
                            |button, delta| button.opacity(delta),
                        ),
                )
        });
        let border = t.border;
        let plan_bar = self.render_plan_bar(cx);
        let history_error_banner = history_error.map(|error| {
            div()
                .w_full()
                .px_4()
                .py_2()
                .text_sm()
                .text_color(t.danger)
                .border_b_1()
                .border_color(t.border)
                .child(error)
                .into_any_element()
        });
        // 顶上的条已经让开胶囊时，列表第一条不要再让一次。
        let clear_capsule = self.message_list_clears_capsule();
        // 消息列表是独立缓存视图。这里每帧都会走到，但列表没脏时不会重排
        // Markdown / 图片；打字只标脏输入框的祖先，不会标脏列表。
        let list = self.transcript.clone().cached(
            gpui::StyleRefinement::default()
                .w_full()
                .flex_1()
                .min_h(gpui::px(0.)),
        );
        let (permission, elicitation) = self.render_approval_ui(animate_ambient, cx);
        let input_row = self.render_composer(composer_focus, cx);
        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            // ⌘⏎ 快捷批准：有待审批卡片时等价于点绿色主按钮。挂在根上冒泡接收，
            // 输入框聚焦时也能生效（Input 只消费不带修饰键的 Enter）。
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _window, cx| {
                if ev.keystroke.modifiers.platform
                    && ev.keystroke.key == "enter"
                    && !this.permissions.is_empty()
                {
                    this.pick_permission_primary(cx);
                    cx.stop_propagation();
                }
            }))
            // 补全弹层的键盘操作。同样只能走 **action 的 capture 阶段**：
            // 上/下/回车/Esc/Tab 在输入框里全都绑成了 action，冒泡阶段和
            // capture_key_down 都轮不到我们（见下面 ⌘V 那段的教训）。
            // 没在补全时一律不拦，按键原样交回输入框。
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::MoveUp, _window, cx| {
                    if this.move_completion(-1, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(cx.listener(
                |this, _: &gpui_component::input::MoveDown, _window, cx| {
                    if this.move_completion(1, cx) {
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Enter, window, cx| {
                    // 补全开着时回车是「选中这条」，不是发送——否则永远选不上。
                    if this.accept_completion(window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(cx.listener(
                |this, _: &gpui_component::input::IndentInline, window, cx| {
                    if this.accept_completion(window, cx) {
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Escape, _window, cx| {
                    if this.usage_popover_open {
                        this.usage_popover_open = false;
                        cx.notify();
                        cx.stop_propagation();
                    } else if this.background_tasks_open {
                        this.background_tasks_open = false;
                        cx.notify();
                        cx.stop_propagation();
                    } else if this.completion.take().is_some() {
                        cx.notify();
                        cx.stop_propagation();
                    } else if this.is_visibly_running() {
                        this.cancel_turn();
                        cx.stop_propagation();
                    }
                }),
            )
            // ⌘V 贴图（输入框聚焦时，也就是绝大多数情况）：必须拦 **Paste
            // action 的 capture 阶段**，不能拦 key_down。
            //
            // 真实教训：第一版挂的是 capture_key_down，实测完全没反应。GPUI 的
            // dispatch_key_event 顺序是「先派发 action bindings，binding 消费掉
            // 就直接 return」，capture 阶段的 key listener 排在那之后——输入框
            // 把 cmd-v 绑成了 Paste（gpui-component input/state.rs），于是这个
            // 事件永远轮不到我们。而 action 的 capture 阶段是从根往下走的，
            // 挂在这里就能抢在输入框（更深的节点）前面拿到。
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Paste, _window, cx| {
                    // 只有剪贴板真是图片才截胡；文本粘贴照样放行给输入框。
                    if this.take_clipboard_image(cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            // 焦点不在输入框里（点了消息流等）时 Paste binding 不匹配，
            // action 那条路走不到——这条按 key_down 兜底。
            .capture_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _window, cx| {
                if ev.keystroke.modifiers.platform
                    && ev.keystroke.key == "v"
                    && this.take_clipboard_image(cx)
                {
                    cx.stop_propagation();
                }
            }))
            .bg(acp_surface)
            .children((!clear_capsule).then(|| {
                v_flex()
                    .w_full()
                    .pt(px(ui_theme::CHROME_ISLAND_CLEARANCE_PX))
                    .children(history_error_banner)
                    .children(banner)
                    .children(plan_bar)
            }))
            .child(
                v_flex()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(list)
                    .children((!self.entries.is_empty()).then(|| {
                        Scrollbar::vertical(&self.list_state)
                            .id("acp-message-scrollbar")
                            .mode(ScrollbarMode::Always)
                    }))
                    .children(jump_to_latest)
                    .children(empty_conversation_state)
                    .children(starting_placeholder)
                    .children(ended_placeholder),
            )
            .children(permission)
            .children(elicitation)
            .children(self.paste_hint.as_ref().map(|msg| {
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_1p5()
                    .border_t_1()
                    .border_color(border)
                    .bg(ui_theme::tint(ui_theme::yellow(), 0x14))
                    .text_xs()
                    .text_color(gpui::rgb(ui_theme::yellow()))
                    .child(msg.clone())
            }))
            .children(input_row)
    }

    fn message_list_clears_capsule(&self) -> bool {
        let history_error_visible =
            !matches!(self.phase, DaemonPhase::Connecting) && self.history_error.is_some();
        if history_error_visible {
            return false;
        }
        if self
            .plan
            .as_ref()
            .is_some_and(|plan| !plan.entries.is_empty())
        {
            return false;
        }
        let show_starting_placeholder = should_show_starting_placeholder(
            &self.phase,
            self.entries.is_empty(),
            self.history_session_id.is_some(),
            self.pending_initial_prompt.is_some(),
        );
        let show_ended_placeholder =
            matches!(self.phase, DaemonPhase::Dead) && self.entries.is_empty();
        let non_fresh_starting =
            matches!(self.phase, DaemonPhase::Connecting) && !self.is_fresh_conversation_start();
        match self.phase {
            DaemonPhase::Connecting if non_fresh_starting && !show_starting_placeholder => false,
            DaemonPhase::Dead if !show_ended_placeholder => false,
            _ => true,
        }
    }
}

impl Render for ConversationTranscript {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _timing = smelt_ui::frame_stats::RenderTiming::start("Transcript");
        let animate_ambient = ambient_motion_enabled(
            ambient_application_active(cx),
            window.is_window_active(),
            true,
        );
        let reduce_motion = cx.reduce_motion();
        self.host.update(cx, |view, _cx| {
            view.settle_transcript_motion(reduce_motion);
        });
        // 入场时钟在上面这次 settle 里才启动。先结算再要帧，新气泡的第一帧才会排下一帧。
        // 帧要打在这条列表上：此刻的 current view 是 transcript，打在 AcpView 上不会让列表重画。
        if !reduce_motion && self.host.read(cx).decorative_transcript_motion() {
            window.request_animation_frame();
        }
        self.host.update(cx, |view, cx| {
            let clear_capsule = view.message_list_clears_capsule();
            view.render_message_list(animate_ambient, clear_capsule, cx)
        })
    }
}
