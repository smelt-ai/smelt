//! 设置页：维护（后台服务、旧版本残留）和关于（版本、应用更新、帮助与反馈）。

use super::*;
#[cfg(target_os = "macos")]
use gpui_component::tab::{Tab, TabBar};

pub(super) fn maintenance_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    SettingPage::new("维护")
        .resettable(false)
        .group(
            daemon_group(entity, snapshot)
                .title("后台服务")
                .description("smeltd 承载本机的终端和 Agent 会话。"),
        )
        .group(storage_group(cx).title("旧版本残留").description(
            "升级后没人再读写的文件。废弃文件启动时自动删；旧版 worktree 目录要你在这里确认。",
        ))
}

pub(super) fn about_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let page = SettingPage::new("关于").resettable(false).group(
        SettingGroup::new().item(SettingItem::render(|_, _, cx: &mut App| about_header(cx))),
    );
    // 应用更新只存在于 macOS（设计文档 D4）：替换的是 .app 包。别的平台整节不出。
    #[cfg(target_os = "macos")]
    let page = page.group(update_group(entity, snapshot, cx).title("更新"));
    #[cfg(not(target_os = "macos"))]
    let _ = (entity, snapshot, cx);
    page.group(help_group().title("帮助与反馈"))
}

/// 关于页头：名字、版本，macOS 上还有当前更新通道。版本号只在这里出现一次。
fn about_header(cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    #[cfg(target_os = "macos")]
    let version = format!(
        "v{} · {}",
        smelt_version::VERSION,
        cx.global::<UpdateSettings>().channel.label()
    );
    #[cfg(not(target_os = "macos"))]
    let version = format!("v{}", smelt_version::VERSION);
    v_flex()
        .gap_1()
        .child(div().text_base().font_semibold().child("Smelt"))
        .child(div().text_sm().text_color(muted).child(version))
        .into_any_element()
}

