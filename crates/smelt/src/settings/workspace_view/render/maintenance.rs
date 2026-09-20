//! 设置页：系统（更新、后台服务、存储清理）。

use super::*;

pub(super) fn update_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let (update, _, _) = maintenance_groups(entity, snapshot, cx);
    SettingPage::new("应用更新")
        .description("检查 Smelt 版本，选择更新通道和自动下载方式。")
        .resettable(false)
        .group(update)
}

pub(super) fn daemon_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let (_, daemon, _) = maintenance_groups(entity, snapshot, cx);
    SettingPage::new("后台服务")
        .description("查看或维护承载本机终端与 Agent 会话的 smeltd 服务。")
        .resettable(false)
        .group(daemon)
}

pub(super) fn storage_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let (_, _, storage) = maintenance_groups(entity, snapshot, cx);
    SettingPage::new("存储清理")
        .description(
            "清理已经没人读写的残留：instincts 的 smelt.db、根目录\
             调试/备份文件和迁库 JSON 备份。不会删除仍在使用的 worktree 或 Git 分支。",
        )
        .resettable(false)
        .group(storage)
}

fn maintenance_groups(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> (SettingGroup, SettingGroup, SettingGroup) {
    let (fg, muted, border, popover) = {
        let t = cx.theme();
        (t.foreground, t.muted_foreground, t.border, t.popover)
    };
    let btn_base = move |id: &'static str, label: String| {
        div()
            .id(id)
            .h(px(26.))
            .px_3()
            .flex()
            .flex_none()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .text_color(fg)
            .bg(popover)
            .border_1()
            .border_color(border)
            .child(label)
    };
    let btn = move |id: &'static str, label: String| btn_base(id, label).hover(|s| s.bg(border));
    let btn_hover = move |id: &'static str, label: String, hover_bg: Hsla| {
        btn_base(id, label).hover(move |s| s.bg(hover_bg))
    };

    // —— 维护：更新、守护进程和残留数据清理 ——
    let update_entity = entity.clone();
    let update_status = snapshot.update_status.clone();
    let daemon_entity = entity;
    let update_group = SettingGroup::new().item(SettingItem::render(move |_, _, cx: &mut App| {
        let status = update_status.clone();
        let channel = cx.global::<UpdateSettings>().channel;
        let auto_install = cx.global::<UpdateSettings>().auto_install;
        let card_bg = rgb(crate::ui_theme::bg_column());
        let card_surface = rgb(crate::ui_theme::bg_card());
        let card_border = rgb(crate::ui_theme::border_mid());
        let accent = rgb(crate::ui_theme::action_fill());
        let accent_text = Hsla::from(rgb(crate::ui_theme::action_on()));

        // 字节数换算成 MB 展示，只在拿得到 Content-Length 时才有百分比。
        let mb = |b: u64| b as f64 / 1024.0 / 1024.0;
        let (status_title, status_detail, status_rgb) = match &status {
            updater::UpdateStatus::Idle => (
                "尚未检查更新".to_string(),
                format!("点击“检查更新”获取{}的最新安装包", channel.label()),
                None,
            ),
            updater::UpdateStatus::Checking => (
                "正在检查更新".to_string(),
                format!("正在获取{}的最新版本信息…", channel.label()),
                Some(crate::ui_theme::blue()),
            ),
            updater::UpdateStatus::UpToDate => (
                "当前已是最新".to_string(),
                format!("{}暂无可用更新", channel.label()),
                Some(crate::ui_theme::green()),
            ),
            updater::UpdateStatus::Available { version, .. } => (
                "发现新版本".to_string(),
                format!(
                    "{}已发布 {version}，点击“下载并更新”获取安装包",
                    channel.label()
                ),
                Some(crate::ui_theme::blue()),
            ),
            updater::UpdateStatus::Downloading {
                version,
                received,
                total,
            } => {
                let detail = match total {
                    Some(total) if *total > 0 => format!(
                        "正在下载更新包 {version}… {:.0}%（{:.1} / {:.1} MB）",
                        *received as f64 / *total as f64 * 100.0,
                        mb(*received),
                        mb(*total),
                    ),
                    _ => format!(
                        "正在下载更新包 {version}…（已下载 {:.1} MB）",
                        mb(*received)
                    ),
                };
                (
                    "正在下载更新".to_string(),
                    detail,
                    Some(crate::ui_theme::blue()),
                )
            }
            updater::UpdateStatus::Installing { version } => (
                "正在准备更新".to_string(),
                format!("正在解压并准备更新包 {version}…"),
                Some(crate::ui_theme::blue()),
            ),
            updater::UpdateStatus::Applying { version } => (
                "正在重启更新".to_string(),
                format!("正在安全交接会话并应用更新包 {version}…"),
                Some(crate::ui_theme::blue()),
            ),
            updater::UpdateStatus::WaitingForSafeHandoff(update) => (
                "等待 ACP 回合结束".to_string(),
                format!(
                    "更新包 {} 已准备；运行中的 Agent 回合结束后将自动重启，\
                             超过 {} 秒未结束会回到就绪状态，可随时再次点击",
                    update.version,
                    updater::SAFE_HANDOFF_WAIT_TIMEOUT.as_secs()
                ),
                Some(crate::ui_theme::yellow()),
            ),
            updater::UpdateStatus::ReadyToInstall(update) => (
                "更新已准备就绪".to_string(),
                format!(
                    "更新包 {} 已下载完成，点击“立即重启更新”生效",
                    update.version
                ),
                Some(crate::ui_theme::green()),
            ),
            updater::UpdateStatus::InstallFailed(update) => (
                "安装更新失败".to_string(),
                format!(
                    "更新包 {} 仍已安全保留；可直接重试，或检查是否已有替代版本",
                    update.version
                ),
                Some(crate::ui_theme::red()),
            ),
            updater::UpdateStatus::RestartRequired { version } => (
                "更新已安装".to_string(),
                format!("更新包 {version} 已生效到磁盘，请重试自动重启"),
                Some(crate::ui_theme::yellow()),
            ),
            updater::UpdateStatus::Failed(failure) => (
                failure.title().to_string(),
                failure.detail().to_string(),
                Some(crate::ui_theme::red()),
            ),
        };
        let status_color = status_rgb
            .map(|color| Hsla::from(rgb(color)))
            .unwrap_or(muted);

        // 进度条：能算出百分比就走确定进度，否则跑不确定的滑动动画。
        let progress_bar = match &status {
            updater::UpdateStatus::Downloading {
                received,
                total: Some(total),
                ..
            } if *total > 0 => Some(
                Progress::new("update-progress").value(*received as f32 / *total as f32 * 100.0),
            ),
            updater::UpdateStatus::Downloading { .. }
            | updater::UpdateStatus::Installing { .. }
            | updater::UpdateStatus::Applying { .. } => {
                Some(Progress::new("update-progress").loading(true))
            }
            _ => None,
        };
        let can_check = status.can_check();
        let can_request_check = can_check || status.can_retry_recovery();
        let ready = status.ready_update().is_some();
        let restart_required = matches!(&status, updater::UpdateStatus::RestartRequired { .. });

        let check_label: String = match &status {
            updater::UpdateStatus::Checking => "检查中…".into(),
            updater::UpdateStatus::Downloading { .. } => "下载中…".into(),
            updater::UpdateStatus::Installing { .. } => "准备中…".into(),
            updater::UpdateStatus::Applying { .. } => "重启中…".into(),
            updater::UpdateStatus::WaitingForSafeHandoff(_) => "等待回合结束…".into(),
            updater::UpdateStatus::InstallFailed(_) => "检查新版本".into(),
            updater::UpdateStatus::Failed(updater::UpdateFailure::Recovery) => "重试恢复".into(),
            _ => "检查更新".into(),
        };

        // 用分段控件让当前通道一眼可见；切换通道会立即检查，进行中的更新
        // 则锁定选择，避免下载源和状态提示在中途发生错配。
        let channel_locked = !can_check;
        let mut channel_selector = h_flex()
            .gap_1()
            .p(px(3.))
            .rounded_md()
            .bg(card_surface)
            .border_1()
            .border_color(card_border)
            .flex_none();
        for option in updater::UpdateChannel::ALL {
            let selected = channel == option;
            let channel_entity = update_entity.clone();
            let mut option_button = div()
                .id(match option {
                    updater::UpdateChannel::Dev => "update-channel-dev",
                    updater::UpdateChannel::Prod => "update-channel-prod",
                })
                .h(px(28.))
                .px_3()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .text_xs()
                .text_color(if selected { accent_text } else { muted })
                .bg(if selected { accent } else { card_surface })
                .child(option.label())
                .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                    if !channel_locked {
                        channel_entity.update(cx, |workspace, cx| {
                            workspace.set_update_channel(option, cx);
                        });
                    }
                });
            if !selected && !channel_locked {
                option_button = option_button.hover(|s| s.bg(card_bg).text_color(fg));
            }
            if channel_locked {
                option_button = option_button.opacity(0.55);
            }
            channel_selector = channel_selector.child(option_button);
        }

        let check_entity = update_entity.clone();
        let check_btn = btn_hover(
            "check-update",
            check_label,
            Hsla::from(crate::ui_theme::tint(crate::ui_theme::action_fill(), 0xe6)),
        )
        .text_color(if can_request_check {
            accent_text
        } else {
            muted
        })
        .bg(if can_request_check {
            accent
        } else {
            card_surface
        })
        .border_color(if can_request_check {
            accent
        } else {
            card_border
        })
        .when(!can_request_check, |button| button.opacity(0.55))
        .on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
            check_entity.update(cx, |this, cx| {
                if this.update_status.can_check() || this.update_status.can_retry_recovery() {
                    this.check_or_recover_update(false, cx);
                }
            });
        });
        // 关掉自动下载时唯一的前进入口：把 Available 里记下的那个包接进
        // 原有下载流程，后续状态流转和自动模式完全一致。
        let download_btn = match &status {
            updater::UpdateStatus::Available { version, url } => {
                let download_entity = update_entity.clone();
                let candidate = updater::UpdateCandidate {
                    version: version.clone(),
                    url: url.clone(),
                };
                Some(
                    btn_hover(
                        "download-update",
                        "下载并更新".into(),
                        Hsla::from(crate::ui_theme::tint(crate::ui_theme::blue(), 0x40)),
                    )
                    .text_color(rgb(crate::ui_theme::blue()))
                    .bg(Hsla::from(crate::ui_theme::tint(
                        crate::ui_theme::blue(),
                        0x24,
                    )))
                    .on_mouse_down(
                        MouseButton::Left,
                        move |_, _window, cx: &mut App| {
                            let candidate = candidate.clone();
                            download_entity.update(cx, |this, cx| {
                                this.start_update_download(candidate, cx);
                            });
                        },
                    ),
                )
            }
            _ => None,
        };

        let restart_entity = update_entity.clone();
        let restart_btn = (ready || restart_required).then(|| {
            btn_hover(
                "restart-update",
                if restart_required {
                    "重试自动重启".into()
                } else if matches!(&status, updater::UpdateStatus::InstallFailed(_)) {
                    "重试重启更新".into()
                } else {
                    "立即重启更新".into()
                },
                Hsla::from(crate::ui_theme::tint(crate::ui_theme::blue(), 0x40)),
            )
            .text_color(rgb(crate::ui_theme::blue()))
            .bg(Hsla::from(crate::ui_theme::tint(
                crate::ui_theme::blue(),
                0x24,
            )))
            .on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
                restart_entity.update(cx, |this, cx| {
                    if restart_required {
                        this.retry_update_relaunch(cx);
                    } else {
                        this.begin_ready_update_install(cx);
                    }
                });
            })
        });

        let cancel_wait_entity = update_entity.clone();
        let cancel_wait_btn = matches!(&status, updater::UpdateStatus::WaitingForSafeHandoff(_))
            .then(|| {
                btn_hover(
                    "cancel-update-wait",
                    "取消等待".into(),
                    Hsla::from(crate::ui_theme::tint(crate::ui_theme::yellow(), 0x40)),
                )
                .text_color(rgb(crate::ui_theme::yellow()))
                .bg(Hsla::from(crate::ui_theme::tint(
                    crate::ui_theme::yellow(),
                    0x24,
                )))
                .on_mouse_down(
                    MouseButton::Left,
                    move |_, _window, cx: &mut App| {
                        cancel_wait_entity.update(cx, |this, cx| {
                            this.cancel_update_install_wait(cx);
                        });
                    },
                )
            });

        let mut action_row = h_flex().gap_2().items_center().child(check_btn);
        if let Some(download_btn) = download_btn {
            action_row = action_row.child(download_btn);
        }
        if let Some(restart_btn) = restart_btn {
            action_row = action_row.child(restart_btn);
        }
        if let Some(cancel_wait_btn) = cancel_wait_btn {
            action_row = action_row.child(cancel_wait_btn);
        }

        let links = h_flex()
            .w_full()
            .gap_3()
            .items_center()
            .flex_wrap()
            .child(
                div()
                    .id("settings-github-link")
                    .text_xs()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(|s| s.text_color(fg))
                    .child("源码 GitHub ↗")
                    .on_mouse_down(MouseButton::Left, |_, _window, cx| {
                        cx.open_url("https://github.com/smelt-ai/smelt");
                    }),
            )
            .child(
                div()
                    .id("settings-help-link")
                    .text_xs()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(|s| s.text_color(fg))
                    .child("帮助文档 ↗")
                    .on_mouse_down(MouseButton::Left, |_, _window, cx| {
                        cx.open_url("https://smelt.onoo.io/");
                    }),
            )
            .child(
                div()
                    .id("settings-open-log-link")
                    .text_xs()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(|s| s.text_color(fg))
                    .child("打开日志 ↗")
                    .on_mouse_down(MouseButton::Left, |_, _window, _cx| {
                        // 反馈问题时手动带上这份日志：GitHub issue 附件靠拖拽，
                        // 没有 API 能替用户自动上传，所以帮用户直接定位到文件。
                        if let Some(path) = smelt_core::app_log::log_path() {
                            #[cfg(target_os = "macos")]
                            {
                                let _ = std::process::Command::new("open")
                                    .arg("-R")
                                    .arg(&path)
                                    .spawn();
                            }
                            #[cfg(not(target_os = "macos"))]
                            {
                                if let Some(dir) = path.parent() {
                                    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                                }
                            }
                        }
                    }),
            )
            .child(
                div()
                    .id("settings-report-issue-link")
                    .text_xs()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(|s| s.text_color(fg))
                    .child("反馈问题 ↗")
                    .on_mouse_down(MouseButton::Left, |_, _window, cx| {
                        // 先把日志在 Finder 里选出来，用户点开 issue 页面后可以直接把它拖进去。
                        if let Some(path) = smelt_core::app_log::log_path() {
                            #[cfg(target_os = "macos")]
                            {
                                let _ = std::process::Command::new("open")
                                    .arg("-R")
                                    .arg(&path)
                                    .spawn();
                            }
                        }
                        cx.open_url(FEEDBACK_URL);
                    }),
            );

        v_flex()
            .w_full()
            .gap_0()
            .child(
                h_flex()
                    .w_full()
                    .min_h(px(48.))
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .items_center()
                            .child(div().size(px(7.)).rounded_full().bg(status_color))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .font_medium()
                                    .text_color(status_color)
                                    .child(status_title),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(muted)
                            .child(concat!("当前版本 v", env!("CARGO_PKG_VERSION"))),
                    )
                    .child(action_row),
            )
            .child(
                div()
                    .w_full()
                    .pb_3()
                    .pl(px(15.))
                    .text_xs()
                    .text_color(muted)
                    .child(status_detail),
            )
            .children(progress_bar.map(|progress| div().w_full().pb_2().child(progress)))
            .child(
                div()
                    .w_full()
                    .min_h(px(52.))
                    .py_2()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(div().text_sm().text_color(fg).child("更新通道"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child("生产版接收正式发布；内测版接收测试构建"),
                                    ),
                            )
                            .child(channel_selector),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_h(px(52.))
                    .py_2()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(div().text_sm().text_color(fg).child("自动下载安装"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child("发现新版本后在后台下载；关闭后仅提示"),
                                    ),
                            )
                            .child(
                                Switch::new("auto-install-update")
                                    .checked(auto_install)
                                    .on_click(|checked, _window, cx: &mut App| {
                                        apply_auto_install(*checked, cx);
                                        cx.refresh_windows();
                                    }),
                            ),
                    ),
            )
            .child(links.pt_3().mt_1().border_t_1().border_color(border))
    }));

    let daemon_outdated = snapshot.daemon_outdated;
    let daemon_upgrading = snapshot.daemon_upgrading;
    let daemon_upgrade_msg = snapshot.daemon_upgrade_msg.clone();
    let daemon_info = snapshot.daemon_info.clone();
    let daemon_group = SettingGroup::new()
            .item(SettingItem::render(move |_, _, _cx: &mut App| {
                    let outdated = daemon_outdated;
                    let upgrading = daemon_upgrading;
                    let upgrade_msg = daemon_upgrade_msg.clone();
                    let upgrade_entity = daemon_entity.clone();
                    let restart_entity = daemon_entity.clone();
                    // 首选：无缝升级（exec 交接，会话不中断）。
                    let upgrade_daemon_btn = (outdated == Some(true)).then(|| {
                        btn(
                            "upgrade-daemon",
                            if upgrading { "升级中…".into() } else { "无缝升级".into() },
                        )
                        .when(!upgrading, |b| {
                            b.on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
                                upgrade_entity.update(cx, |this, cx| {
                                    this.upgrade_daemon_seamless(cx);
                                });
                            })
                        })
                    });
                    // 硬重启：常驻入口（守护卡死 / 想强制换二进制时用），会断会话。
                    // 不受版本是否落后限制；点击走二次确认弹窗兜底。
                    // 用 btn_hover：自定义 hover 色，避免在已有 hover 的 btn 上再链式 .hover() 崩。
                    let restart_daemon_btn = btn_hover(
                        "restart-daemon",
                        "重启守护进程".into(),
                        Hsla::from(crate::ui_theme::tint(crate::ui_theme::red(), 0x40)),
                    )
                        .text_color(rgb(crate::ui_theme::red()))
                        .bg(Hsla::from(crate::ui_theme::tint(crate::ui_theme::red(), 0x24)))
                        .on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
                            restart_entity.update(cx, |this, cx| {
                                this.show_daemon_restart_confirm = true;
                                cx.notify();
                            });
                        });
                    let status_text = match outdated {
                        Some(true) => "版本落后于当前安装包，升级守护后新功能/修复才生效。".to_string(),
                        Some(false) => "已是最新。".to_string(),
                        None => "检测中…".to_string(),
                    };
                    // 运行信息：守护没起就明说，别留空白让人以为没加载出来。
                    let info = daemon_info.clone();
                    let info_text = match (&info, outdated) {
                        (Some(i), _) => Some(daemon_info_line(i)),
                        // outdated 已探测完但拿不到 info → 守护确实没跑。
                        (None, Some(_)) => Some("未在运行（新建终端时会自动拉起）".to_string()),
                        (None, None) => None,
                    };
                    // 「N 个会话」不只是个数字——守护持有的会话不全是侧栏认领的
                    // （测试跑出来的游离会话、忘了关的临时会话也计在内），点开能看
                    // 到明细并单独清理，不用被迫走「重启守护进程」那种连坐所有
                    // 会话的核选项。守护没起来就没什么可看的，不露这个入口。
                    let manage_sessions_entity = daemon_entity.clone();
                    let manage_sessions_link = info.is_some().then(|| {
                        div()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(muted)
                            .hover(|s| s.text_color(fg))
                            .child("查看/清理会话 ›")
                            .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                                manage_sessions_entity.update(cx, |ws, cx| {
                                    ws.open_session_manager(cx);
                                });
                            })
                    });

                    v_flex()
                        .w_full()
                        .gap_3()
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .items_center()
                                .child(div().text_sm().text_color(fg).child("守护进程（smeltd）"))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .children(upgrade_daemon_btn)
                                        .child(restart_daemon_btn),
                                ),
                        )
                        .child(div().text_xs().text_color(muted).child(status_text))
                        .children(
                            info_text.map(|t| div().text_xs().text_color(muted).child(t)),
                        )
                        .children(manage_sessions_link)
                        .children(upgrade_msg.map(|m| div().text_xs().text_color(muted).child(m)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("「重启守护进程」会断开并终止当前所有终端会话（含正在跑的 agent）；若只是版本落后，优先用会话不中断的「无缝升级」。"),
                        )
            }));

    // —— 存储：`~/.smelt` 下的历史残留（老 schema / 老实现留下的遗留文件）扫描与清理 ——
    let storage_group = SettingGroup::new().item(SettingItem::render(move |_, _, cx: &mut App| {
        let state = cx.try_global::<CleanupState>().cloned().unwrap_or_default();

        let summary_text = match &state.scan {
            None => "还没扫描".to_string(),
            Some(s) if s.is_empty() => "没有发现残留数据".to_string(),
            Some(s) => format!(
                "发现 {} 处残留：废弃文件 {} 个、旧版 worktree 目录 {} 个、废弃目录 {} 个",
                s.total_items(),
                s.obsolete_files.len(),
                s.legacy_worktree_dirs.len(),
                s.obsolete_dirs.len(),
            ),
        };

        let has_findings = state.scan.as_ref().is_some_and(|s| !s.is_empty());

        let scan_btn = btn("storage-scan", "扫描残留数据".into()).on_mouse_down(
            MouseButton::Left,
            move |_, _window, cx: &mut App| {
                let scan = crate::storage_cleanup::scan();
                cx.set_global(CleanupState {
                    scan: Some(scan),
                    message: None,
                });
            },
        );

        let clean_btn = has_findings.then(|| {
            btn_hover(
                "storage-clean",
                "清理".into(),
                Hsla::from(crate::ui_theme::tint(crate::ui_theme::blue(), 0x40)),
            )
            .text_color(rgb(crate::ui_theme::blue()))
            .bg(Hsla::from(crate::ui_theme::tint(
                crate::ui_theme::blue(),
                0x24,
            )))
            .on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
                let Some(scan) = cx
                    .try_global::<CleanupState>()
                    .and_then(|state| state.scan.clone())
                else {
                    return;
                };
                let removed = crate::storage_cleanup::clean(&scan);
                cx.set_global(CleanupState {
                    scan: Some(crate::storage_cleanup::scan()),
                    message: Some(format!("已清理 {removed} 项").into()),
                });
            })
        });

        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().text_color(fg).child("~/.smelt 历史残留"))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(scan_btn)
                            .children(clean_btn),
                    ),
            )
            .child(div().text_xs().text_color(muted).child(summary_text))
            .children(
                state
                    .message
                    .map(|m| div().text_xs().text_color(muted).child(m)),
            )
    }));

    (update_group, daemon_group, storage_group)
}
