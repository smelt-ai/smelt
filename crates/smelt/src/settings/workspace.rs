//! `Workspace` 的设置页交互与渲染实现。
//!
//! 设置数据模型、远程生命周期和 Agent hooks 分别由父模块及其子模块提供；本文件
//! 只保留设置页对 `Workspace` 的状态操作，避免把数千行 UI 回调堆在模块入口。

use super::*;

impl Workspace {
    /// 懒创建外观设置的有状态控件（需要 window）。
    pub fn ensure_appearance_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.opacity_slider.is_some()
            && self.ui_font_size_slider.is_some()
            && self.font_size_slider.is_some()
            && self.bg_image_opacity_slider.is_some()
            && self.bg_color_picker.is_some()
        {
            return;
        }

        // 不透明度滑块 + 界面字号滑块 + 终端字号滑块 + 背景图透明度滑块 + 背景色取色器。
        let ap = cx.global::<Appearance>().clone();
        let opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(60.0)
                .max(100.0)
                .step(1.0)
                .default_value(ap.window_opacity() * 100.0)
        });
        let ui_font_size_slider = cx.new(|_| {
            SliderState::new()
                .min(MIN_UI_FONT_PX as f32)
                .max(MAX_UI_FONT_PX as f32)
                .step(1.0)
                .default_value(ap.ui_font_px as f32)
        });
        let font_size_slider = cx.new(|_| {
            SliderState::new()
                .min(terminal_view::MIN_FONT_PX as f32)
                .max(terminal_view::MAX_FONT_PX as f32)
                .step(1.0)
                .default_value(ap.font_px as f32)
        });
        let bg_image_opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .step(1.0)
                .default_value(ap.bg_image_opacity.clamp(0.0, 1.0) * 100.0)
        });
        let bg_color_picker =
            cx.new(|cx| ColorPickerState::new(window, cx).default_value(rgb(ap.bg_color)));

        self.settings_subs.clear();

        self.settings_subs.push(
            cx.subscribe(&opacity_slider, |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let op = (*x / 100.0).clamp(0.3, 1.0);
                    this.set_appearance(move |a| a.opacity = op, cx);
                }
            }),
        );
        self.settings_subs.push(cx.subscribe(
            &ui_font_size_slider,
            |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let size = x
                        .round()
                        .clamp(MIN_UI_FONT_PX as f32, MAX_UI_FONT_PX as f32)
                        as u32;
                    this.set_appearance(move |a| a.ui_font_px = size, cx);
                }
            },
        ));
        self.settings_subs.push(cx.subscribe(
            &font_size_slider,
            |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let size = x.round().clamp(
                        terminal_view::MIN_FONT_PX as f32,
                        terminal_view::MAX_FONT_PX as f32,
                    ) as u32;
                    terminal_view::set_font_px(size);
                    this.set_appearance(move |a| a.font_px = size, cx);
                }
            },
        ));
        self.settings_subs.push(cx.subscribe(
            &bg_image_opacity_slider,
            |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let op = (*x / 100.0).clamp(0.0, 1.0);
                    this.set_appearance(move |a| a.bg_image_opacity = op, cx);
                }
            },
        ));
        self.settings_subs.push(cx.subscribe(
            &bg_color_picker,
            |this, _s, ev: &ColorPickerEvent, cx| {
                let ColorPickerEvent::Change(c) = ev;
                if let Some(hsla) = c {
                    let color = hsla_to_rgb(*hsla);
                    this.set_appearance(move |a| a.bg_color = color, cx);
                }
            },
        ));
        self.opacity_slider = Some(opacity_slider);
        self.ui_font_size_slider = Some(ui_font_size_slider);
        self.font_size_slider = Some(font_size_slider);
        self.bg_image_opacity_slider = Some(bg_image_opacity_slider);
        self.bg_color_picker = Some(bg_color_picker);
    }

    /// 无 window 版：改全局 + 存盘 + 重绘。窗口背景（透明/模糊）由 render 里的
    /// applied_window_bg 同步——供 slider/color_picker 的订阅回调用（它们拿不到 window）。
    pub fn set_appearance(&mut self, f: impl FnOnce(&mut Appearance), cx: &mut Context<Self>) {
        apply_appearance(f, cx);
        cx.notify();
        cx.refresh_windows();
    }

    /// 切换在线更新通道并立即检查一次。通道切换只影响后续更新来源，不改变当前包的
    /// 展示版本；若新通道指向不同发布包，仍按 URL 差异正常下载。
    #[cfg(any(target_os = "macos", windows))]
    pub fn set_update_channel(&mut self, channel: updater::UpdateChannel, cx: &mut Context<Self>) {
        if cx.global::<UpdateSettings>().channel == channel {
            return;
        }
        if !self.update_status.can_check() {
            return;
        }
        apply_update_channel(channel, cx);
        self.check_for_update(false, cx);
    }

    /// 后台刷新运行时那台机器上 Agent CLI 的诊断快照。
    ///
    /// 这条路径只运行固定的 `--version`，不启动 agent 或访问网络；因此设置页可
    /// 安全地自动首查，并允许用户手动复查 PATH 变动。
    pub fn refresh_acp_runtime(&mut self, cx: &mut Context<Self>) {
        if cx
            .try_global::<AcpRuntimeState>()
            .is_some_and(|state| state.refreshing || state.installing.is_some())
        {
            return;
        }
        cx.set_global(AcpRuntimeState {
            refreshing: true,
            ..cx.try_global::<AcpRuntimeState>()
                .cloned()
                .unwrap_or_default()
        });
        cx.spawn(async move |this, cx| {
            let diagnostics = cx
                .background_executor()
                .spawn(async { inspect_agent_clis() })
                .await;
            let _ = this.update(cx, |_this, cx| {
                cx.set_global(AcpRuntimeState {
                    diagnostics: diagnostics.or_else(|| {
                        cx.try_global::<AcpRuntimeState>()
                            .and_then(|state| state.diagnostics.clone())
                    }),
                    refreshing: false,
                    checked_at: Some(Instant::now()),
                    ..cx.try_global::<AcpRuntimeState>()
                        .cloned()
                        .unwrap_or_default()
                });
            });
        })
        .detach();
    }

    /// 用户确认后，在后台通过本机包管理器安装缺失的 Agent CLI。安装结束无论成功
    /// 与否都重新探测一次，因此 PATH 改动、包管理器成功但 CLI 不可执行等状态能
    /// 立即反映在设置页，而不是只报一个笼统的「完成」。
    pub fn install_acp_cli(&mut self, agent: ConversationAgentKind, cx: &mut Context<Self>) {
        if cx
            .try_global::<AcpRuntimeState>()
            .is_some_and(|state| state.installing.is_some() || state.refreshing)
        {
            return;
        }
        cx.set_global(AcpRuntimeState {
            installing: Some(agent),
            install_message: None,
            install_failed: false,
            ..cx.try_global::<AcpRuntimeState>()
                .cloned()
                .unwrap_or_default()
        });
        cx.spawn(async move |this, cx| {
            let (result, diagnostics) = cx
                .background_executor()
                .spawn(async move {
                    match crate::runtime_call::call::<smelt_model::control_api::AgentCliInstall>(
                        smelt_model::control_api::AgentCliInstallParams {
                            agent: agent.id().to_string(),
                        },
                        CLI_INSTALL_TIMEOUT,
                    ) {
                        Ok(installed) => (Ok(installed.installer), Some(installed.diagnostics)),
                        // 装到一半失败也可能已经改了东西，重新探测一次。
                        Err(error) => (
                            Err(crate::runtime_call::error_text(error)),
                            inspect_agent_clis(),
                        ),
                    }
                })
                .await;
            let _ = this.update(cx, |_this, cx| {
                // 探测不到可执行文件的不当作"没装"：
                // 安装成功就照直说成功，别让一个探不到的事实盖掉真实结果。
                let executable_available = diagnostics
                    .as_ref()
                    .and_then(|diagnostics| diagnostics.for_agent(agent))
                    .is_none_or(|executable| executable.is_available());
                let (install_message, install_failed) = match result {
                    Ok(installer) if executable_available => (
                        format!(
                            "{} 已通过 {installer} 安装；请在首次启动时完成登录。",
                            agent.label(),
                        )
                        .into(),
                        false,
                    ),
                    Ok(installer) => (
                        format!(
                            "{} 已通过 {installer} 安装，但当前搜索路径未找到 CLI；重启 Smelt 或检查 PATH 后重试检测。",
                            agent.label(),
                        )
                        .into(),
                        true,
                    ),
                    Err(error) => (format!("安装失败：{error}").into(), true),
                };
                cx.set_global(AcpRuntimeState {
                    diagnostics: diagnostics.or_else(|| {
                        cx.try_global::<AcpRuntimeState>()
                            .and_then(|state| state.diagnostics.clone())
                    }),
                    refreshing: false,
                    checked_at: Some(Instant::now()),
                    installing: None,
                    install_message: Some(install_message),
                    install_failed,
                });
            });
        })
        .detach();
    }

    /// 启动项条数变了就重建输入框（增删后调用）。
    pub fn reset_launch_inputs(&mut self) {
        self.launch_inputs = None;
    }

    /// 懒创建启动项列表编辑器（需要 window）。
    pub fn ensure_launch_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = cx.global::<LaunchConfig>().entries.len();
        let stale = self
            .launch_inputs
            .as_ref()
            .is_none_or(|i| i.rows.len() != count);
        if stale {
            self.init_launch_inputs(window, cx);
        }
    }

    fn init_launch_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputEvent, InputState};

        let entries = cx.global::<LaunchConfig>().entries.clone();
        let save_on = |ev: &InputEvent| matches!(ev, InputEvent::Change | InputEvent::Blur);
        let mut rows = Vec::new();
        let mut subs = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            let label_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("显示名称")
                    .default_value(entry.label.clone())
            });
            let command_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("启动命令，如 claude")
                    .default_value(entry.command.clone())
            });
            subs.push(
                cx.subscribe(&label_input, move |_, s, ev: &InputEvent, cx| {
                    if save_on(ev) {
                        let v = s.read(cx).value().to_string();
                        apply_launch_config(
                            |c| {
                                if let Some(e) = c.entries.get_mut(i) {
                                    e.label = v;
                                }
                            },
                            cx,
                        );
                    }
                }),
            );
            subs.push(
                cx.subscribe(&command_input, move |_, s, ev: &InputEvent, cx| {
                    if save_on(ev) {
                        let v = s.read(cx).value().to_string();
                        apply_launch_config(
                            |c| {
                                if let Some(e) = c.entries.get_mut(i) {
                                    e.command = v;
                                }
                            },
                            cx,
                        );
                    }
                }),
            );
            rows.push((
                label_input,
                command_input,
                !smelt_model::launch_settings::launch_entry_is_builtin(entry),
            ));
        }
        self.launch_inputs = Some(LaunchInputs { rows, _subs: subs });
    }

    pub fn add_launch_entry(&mut self, cx: &mut Context<Self>) {
        apply_launch_config(
            |c| {
                c.entries.push(LaunchEntry {
                    label: "新启动项".into(),
                    command: String::new(),
                    provider: None,
                });
            },
            cx,
        );
        self.reset_launch_inputs();
        cx.notify();
    }

    pub fn remove_launch_entry(&mut self, index: usize, cx: &mut Context<Self>) {
        apply_launch_config(
            |c| {
                remove_launch_entry_at(c, index);
            },
            cx,
        );
        self.reset_launch_inputs();
        cx.notify();
    }

    /// 自定义 Agent 条数变了就重建输入框（增删后调用）。
    pub fn reset_profile_inputs(&mut self) {
        self.profile_inputs = None;
    }

    /// 懒创建自定义 Agent 编辑器（需要 window）。
    pub fn ensure_profile_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<String> = super::agent_host_settings(&crate::RuntimeKey::Local, cx)
            .profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect();
        let stale = self.profile_inputs.as_ref().is_none_or(|inputs| {
            inputs.rows.len() != ids.len()
                || inputs.rows.iter().zip(&ids).any(|(row, id)| &row.id != id)
        });
        if stale {
            self.init_profile_inputs(window, cx);
        }
    }

    fn init_profile_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputEvent, InputState, TextareaState};

        let profiles = super::agent_host_settings(&crate::RuntimeKey::Local, cx)
            .profiles
            .clone();
        let save_on = |ev: &InputEvent| matches!(ev, InputEvent::Change | InputEvent::Blur);
        let mut rows = Vec::new();
        let mut subs = Vec::new();
        for p in &profiles {
            let label = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("名称")
                    .default_value(p.label.clone())
            });
            let command_placeholder = match p.kind() {
                Some(ConversationAgentKind::Custom) | None => {
                    "ACP 启动命令，如 my-agent --acp".to_string()
                }
                Some(kind) => format!("留空沿用 {} 的命令", kind.label()),
            };
            let command = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(command_placeholder)
                    .default_value(p.command.clone())
            });
            let env = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, 8)
                    .placeholder("每行一个 KEY=VALUE")
                    .default_value(format_env_lines(&p.env))
            });
            let dir = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("留空用默认目录")
                    .default_value(p.workspace_dir.clone())
            });
            subs.push(subscribe_profile_text(&label, p.id.clone(), cx, |p, v| {
                p.label = v;
            }));
            subs.push(subscribe_profile_text(
                &command,
                p.id.clone(),
                cx,
                |p, v| {
                    p.command = v;
                },
            ));
            subs.push(subscribe_profile_text(&dir, p.id.clone(), cx, |p, v| {
                p.workspace_dir = v;
            }));
            let id = p.id.clone();
            subs.push(cx.subscribe(&env, move |this, s, ev: &InputEvent, cx| {
                if !save_on(ev) {
                    return;
                }
                // 写到一半的变量名不合法很正常：只在能解析时存盘，错误挂在这一行上显示。
                let parsed = parse_env_lines(&s.read(cx).value());
                let error = parsed.as_ref().err().cloned();
                if let Some(row) = this
                    .profile_inputs
                    .as_mut()
                    .and_then(|inputs| inputs.rows.iter_mut().find(|row| row.id == id))
                    && row.env_error != error
                {
                    row.env_error = error;
                    cx.notify();
                }
                if let Ok(env) = parsed {
                    let id = id.clone();
                    apply_agent_host(
                        &crate::RuntimeKey::Local,
                        move |c| {
                            if let Some(p) = c.profiles.iter_mut().find(|p| p.id == id) {
                                p.env = env;
                            }
                        },
                        cx,
                    );
                }
            }));
            rows.push(ProfileRow {
                id: p.id.clone(),
                label,
                command,
                env,
                dir,
                env_error: None,
            });
        }
        self.profile_inputs = Some(ProfileInputs { rows, _subs: subs });
    }

    /// 新增一个自定义 Agent：默认是通用 ACP，要用户自己填启动命令；想沿用某个内置
    /// Agent 的适配器和历史，在类型里换过去即可。
    pub fn add_profile(&mut self, cx: &mut Context<Self>) {
        apply_agent_host(
            &crate::RuntimeKey::Local,
            |c| {
                c.profiles.push(AcpProfile {
                    id: uuid::Uuid::new_v4().to_string(),
                    kind_id: ConversationAgentKind::Custom.id().to_string(),
                    label: "新 Agent".into(),
                    command: String::new(),
                    env: Default::default(),
                    workspace_dir: String::new(),
                });
            },
            cx,
        );
        self.reset_profile_inputs();
        cx.notify();
    }

    /// 按 id 删：配置可能在渲染和点击之间被整份换掉，下标会指到别的 Agent。
    pub fn remove_profile(&mut self, id: String, cx: &mut Context<Self>) {
        apply_agent_host(
            &crate::RuntimeKey::Local,
            move |c| c.profiles.retain(|p| p.id != id),
            cx,
        );
        self.reset_profile_inputs();
        cx.notify();
    }

    /// 改某个自定义 Agent 的类型（下拉菜单选中项回调）。换到不支持数据目录的类型时
    /// 一并清掉目录，否则这一项就成了起不来的配置。
    pub fn set_profile_kind(
        &mut self,
        id: String,
        kind: ConversationAgentKind,
        cx: &mut Context<Self>,
    ) {
        apply_agent_host(
            &crate::RuntimeKey::Local,
            move |c| {
                if let Some(p) = c.profiles.iter_mut().find(|p| p.id == id) {
                    p.kind_id = kind.id().to_string();
                    if p.workspace_env_var().is_none() {
                        p.workspace_dir.clear();
                    }
                }
            },
            cx,
        );
        self.reset_profile_inputs();
        cx.notify();
    }
}