#[cfg(target_os = "macos")]
fn update_group(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingGroup {
    let (fg, muted, border) = {
        let t = cx.theme();
        (t.foreground, t.muted_foreground, t.border)
    };

    // —— 维护：更新、守护进程和残留数据清理 ——
    let update_entity = entity;
    let update_status = snapshot.update_status.clone();
    let moving_app = snapshot.moving_app;
    let move_app_error = snapshot.move_app_error.clone();
    SettingGroup::new().item(SettingItem::render(move |_, _, cx: &mut App| {
        let status = update_status.clone();
        let channel = cx.global::<UpdateSettings>().channel;
        let auto_install = cx.global::<UpdateSettings>().auto_install;

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
                "正在启动更新安装器".to_string(),
                format!("正在为更新包 {version} 创建受控安装事务；成功后 App 才会退出"),
                Some(crate::ui_theme::blue()),
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
            updater::UpdateStatus::Failed(failure) => (
                failure.title().to_string(),
                match (failure, &move_app_error) {
                    (updater::UpdateFailure::Location, Some(error)) => error.to_string(),
                    _ => failure.detail().to_string(),
                },
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

        let check_label: String = match &status {
            updater::UpdateStatus::Checking => "检查中…".into(),
            updater::UpdateStatus::Downloading { .. } => "下载中…".into(),
            updater::UpdateStatus::Installing { .. } => "准备中…".into(),
            updater::UpdateStatus::Applying { .. } => "正在退出…".into(),
            updater::UpdateStatus::InstallFailed(_) => "检查新版本".into(),
            updater::UpdateStatus::Failed(updater::UpdateFailure::Recovery) => "重试恢复".into(),
            _ => "检查更新".into(),
        };

        // 用分段控件让当前通道一眼可见；切换通道会立即检查，进行中的更新
        // 则锁定选择，避免下载源和状态提示在中途发生错配。
        let channel_locked = !can_check;
        let channel_entity = update_entity.clone();
        let channel_selector = TabBar::new("update-channel")
            .segmented()
            .flex_none()
            .when_some(
                updater::UpdateChannel::ALL
                    .iter()
                    .position(|option| *option == channel),
                |bar, ix| bar.selected_index(ix),
            )
            .on_click(move |ix, _, cx| {
                if channel_locked {
                    return;
                }
                if let Some(option) = updater::UpdateChannel::ALL.get(*ix).copied() {
                    channel_entity.update(cx, |workspace, cx| {
                        workspace.set_update_channel(option, cx);
                    });
                }
            })
            .children(
                updater::UpdateChannel::ALL
                    .map(|option| Tab::new().label(option.label()).disabled(channel_locked)),
            );

        let check_entity = update_entity.clone();
        let check_btn = Button::new("check-update")
            .normal()
            .compact()
            .label(check_label)
            .disabled(!can_request_check)
            .on_click(move |_, _window, cx: &mut App| {
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
                    Button::new("download-update")
                        .commit()
                        .compact()
                        .label("下载并更新")
                        .on_click(move |_, _window, cx: &mut App| {
                            let candidate = candidate.clone();
                            download_entity.update(cx, |this, cx| {
                                this.start_update_download(candidate, cx);
                            });
                        }),
                )
            }
            _ => None,
        };

        let restart_entity = update_entity.clone();
        let restart_btn = ready.then(|| {
            Button::new("restart-update")
                .commit()
                .compact()
                .label(
                    if matches!(&status, updater::UpdateStatus::InstallFailed(_)) {
                        "重试重启更新"
                    } else {
                        "立即重启更新"
                    },
                )
                .on_click(move |_, _window, cx: &mut App| {
                    restart_entity.update(cx, |this, cx| {
                        this.begin_ready_update_install(cx);
                    });
                })
        });

        // 位置不对时唯一的前进入口，和启动时那个确认框是同一个动作。
        let move_btn = matches!(
            &status,
            updater::UpdateStatus::Failed(updater::UpdateFailure::Location)
        )
        .then(|| {
            let move_entity = update_entity.clone();
            Button::new("move-app-to-applications")
                .commit()
                .compact()
                .label(if moving_app {
                    "正在移动…"
                } else {
                    "移到「应用程序」并重新打开"
                })
                .disabled(moving_app)
                .on_click(move |_, _window, cx: &mut App| {
                    move_entity.update(cx, |this, cx| this.move_app_to_applications(cx));
                })
        });

        let mut action_row = h_flex().gap_2().items_center().child(check_btn);
        if let Some(move_btn) = move_btn {
            action_row = action_row.child(move_btn);
        }
        if let Some(download_btn) = download_btn {
            action_row = action_row.child(download_btn);
        }
        if let Some(restart_btn) = restart_btn {
            action_row = action_row.child(restart_btn);
        }

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
                                    .accessibility_label("自动下载安装")
                                    .on_click(|checked, _window, cx: &mut App| {
                                        apply_auto_install(*checked, cx);
                                        cx.refresh_windows();
                                    }),
                            ),
                    ),
            )
    }))
}

/// 帮助文档、反馈、源码。三个平台都有：Windows、Linux 没有菜单栏，帮助和反馈只能从这里进。
fn help_group() -> SettingGroup {
    SettingGroup::new().items(vec![
        SettingItem::new(
            "使用文档",
            SettingField::render(|_, _, _| {
                Button::new("settings-help-link")
                    .link()
                    .icon_size()
                    .label("帮助文档 ↗")
                    .on_click(|_, _window, cx| cx.open_url(DOCS_URL))
            }),
        ),
        SettingItem::new(
            "反馈问题",
            SettingField::render(|_, _, _| {
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        Button::new("settings-open-log-link")
                            .quiet()
                            .icon_size()
                            .label("打开日志")
                            .on_click(|_, _window, _cx| reveal_app_log()),
                    )
                    .child(
                        Button::new("settings-report-issue-link")
                            .link()
                            .icon_size()
                            .label("反馈问题 ↗")
                            .on_click(|_, _window, cx| {
                                reveal_app_log();
                                cx.open_url(FEEDBACK_URL);
                            }),
                    )
            }),
        )
        .description("会同时在文件管理器里找出日志，拖进反馈页就行"),
        SettingItem::new(
            "源码",
            SettingField::render(|_, _, _| {
                Button::new("settings-github-link")
                    .link()
                    .icon_size()
                    .label("GitHub ↗")
                    .on_click(|_, _window, cx| {
                        cx.open_url("https://github.com/smelt-ai/smelt");
                    })
            }),
        ),
    ])
}

