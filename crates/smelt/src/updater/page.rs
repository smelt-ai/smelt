//! 设置「系统 → 关于」页：版本、在线更新（macOS、Windows）、帮助与反馈；渲染由 NSH 主壳承担。

use gpui::{App, SharedString, Window};
use rust_i18n::t;
use serde_json::{Value, json};
#[cfg(any(target_os = "macos", windows))]
use smelt_plugin_api::updater::{self, UpdateFailure, UpdateStatus};
use smelt_plugin_api::{self as api, SettingsEntry, SettingsGroup, ViewSpec, text};
use smelt_ui::IconName;

#[cfg(any(target_os = "macos", windows))]
use super::update;
use super::{DOCS_URL, FEEDBACK_URL, OWNER, SOURCE_URL};

pub(crate) fn register(cx: &mut App) {
    let spec = ViewSpec {
        name: "about".into(),
        title: text!("updater.about.title"),
        icon: IconName::Info.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::System,
            keywords: [
                "软件版本",
                "应用版本",
                "版本",
                "更新",
                "检查更新",
                "更新通道",
                "自动下载安装",
                "升级",
                "下载",
                "通道",
                "生产版",
                "内测版",
                "自动更新",
                "帮助",
                "帮助文档",
                "文档",
                "反馈",
                "反馈问题",
                "日志",
                "源码",
                "GitHub",
                "about",
                "version",
                "update",
                "check for updates",
                "update channel",
                "beta",
                "auto update",
                "download",
                "help",
                "docs",
                "feedback",
                "report issue",
                "log",
                "source code",
            ]
            .into_iter()
            .map(SharedString::from)
            .collect(),
        }),
        fills: Vec::new(),
        build: crate::host_views::nsh_managed_view_builder(),
    };
    if let Err(error) = api::register_views(OWNER, vec![spec], cx) {
        api::app_log::error("updater", &format!("「关于」页登记失败：{error}"));
    }
}

