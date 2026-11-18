//! 设置「系统 → 关于」那一页：版本、在线更新（macOS、Windows）、帮助与反馈。
//!
//! 更新的状态在 [`crate::update::Updater`] 里，页面盯着它重绘，render 只读内存。

use std::rc::Rc;

#[cfg(any(target_os = "macos", windows))]
use api::ui::Progress;
use api::ui::{Button, SettingsPage, SettingsRow, SettingsSection, ui_theme};
use api::ui::{IconName, Link};
#[cfg(any(target_os = "macos", windows))]
use api::ui::{Segmented, Switch};
#[cfg(any(target_os = "macos", windows))]
use api::updater::{self, UpdateFailure, UpdateStatus};
use api::{PluginView, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec, text};
use gpui::*;
use rust_i18n::t;
use smelt_plugin_api as api;

#[cfg(any(target_os = "macos", windows))]
use crate::update;
use crate::{DOCS_URL, FEEDBACK_URL, OWNER, SOURCE_URL};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

pub(crate) fn register(cx: &mut App) {
    let spec = ViewSpec {
        name: "about".into(),
        title: text!("about.title"),
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
        build: Rc::new(|_, cx| Rc::new(cx.new(AboutPage::new)) as Rc<dyn ViewHandle>),
    };
    if let Err(error) = api::register_views(OWNER, vec![spec], cx) {
        api::app_log::error("updater", &format!("「关于」页登记失败：{error}"));
    }
}

struct AboutPage {
    _observe: Option<Subscription>,
}

impl AboutPage {
    fn new(cx: &mut Context<Self>) -> Self {
        #[cfg(any(target_os = "macos", windows))]
        let observe =
            update::updater(cx).map(|updater| cx.observe(&updater, |_, _, cx| cx.notify()));
        #[cfg(not(any(target_os = "macos", windows)))]
        let observe = None;
        Self { _observe: observe }
    }
}

impl PluginView for AboutPage {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

impl Render for AboutPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = SettingsPage::new("updater-about");
        #[cfg(any(target_os = "macos", windows))]
        let page = match update::updater(cx) {
            Some(updater) => {
                let shown = Shown::read(&updater, cx);
                page.child(header_section(Some(shown.channel)))
                    .child(update_section(updater, shown))
            }
            None => page.child(header_section(None)),
        };
        #[cfg(not(any(target_os = "macos", windows)))]
        let page = {
            let _ = cx;
            page.child(header_section())
        };
        page.child(help_section())
    }
}

/// 页头：名字、版本和当前更新通道。版本号只在这里出现一次。
#[cfg(any(target_os = "macos", windows))]
fn header_section(channel: Option<updater::UpdateChannel>) -> SettingsSection {
    let version = match channel {
        Some(channel) => format!(
            "v{} · {}",
            api::smelt_version::VERSION,
            channel_label(channel)
        ),
        None => format!("v{}", api::smelt_version::VERSION),
    };
    header(version)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn header_section() -> SettingsSection {
    header(format!("v{}", api::smelt_version::VERSION))
}

fn header(version: String) -> SettingsSection {
    SettingsSection::new().child(
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(ui_theme::text()))
                    .child("Smelt"),
            )
            .child(
                div()
                    .type_style(TYPE.ui)
                    .text_color(rgb(ui_theme::muted()))
                    .child(version),
            ),
    )
}

/// 更新通道给人看的名字。
#[cfg(any(target_os = "macos", windows))]
fn channel_label(channel: updater::UpdateChannel) -> SharedString {
    match channel {
        updater::UpdateChannel::Dev => t!("channel.dev").into(),
        updater::UpdateChannel::Prod => t!("channel.prod").into(),
    }
}

#[cfg(any(target_os = "macos", windows))]
fn failure_title(failure: UpdateFailure) -> std::borrow::Cow<'static, str> {
    match failure {
        UpdateFailure::Check => t!("failure.check"),
        UpdateFailure::Download => t!("failure.download"),
        UpdateFailure::Recovery => t!("failure.recovery"),
        UpdateFailure::Location => t!("failure.location"),
        UpdateFailure::NotInstalled => t!("failure.not_installed"),
    }
}