/// 在文件管理器里找出日志。反馈问题时手动带上它：issue 附件靠拖拽，没有 API 能替用户
/// 自动上传。macOS 直接选中文件，别的平台打开所在目录。
fn reveal_app_log() {
    let Some(path) = smelt_sys::app_log::log_path() else {
        return;
    };
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(&path)
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(dir) = path.parent() {
        let _ = crate::ide::open_in_file_manager(dir);
    }
}

fn daemon_group(entity: Entity<Workspace>, snapshot: &SettingsRenderSnapshot) -> SettingGroup {
    let daemon_outdated = snapshot.daemon_outdated;
    let daemon_upgrading = snapshot.daemon_upgrading;
    let daemon_upgrade_msg = snapshot.daemon_upgrade_msg.clone();
    let daemon_info = snapshot.daemon_info.clone();
    let mut group = SettingGroup::new();
    // 版本落后时，警示和「重启以升级」放在最上面：这是这一页唯一要你处理的事。
    if daemon_outdated == Some(true) {
        let upgrade_entity = entity.clone();
        group = group.item(
            SettingItem::new(
                "版本落后于当前安装包",
                SettingField::render(move |_, _, _| {
                    let upgrade_entity = upgrade_entity.clone();
                    Button::new("upgrade-daemon")
                        .compact()
                        .label(if daemon_upgrading {
                            "升级中…"
                        } else {
                            "重启以升级"
                        })
                        .loading(daemon_upgrading)
                        .disabled(daemon_upgrading)
                        .on_click(move |_, _window, cx: &mut App| {
                            upgrade_entity.update(cx, |this, cx| this.request_daemon_restart(cx));
                        })
                }),
            )
            .description(
                "重启后新版本才生效。进行中的回合会被中断，历史记录还在，重启后可以接着聊。",
            ),
        );
    }
    // 运行信息：守护没起就明说，别留空白让人以为没加载出来。
    let info_text = match (&daemon_info, daemon_outdated) {
        (Some(info), _) => daemon_info_line(info),
        // outdated 已探测完但拿不到 info → 守护确实没跑。
        (None, Some(_)) => "未在运行（新建终端时会自动拉起）".to_string(),
        (None, None) => "检测中…".to_string(),
    };
    let info_text = match daemon_upgrade_msg {
        Some(message) => format!("{info_text}\n{message}"),
        None => info_text,
    };
    let has_info = daemon_info.is_some();
    group.item(
        SettingItem::new(
            "smeltd",
            SettingField::render(move |_, _, _| {
                // 「N 个会话」不只是个数字——守护持有的会话不全是侧栏认领的（测试跑出来的
                // 游离会话、忘了关的临时会话也计在内），点开能看到明细并单独清理，不用被迫
                // 走重启那种连坐所有会话的核选项。守护没起来就没什么可看的，不露这个入口。
                let sessions_entity = entity.clone();
                let restart_entity = entity.clone();
                h_flex()
                    .gap_1()
                    .items_center()
                    .children(has_info.then(|| {
                        Button::new("settings-manage-daemon-sessions")
                            .quiet()
                            .compact()
                            .label("查看会话 ›")
                            .on_click(move |_, _window, cx| {
                                sessions_entity.update(cx, |ws, cx| ws.open_session_manager(cx));
                            })
                    }))
                    // 常驻入口（守护卡死 / 想强制换二进制时用）。有进行中的会话时先确认。
                    .child(
                        Button::new("restart-daemon")
                            .quiet()
                            .compact()
                            .label("重启")
                            .on_click(move |_, _window, cx: &mut App| {
                                restart_entity.update(cx, |this, cx| this.request_daemon_restart(cx));
                            }),
                    )
            }),
        )
        .description(info_text),
    )
    // 页脚本身就是小号灰字（见 SettingGroup::footer），不用再上色。
    .footer(|_, _| "重启时如果有进行中的会话，会先问你。")
}