impl Workspace {
    /// 设置 / 清除背景图（不影响窗口透明度，故无需 window）。
    pub fn set_bg_image(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        apply_appearance(|a| a.bg_image = path, cx);
        cx.notify();
    }

    /// 弹原生选择框选一张背景图。
    pub fn pick_bg_image(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择背景图片".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths
                    .into_iter()
                    .next()
                    .and_then(|p| p.to_str().map(String::from))
            {
                this.update(cx, |this, cx| this.set_bg_image(Some(p), cx))
                    .ok();
            }
        })
        .detach();
    }
}

/// 装 CLI 要等包管理器下载，比普通调用久得多。
const CLI_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// 请运行时探测各家 agent CLI。连不上运行时返回 `None`，调用方保留上一次的结果。
fn inspect_agent_clis() -> Option<smelt_model::acp_conn::AcpRuntimeDiagnostics> {
    crate::runtime_call::call::<smelt_model::control_api::AgentCliInspect>(
        smelt_runtime_client::protocol::control::EmptyParams::default(),
        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
    )
    .inspect_err(|error| eprintln!("[settings] 探测 agent CLI 失败：{error}"))
    .ok()
}

/// 自定义 Agent 的单行输入框：改动和失焦都直接写回对应字段。
fn subscribe_profile_text(
    input: &Entity<gpui_component::input::InputState>,
    id: String,
    cx: &mut Context<Workspace>,
    apply: fn(&mut AcpProfile, String),
) -> Subscription {
    use gpui_component::input::InputEvent;
    cx.subscribe(input, move |_, s, ev: &InputEvent, cx| {
        if !matches!(ev, InputEvent::Change | InputEvent::Blur) {
            return;
        }
        let value = s.read(cx).value().to_string();
        let id = id.clone();
        apply_agent_host(
            &crate::RuntimeKey::Local,
            move |c| {
                if let Some(p) = c.profiles.iter_mut().find(|p| p.id == id) {
                    apply(p, value);
                }
            },
            cx,
        );
    })
}