pub(crate) fn snapshot(cx: &App) -> Value {
    #[cfg(any(target_os = "macos", windows))]
    let (version, update_json) = match update::updater(cx) {
        Some(entity) => {
            let this = entity.read(cx);
            let channel = this.prefs.channel;
            let version = format!(
                "v{} · {}",
                api::smelt_version::VERSION,
                channel_label(channel)
            );
            let update_json = build_update_json(this);
            (version, Some(update_json))
        }
        None => (format!("v{}", api::smelt_version::VERSION), None),
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let (version, update_json): (String, Option<Value>) = {
        let _ = cx;
        (format!("v{}", api::smelt_version::VERSION), None)
    };

    json!({
        "kind": "about",
        "version": version,
        "update": update_json,
        "help": {
            "section_title": t!("updater.section.help").into_owned(),
            "docs_row": t!("updater.help.docs_row").into_owned(),
            "docs_label": format!("{} ↗", t!("updater.docs")),
            "docs_url": DOCS_URL,
            "report_row": t!("updater.report_issue").into_owned(),
            "report_description": t!("updater.help.report_description").into_owned(),
            "open_log_label": t!("updater.help.open_log").into_owned(),
            "report_label": format!("{} ↗", t!("updater.report_issue")),
            "report_url": FEEDBACK_URL,
            "source_row": t!("updater.help.source").into_owned(),
            "source_label": "GitHub ↗",
            "source_url": SOURCE_URL,
        },
    })
}

pub(crate) fn handle_action(action: &Value, window: &mut Window, cx: &mut App) {
    let Some(kind) = action["kind"].as_str() else {
        return;
    };
    match kind {
        "open_log" => reveal_app_log(cx),
        #[cfg(any(target_os = "macos", windows))]
        "check" => {
            if let Some(entity) = update::updater(cx) {
                entity.update(cx, |this, cx| this.request_check(cx));
            }
        }
        #[cfg(any(target_os = "macos", windows))]
        "download" => {
            if let Some(entity) = update::updater(cx) {
                let candidate = match &entity.read(cx).status {
                    UpdateStatus::Available { version, url } => Some(updater::UpdateCandidate {
                        version: version.clone(),
                        url: url.clone(),
                    }),
                    _ => None,
                };
                if let Some(candidate) = candidate {
                    entity.update(cx, |this, cx| this.start_download(candidate, cx));
                }
            }
        }
        #[cfg(any(target_os = "macos", windows))]
        "restart" => {
            update::begin_ready_update_install(window, cx);
        }
        #[cfg(target_os = "macos")]
        "move_app" => {
            if let Some(entity) = update::updater(cx) {
                entity.update(cx, |this, cx| this.move_app_to_applications(cx));
            }
        }
        #[cfg(any(target_os = "macos", windows))]
        "set_channel" => {
            let channel = match action["value"].as_str() {
                Some("prod") => Some(updater::UpdateChannel::Prod),
                Some("dev") => Some(updater::UpdateChannel::Dev),
                _ => None,
            };
            if let (Some(channel), Some(entity)) = (channel, update::updater(cx)) {
                entity.update(cx, |this, cx| this.set_channel(channel, cx));
            }
        }
        #[cfg(any(target_os = "macos", windows))]
        "set_auto_install" => {
            if let (Some(checked), Some(entity)) =
                (action["checked"].as_bool(), update::updater(cx))
            {
                entity.update(cx, |this, cx| this.set_auto_install(checked, cx));
            }
        }
        _ => {
            let _ = window;
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
fn channel_key(channel: updater::UpdateChannel) -> &'static str {
    match channel {
        updater::UpdateChannel::Prod => "prod",
        updater::UpdateChannel::Dev => "dev",
    }
}

#[cfg(any(target_os = "macos", windows))]
fn channel_label(channel: updater::UpdateChannel) -> String {
    match channel {
        updater::UpdateChannel::Dev => t!("updater.channel.dev").into_owned(),
        updater::UpdateChannel::Prod => t!("updater.channel.prod").into_owned(),
    }
}

#[cfg(any(target_os = "macos", windows))]
fn failure_title(failure: UpdateFailure) -> String {
    match failure {
        UpdateFailure::Check => t!("updater.failure.check").into_owned(),
        UpdateFailure::Download => t!("updater.failure.download").into_owned(),
        UpdateFailure::Recovery => t!("updater.failure.recovery").into_owned(),
        UpdateFailure::Location => t!("updater.failure.location").into_owned(),
        UpdateFailure::NotInstalled => t!("updater.failure.not_installed").into_owned(),
    }
}

#[cfg(any(target_os = "macos", windows))]
fn failure_detail(failure: UpdateFailure) -> String {
    match failure {
        UpdateFailure::Check => t!("updater.failure.check_detail").into_owned(),
        UpdateFailure::Download => t!("updater.failure.download_detail").into_owned(),
        UpdateFailure::Recovery => t!("updater.failure.recovery_detail").into_owned(),
        UpdateFailure::Location => t!("updater.failure.location_detail").into_owned(),
        UpdateFailure::NotInstalled => t!("updater.failure.not_installed_detail").into_owned(),
    }
}

#[cfg(any(target_os = "macos", windows))]
fn build_update_json(this: &update::Updater) -> Value {
    let status = &this.status;
    let channel = this.prefs.channel;
    let auto_install = this.prefs.auto_install;
    #[cfg(target_os = "macos")]
    let (moving_app, move_app_error) = (this.moving_app, this.move_app_error.clone());
    #[cfg(windows)]
    let (moving_app, move_app_error): (bool, Option<SharedString>) = (false, None);

    let mb = |b: u64| b as f64 / 1024.0 / 1024.0;
    let (status_title, status_detail, status_tone) = match status {
        UpdateStatus::Idle => (
            t!("updater.status.idle").into_owned(),
            t!(
                "updater.status.idle_detail",
                channel = channel_label(channel)
            )
            .into_owned(),
            "muted",
        ),
        UpdateStatus::Checking => (
            t!("updater.status.checking").into_owned(),
            t!(
                "updater.status.checking_detail",
                channel = channel_label(channel)
            )
            .into_owned(),
            "muted",
        ),
        UpdateStatus::UpToDate => (
            t!("updater.status.up_to_date").into_owned(),
            t!(
                "updater.status.up_to_date_detail",
                channel = channel_label(channel)
            )
            .into_owned(),
            "ok",
        ),
        UpdateStatus::Available { version, .. } => (
            t!("updater.status.available").into_owned(),
            t!(
                "updater.status.available_detail",
                channel = channel_label(channel),
                version = version
            )
            .into_owned(),
            "muted",
        ),
        UpdateStatus::Downloading {
            version,
            received,
            total,
        } => {
            let detail = match total {
                Some(total) if *total > 0 => t!(
                    "updater.status.downloading_progress",
                    version = version,
                    percent = format!("{:.0}", *received as f64 / *total as f64 * 100.0),
                    received = format!("{:.1}", mb(*received)),
                    total = format!("{:.1}", mb(*total)),
                )
                .into_owned(),
                _ => t!(
                    "updater.status.downloading_bytes",
                    version = version,
                    received = format!("{:.1}", mb(*received)),
                )
                .into_owned(),
            };
            (
                t!("updater.status.downloading").into_owned(),
                detail,
                "muted",
            )
        }
        UpdateStatus::Installing { version } => (
            t!("updater.status.installing").into_owned(),
            t!("updater.status.installing_detail", version = version).into_owned(),
            "muted",
        ),
        UpdateStatus::Applying { version } => (
            t!("updater.status.applying").into_owned(),
            t!("updater.status.applying_detail", version = version).into_owned(),
            "muted",
        ),
        UpdateStatus::ReadyToInstall(update) => (
            t!("updater.status.ready").into_owned(),
            t!("updater.status.ready_detail", version = update.version).into_owned(),
            "ok",
        ),
        UpdateStatus::InstallFailed(update) => (
            t!("updater.status.install_failed").into_owned(),
            t!(
                "updater.status.install_failed_detail",
                version = update.version
            )
            .into_owned(),
            "bad",
        ),
        UpdateStatus::Failed(failure) => (
            failure_title(*failure),
            match (failure, &move_app_error) {
                (UpdateFailure::Location, Some(error)) => error.to_string(),
                _ => failure_detail(*failure),
            },
            "bad",
        ),
    };

    let progress = match status {
        UpdateStatus::Downloading {
            received,
            total: Some(total),
            ..
        } if *total > 0 => Some(json!({
            "value": *received as f32 / *total as f32 * 100.0,
            "indeterminate": false,
        })),
        UpdateStatus::Downloading { .. }
        | UpdateStatus::Installing { .. }
        | UpdateStatus::Applying { .. } => Some(json!({
            "value": null,
            "indeterminate": true,
        })),
        _ => None,
    };

    let can_check = status.can_check();
    let can_request_check = can_check || status.can_retry_recovery();
    let check_label = match status {
        UpdateStatus::Checking => t!("updater.check.checking").into_owned(),
        UpdateStatus::Downloading { .. } => t!("updater.check.downloading").into_owned(),
        UpdateStatus::Installing { .. } => t!("updater.check.installing").into_owned(),
        UpdateStatus::Applying { .. } => t!("updater.check.quitting").into_owned(),
        UpdateStatus::ReadyToInstall(_) | UpdateStatus::InstallFailed(_) => {
            t!("updater.check.again").into_owned()
        }
        UpdateStatus::Failed(UpdateFailure::Recovery) => {
            t!("updater.check.retry_recovery").into_owned()
        }
        _ => t!("updater.check.idle").into_owned(),
    };

    let download_label = matches!(status, UpdateStatus::Available { .. })
        .then(|| t!("updater.action.download").into_owned());
    let restart_label = status.ready_update().map(|_| {
        if matches!(status, UpdateStatus::InstallFailed(_)) {
            t!("updater.action.retry_restart").into_owned()
        } else {
            t!("updater.action.restart").into_owned()
        }
    });
    #[cfg(target_os = "macos")]
    let move_label = matches!(status, UpdateStatus::Failed(UpdateFailure::Location)).then(|| {
        if moving_app {
            t!("updater.move.moving").into_owned()
        } else {
            t!("updater.action.move").into_owned()
        }
    });
    #[cfg(windows)]
    let move_label: Option<String> = None;

    let manual_download_url = status
        .offers_manual_download()
        .then(|| updater::manual_download_page(channel))
        .flatten();
    let manual_download_label = manual_download_url
        .as_ref()
        .map(|_| t!("updater.action.manual_download").into_owned());

    let channels: Vec<Value> = updater::UpdateChannel::ALL
        .iter()
        .map(|option| {
            json!({
                "value": channel_key(*option),
                "label": channel_label(*option),
            })
        })
        .collect();

    json!({
        "section_title": t!("updater.section.update").into_owned(),
        "status_title": status_title,
        "status_detail": status_detail,
        "status_tone": status_tone,
        "progress": progress,
        "check_label": check_label,
        "can_check_request": can_request_check,
        "download_label": download_label,
        "restart_label": restart_label,
        "move_label": move_label,
        "moving_app": moving_app,
        "manual_download_label": manual_download_label,
        "manual_download_url": manual_download_url,
        "channel_label": t!("updater.channel.label").into_owned(),
        "channel_description": t!("updater.channel.description").into_owned(),
        "channels": channels,
        "selected_channel": channel_key(channel),
        "can_change_channel": can_check,
        "auto_install_label": t!("updater.auto_install.label").into_owned(),
        "auto_install_description": t!("updater.auto_install.description").into_owned(),
        "auto_install": auto_install,
    })
}

fn reveal_app_log(cx: &mut App) {
    let Some(path) = api::app_log::log_path() else {
        return;
    };
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        if let Err(error) = std::process::Command::new("open")
            .arg("-R")
            .arg(&path)
            .spawn()
        {
            api::app_log::error("updater", &format!("在访达里找出日志失败：{error}"));
        }
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(dir) = path.parent() {
        let opened = api::OPEN_IN_FILE_MANAGER.run(
            &api::OpenInFileManager {
                path: dir.to_path_buf(),
            },
            cx,
        );
        cx.spawn(async move |_: &mut gpui::AsyncApp| {
            if let Err(error) = opened.await {
                api::app_log::error("updater", &format!("打开日志所在目录失败：{error}"));
            }
        })
        .detach();
    }
}