fn storage_group(cx: &App) -> SettingGroup {
    // —— `~/.smelt` 下的历史残留（老 schema / 老实现留下的遗留文件）扫描与清理 ——
    let state = cx.try_global::<CleanupState>().cloned().unwrap_or_default();
    let summary = match &state.scan {
        None => "还没扫描".to_string(),
        Some(scan) if scan.is_empty() => "没有发现残留".to_string(),
        Some(scan) => format!(
            "发现 {} 处：废弃文件 {} 个、旧版 worktree 目录 {} 个、废弃目录 {} 个。不会删除 Git 分支。",
            scan.total_items(),
            scan.obsolete_files.len(),
            scan.legacy_worktree_dirs.len(),
            scan.obsolete_dirs.len(),
        ),
    };
    let summary = match &state.message {
        Some(message) => format!("{summary}\n{message}"),
        None => summary,
    };
    SettingGroup::new().item(
        SettingItem::new(
            "~/.smelt 里的残留",
            SettingField::render(move |_, _, cx: &mut App| {
                let state = cx.try_global::<CleanupState>().cloned().unwrap_or_default();
                let has_findings = state.scan.as_ref().is_some_and(|scan| !scan.is_empty());
                let scan_btn = Button::new("storage-scan")
                    .quiet()
                    .compact()
                    .label(if state.scan.is_some() {
                        "重新扫描"
                    } else {
                        "扫描"
                    })
                    .on_click(|_, _window, cx: &mut App| {
                        cx.set_global(CleanupState {
                            scan: Some(crate::storage_cleanup::scan()),
                            ..Default::default()
                        });
                    });
                let mut row = h_flex().gap_1().items_center().child(scan_btn);
                if has_findings && state.confirming {
                    row = row
                        .child(
                            Button::new("storage-clean-cancel")
                                .quiet()
                                .compact()
                                .label("取消")
                                .on_click(|_, _window, cx: &mut App| set_confirming(false, cx)),
                        )
                        .child(
                            Button::new("storage-clean-confirm")
                                .danger()
                                .compact()
                                .label("确认清理")
                                .on_click(|_, _window, cx: &mut App| {
                                    let Some(scan) = cx
                                        .try_global::<CleanupState>()
                                        .and_then(|state| state.scan.clone())
                                    else {
                                        return;
                                    };
                                    let removed = crate::storage_cleanup::clean(&scan);
                                    cx.set_global(CleanupState {
                                        scan: Some(crate::storage_cleanup::scan()),
                                        confirming: false,
                                        message: Some(format!("已清理 {removed} 项").into()),
                                    });
                                }),
                        );
                } else if has_findings {
                    row = row.child(
                        Button::new("storage-clean")
                            .danger_outline()
                            .compact()
                            .label("清理…")
                            .on_click(|_, _window, cx: &mut App| set_confirming(true, cx)),
                    );
                }
                row
            }),
        )
        .description(summary),
    )
}

/// 进入或退出「确认清理」。整份换掉全局，设置窗口跟着重绘（跟扫描按钮同一个做法）。
fn set_confirming(confirming: bool, cx: &mut App) {
    let mut state = cx.try_global::<CleanupState>().cloned().unwrap_or_default();
    state.confirming = confirming;
    cx.set_global(state);
}