/// 失败的说明。只讲该怎么办，不带底层请求的地址。
#[cfg(any(target_os = "macos", windows))]
fn failure_detail(failure: UpdateFailure) -> std::borrow::Cow<'static, str> {
    match failure {
        UpdateFailure::Check => t!("failure.check_detail"),
        UpdateFailure::Download => t!("failure.download_detail"),
        UpdateFailure::Recovery => t!("failure.recovery_detail"),
        UpdateFailure::Location => t!("failure.location_detail"),
        UpdateFailure::NotInstalled => t!("failure.not_installed_detail"),
    }
}

/// 画更新那一节要的，render 时从更新状态里抄一份。
#[cfg(any(target_os = "macos", windows))]
#[derive(Clone)]
struct Shown {
    status: UpdateStatus,
    channel: updater::UpdateChannel,
    auto_install: bool,
    moving_app: bool,
    move_app_error: Option<SharedString>,
}

#[cfg(any(target_os = "macos", windows))]
impl Shown {
    fn read(updater: &Entity<update::Updater>, cx: &App) -> Self {
        let this = updater.read(cx);
        Self {
            status: this.status.clone(),
            channel: this.prefs.channel,
            auto_install: this.prefs.auto_install,
            #[cfg(target_os = "macos")]
            moving_app: this.moving_app,
            #[cfg(target_os = "macos")]
            move_app_error: this.move_app_error.clone(),
            // 「移到『应用程序』」只有 macOS 有；Windows 由安装程序装在固定位置。
            #[cfg(windows)]
            moving_app: false,
            #[cfg(windows)]
            move_app_error: None,
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
fn update_section(entity: Entity<update::Updater>, shown: Shown) -> SettingsSection {
    let muted = Hsla::from(rgb(ui_theme::muted()));
    let Shown {
        status,
        channel,
        auto_install,
        moving_app,
        move_app_error,
    } = shown;

    // 字节数换算成 MB 展示，只在拿得到 Content-Length 时才有百分比。
    let mb = |b: u64| b as f64 / 1024.0 / 1024.0;
    let (status_title, status_detail, status_rgb) = match &status {
        UpdateStatus::Idle => (
            t!("status.idle"),
            t!("status.idle_detail", channel = channel_label(channel)),
            None,
        ),
        UpdateStatus::Checking => (
            t!("status.checking"),
            t!("status.checking_detail", channel = channel_label(channel)),
            Some(api::ui::ui_theme::run()),
        ),
        UpdateStatus::UpToDate => (
            t!("status.up_to_date"),
            t!("status.up_to_date_detail", channel = channel_label(channel)),
            Some(api::ui::ui_theme::ok()),
        ),
        UpdateStatus::Available { version, .. } => (
            t!("status.available"),
            t!(
                "status.available_detail",
                channel = channel_label(channel),
                version = version
            ),
            Some(api::ui::ui_theme::run()),
        ),
        UpdateStatus::Downloading {
            version,
            received,
            total,
        } => {
            let detail = match total {
                Some(total) if *total > 0 => t!(
                    "status.downloading_progress",
                    version = version,
                    percent = format!("{:.0}", *received as f64 / *total as f64 * 100.0),
                    received = format!("{:.1}", mb(*received)),
                    total = format!("{:.1}", mb(*total)),
                ),
                _ => t!(
                    "status.downloading_bytes",
                    version = version,
                    received = format!("{:.1}", mb(*received)),
                ),
            };
            (
                t!("status.downloading"),
                detail,
                Some(api::ui::ui_theme::run()),
            )
        }
        UpdateStatus::Installing { version } => (
            t!("status.installing"),
            t!("status.installing_detail", version = version),
            Some(api::ui::ui_theme::run()),
        ),
        UpdateStatus::Applying { version } => (
            t!("status.applying"),
            t!("status.applying_detail", version = version),
            Some(api::ui::ui_theme::run()),
        ),
        UpdateStatus::ReadyToInstall(update) => (
            t!("status.ready"),
            t!("status.ready_detail", version = update.version),
            Some(api::ui::ui_theme::ok()),
        ),
        UpdateStatus::InstallFailed(update) => (
            t!("status.install_failed"),
            t!("status.install_failed_detail", version = update.version),
            Some(api::ui::ui_theme::bad()),
        ),
        UpdateStatus::Failed(failure) => (
            failure_title(*failure),
            match (failure, &move_app_error) {
                (UpdateFailure::Location, Some(error)) => error.to_string().into(),
                _ => failure_detail(*failure),
            },
            Some(api::ui::ui_theme::bad()),
        ),
    };
    let status_color = status_rgb
        .map(|color| Hsla::from(rgb(color)))
        .unwrap_or(muted);

    // 进度条：能算出百分比就走确定进度，否则跑不确定的滑动动画。
    let progress_bar = match &status {
        UpdateStatus::Downloading {
            received,
            total: Some(total),
            ..
        } if *total > 0 => {
            Some(Progress::new("update-progress").value(*received as f32 / *total as f32 * 100.0))
        }
        UpdateStatus::Downloading { .. }
        | UpdateStatus::Installing { .. }
        | UpdateStatus::Applying { .. } => {
            Some(Progress::new("update-progress").indeterminate(true))
        }
        _ => None,
    };
    let can_check = status.can_check();
    let can_request_check = can_check || status.can_retry_recovery();
    let ready = status.ready_update().is_some();

    let check_label = match &status {
        UpdateStatus::Checking => t!("check.checking"),
        UpdateStatus::Downloading { .. } => t!("check.downloading"),
        UpdateStatus::Installing { .. } => t!("check.installing"),
        UpdateStatus::Applying { .. } => t!("check.quitting"),
        UpdateStatus::ReadyToInstall(_) | UpdateStatus::InstallFailed(_) => t!("check.again"),
        UpdateStatus::Failed(UpdateFailure::Recovery) => t!("check.retry_recovery"),
        _ => t!("check.idle"),
    };

    // 用分段控件让当前通道一眼可见；切换通道会立即检查，进行中的更新则锁定选择，
    // 避免下载源和状态提示在中途发生错配。
    let channel_entity = entity.clone();
    let channel_selector = Segmented::new("update-channel")
        .segments(updater::UpdateChannel::ALL.map(channel_label))
        .selected(
            updater::UpdateChannel::ALL
                .iter()
                .position(|option| *option == channel),
        )
        .disabled(!can_check)
        .on_select(move |ix, _, cx| {
            if let Some(option) = updater::UpdateChannel::ALL.get(ix).copied() {
                channel_entity.update(cx, |this, cx| this.set_channel(option, cx));
            }
        });

    let check_entity = entity.clone();
    let check_btn = Button::new("check-update")
        .small()
        .label(check_label)
        .disabled(!can_request_check)
        .on_click(move |_, _, cx: &mut App| {
            check_entity.update(cx, |this, cx| this.request_check(cx));
        });
    // 关掉自动下载时唯一的前进入口：把 Available 里记下的那个包接进原有下载流程，
    // 后续状态流转和自动模式完全一致。
    let download_btn = match &status {
        UpdateStatus::Available { version, url } => {
            let download_entity = entity.clone();
            let candidate = updater::UpdateCandidate {
                version: version.clone(),
                url: url.clone(),
            };
            Some(
                Button::new("download-update")
                    .primary()
                    .small()
                    .label(t!("action.download"))
                    .on_click(move |_, _, cx: &mut App| {
                        let candidate = candidate.clone();
                        download_entity.update(cx, |this, cx| {
                            this.start_download(candidate, cx);
                        });
                    }),
            )
        }
        _ => None,
    };

    let restart_btn = ready.then(|| {
        Button::new("restart-update")
            .primary()
            .small()
            .label(if matches!(&status, UpdateStatus::InstallFailed(_)) {
                t!("action.retry_restart")
            } else {
                t!("action.restart")
            })
            .on_click(|_, window, cx: &mut App| {
                update::begin_ready_update_install(window, cx);
            })
    });

    // 位置不对时唯一的前进入口，和启动时那个确认框是同一个动作。
    #[cfg(target_os = "macos")]
    let move_btn = matches!(&status, UpdateStatus::Failed(UpdateFailure::Location)).then(|| {
        let move_entity = entity.clone();
        Button::new("move-app-to-applications")
            .primary()
            .small()
            .label(if moving_app {
                t!("move.moving")
            } else {
                t!("action.move")
            })
            .disabled(moving_app)
            .on_click(move |_, _, cx: &mut App| {
                move_entity.update(cx, |this, cx| this.move_app_to_applications(cx));
            })
    });
    #[cfg(windows)]
    let move_btn: Option<Button> = {
        let _ = moving_app;
        None
    };

    // 自动更新走不通时的退路：去发布页手动下载。
    let manual_btn = status
        .offers_manual_download()
        .then(|| updater::manual_download_page(channel))
        .flatten()
        .map(|page| {
            Button::new("manual-download-update")
                .small()
                .label(t!("action.manual_download"))
                .on_click(move |_, _, cx: &mut App| cx.open_url(&page))
        });

    let auto_entity = entity.clone();
    let auto_install_switch = Switch::new("auto-install-update")
        .checked(auto_install)
        .accessibility_label(t!("auto_install.label"))
        .on_change(move |checked, _, cx: &mut App| {
            auto_entity.update(cx, |this, cx| this.set_auto_install(checked, cx));
        });

    let action_row = div()
        .flex()
        .gap_2()
        .items_center()
        .child(check_btn)
        .children(move_btn)
        .children(download_btn)
        .children(restart_btn)
        .children(manual_btn);

    // 状态一格：圆点和标题在左、动作按钮在右，下面是说明和进度条。
    let status_cell = div()
        .flex()
        .flex_col()
        .w_full()
        .gap_1()
        .child(
            div()
                .flex()
                .w_full()
                .min_h(px(28.))
                .gap_2()
                .items_center()
                .flex_wrap()
                .child(
                    div()
                        .flex()
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
                                .type_style(TYPE.ui_medium)
                                .text_color(status_color)
                                .child(SharedString::from(status_title)),
                        ),
                )
                .child(action_row),
        )
        .child(
            div()
                .w_full()
                .pl(px(15.))
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(SharedString::from(status_detail)),
        )
        .children(progress_bar.map(|progress| div().w_full().pt_1().child(progress)));

    SettingsSection::new()
        .title(t!("section.update"))
        .child(status_cell)
        .child(
            SettingsRow::new(t!("channel.label"))
                .description(t!("channel.description"))
                .child(channel_selector),
        )
        .child(
            SettingsRow::new(t!("auto_install.label"))
                .description(t!("auto_install.description"))
                .child(auto_install_switch),
        )
}

/// 帮助文档、反馈、源码。三个平台都有：Windows、Linux 没有菜单栏，帮助和反馈只能从这里进。
fn help_section() -> SettingsSection {
    SettingsSection::new()
        .title(t!("section.help"))
        .child(
            SettingsRow::new(t!("help.docs_row")).child(
                Link::new("settings-help-link", DOCS_URL)
                    .type_style(TYPE.ui)
                    .child(format!("{} ↗", t!("docs"))),
            ),
        )
        .child(
            SettingsRow::new(t!("report_issue"))
                .description(t!("help.report_description"))
                .child(
                    Button::new("settings-open-log-link")
                        .quiet()
                        .small()
                        .label(t!("help.open_log"))
                        .on_click(|_, _, cx: &mut App| reveal_app_log(cx)),
                )
                // 链接只管打开网页，不顺手做别的；找日志是旁边那个按钮。
                .child(
                    Link::new("settings-report-issue-link", FEEDBACK_URL)
                        .type_style(TYPE.ui)
                        .child(format!("{} ↗", t!("report_issue"))),
                ),
        )
        .child(
            SettingsRow::new(t!("help.source")).child(
                Link::new("settings-github-link", SOURCE_URL)
                    .type_style(TYPE.ui)
                    .child("GitHub ↗"),
            ),
        )
}

/// 在文件管理器里找出日志。反馈问题时手动带上它：issue 附件靠拖拽，没有 API 能替用户
/// 自动上传。macOS 直接选中文件，别的平台打开所在目录。
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
        cx.spawn(async move |_: &mut AsyncApp| {
            if let Err(error) = opened.await {
                api::app_log::error("updater", &format!("打开日志所在目录失败：{error}"));
            }
        })
        .detach();
    }
}
