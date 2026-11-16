//! 设置「系统 → 关于」那一页：版本、在线更新（macOS、Windows）、帮助与反馈。
//!
//! 更新的状态在 [`crate::update::Updater`] 里，页面盯着它重绘，render 只读内存。

use std::rc::Rc;

use api::ui::Button;
#[cfg(any(target_os = "macos", windows))]
use api::ui::{Segmented, Switch};
#[cfg(any(target_os = "macos", windows))]
use api::updater::{self, UpdateFailure, UpdateStatus};
use api::{PluginView, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec};
use gpui::*;
use gpui_component::link::Link;
#[cfg(any(target_os = "macos", windows))]
use gpui_component::progress::Progress;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_component::*;
use smelt_plugin_api as api;

#[cfg(any(target_os = "macos", windows))]
use crate::update;
use crate::{DOCS_URL, FEEDBACK_URL, OWNER, SOURCE_URL};

pub(crate) fn register(cx: &mut App) {
    let spec = ViewSpec {
        name: "about".into(),
        title: "关于".into(),
        icon: IconName::Info.path(),
        per_machine: false,
        draws_top_bar: false,
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
            ]
            .into_iter()
            .map(SharedString::from)
            .collect(),
        }),
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
        let page = SettingPage::new("关于").resettable(false);
        #[cfg(any(target_os = "macos", windows))]
        let page = match update::updater(cx) {
            Some(updater) => {
                let shown = Shown::read(&updater, cx);
                page.group(header_group(Some(shown.channel)))
                    .group(update_group(updater, shown).title("更新"))
            }
            None => page.group(header_group(None)),
        };
        #[cfg(not(any(target_os = "macos", windows)))]
        let page = page.group(header_group());
        api::settings_pane(
            "updater-about",
            page.group(help_group().title("帮助与反馈")),
        )
    }
}

/// 页头：名字、版本和当前更新通道。版本号只在这里出现一次。
#[cfg(any(target_os = "macos", windows))]
fn header_group(channel: Option<updater::UpdateChannel>) -> SettingGroup {
    let version = match channel {
        Some(channel) => format!("v{} · {}", api::smelt_version::VERSION, channel.label()),
        None => format!("v{}", api::smelt_version::VERSION),
    };
    header(version)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn header_group() -> SettingGroup {
    header(format!("v{}", api::smelt_version::VERSION))
}

fn header(version: String) -> SettingGroup {
    SettingGroup::new().item(SettingItem::render(move |_, _, cx: &mut App| {
        v_flex()
            .gap_1()
            .child(div().text_base().font_semibold().child("Smelt"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(version.clone()),
            )
            .into_any_element()
    }))
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
fn update_group(entity: Entity<update::Updater>, shown: Shown) -> SettingGroup {
    SettingGroup::new().item(SettingItem::render(move |_, _, cx: &mut App| {
        let (fg, muted, border) = {
            let theme = cx.theme();
            (theme.foreground, theme.muted_foreground, theme.border)
        };
        let Shown {
            status,
            channel,
            auto_install,
            moving_app,
            move_app_error,
        } = shown.clone();

        // 字节数换算成 MB 展示，只在拿得到 Content-Length 时才有百分比。
        let mb = |b: u64| b as f64 / 1024.0 / 1024.0;
        let (status_title, status_detail, status_rgb) = match &status {
            UpdateStatus::Idle => (
                "尚未检查更新".to_string(),
                format!("点击“检查更新”获取{}的最新安装包", channel.label()),
                None,
            ),
            UpdateStatus::Checking => (
                "正在检查更新".to_string(),
                format!("正在获取{}的最新版本信息…", channel.label()),
                Some(api::ui::ui_theme::run()),
            ),
            UpdateStatus::UpToDate => (
                "当前已是最新".to_string(),
                format!("{}暂无可用更新", channel.label()),
                Some(api::ui::ui_theme::ok()),
            ),
            UpdateStatus::Available { version, .. } => (
                "发现新版本".to_string(),
                format!(
                    "{}已发布 {version}，点击“下载并更新”获取安装包",
                    channel.label()
                ),
                Some(api::ui::ui_theme::run()),
            ),
            UpdateStatus::Downloading {
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
                    Some(api::ui::ui_theme::run()),
                )
            }
            UpdateStatus::Installing { version } => (
                "正在准备更新".to_string(),
                format!("正在校验并准备更新包 {version}…"),
                Some(api::ui::ui_theme::run()),
            ),
            UpdateStatus::Applying { version } => (
                "正在启动更新安装器".to_string(),
                format!("正在为更新包 {version} 启动安装程序；启动成功后 App 才会退出"),
                Some(api::ui::ui_theme::run()),
            ),
            UpdateStatus::ReadyToInstall(update) => (
                "更新已准备就绪".to_string(),
                format!(
                    "更新包 {} 已下载完成，点击“立即重启更新”生效",
                    update.version
                ),
                Some(api::ui::ui_theme::ok()),
            ),
            UpdateStatus::InstallFailed(update) => (
                "安装更新失败".to_string(),
                format!(
                    "更新包 {} 仍已安全保留；可直接重试，或检查是否已有替代版本",
                    update.version
                ),
                Some(api::ui::ui_theme::bad()),
            ),
            UpdateStatus::Failed(failure) => (
                failure.title().to_string(),
                match (failure, &move_app_error) {
                    (UpdateFailure::Location, Some(error)) => error.to_string(),
                    _ => failure.detail().to_string(),
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
            } if *total > 0 => Some(
                Progress::new("update-progress").value(*received as f32 / *total as f32 * 100.0),
            ),
            UpdateStatus::Downloading { .. }
            | UpdateStatus::Installing { .. }
            | UpdateStatus::Applying { .. } => Some(Progress::new("update-progress").loading(true)),
            _ => None,
        };
        let can_check = status.can_check();
        let can_request_check = can_check || status.can_retry_recovery();
        let ready = status.ready_update().is_some();

        let check_label: &'static str = match &status {
            UpdateStatus::Checking => "检查中…",
            UpdateStatus::Downloading { .. } => "下载中…",
            UpdateStatus::Installing { .. } => "准备中…",
            UpdateStatus::Applying { .. } => "正在退出…",
            UpdateStatus::ReadyToInstall(_) | UpdateStatus::InstallFailed(_) => "检查新版本",
            UpdateStatus::Failed(UpdateFailure::Recovery) => "重试恢复",
            _ => "检查更新",
        };

        // 用分段控件让当前通道一眼可见；切换通道会立即检查，进行中的更新则锁定选择，
        // 避免下载源和状态提示在中途发生错配。
        let channel_entity = entity.clone();
        let channel_selector = Segmented::new("update-channel")
            .segments(updater::UpdateChannel::ALL.map(|option| option.label()))
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
                        .label("下载并更新")
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
                    "重试重启更新"
                } else {
                    "立即重启更新"
                })
                .on_click(|_, window, cx: &mut App| {
                    update::begin_ready_update_install(window, cx);
                })
        });

        // 位置不对时唯一的前进入口，和启动时那个确认框是同一个动作。
        #[cfg(target_os = "macos")]
        let move_btn =
            matches!(&status, UpdateStatus::Failed(UpdateFailure::Location)).then(|| {
                let move_entity = entity.clone();
                Button::new("move-app-to-applications")
                    .primary()
                    .small()
                    .label(if moving_app {
                        "正在移动…"
                    } else {
                        "移到「应用程序」并重新打开"
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
                    .label("去下载页")
                    .on_click(move |_, _, cx: &mut App| cx.open_url(&page))
            });

        let auto_entity = entity.clone();
        let auto_install_switch = Switch::new("auto-install-update")
            .checked(auto_install)
            .accessibility_label("自动下载安装")
            .on_change(move |checked, _, cx: &mut App| {
                auto_entity.update(cx, |this, cx| this.set_auto_install(checked, cx));
            });

        let action_row = h_flex()
            .gap_2()
            .items_center()
            .child(check_btn)
            .children(move_btn)
            .children(download_btn)
            .children(restart_btn)
            .children(manual_btn);

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
            .child(option_row(
                "更新通道",
                "生产版接收正式发布；内测版接收测试构建",
                channel_selector,
                (fg, muted, border),
            ))
            .child(option_row(
                "自动下载安装",
                "发现新版本后在后台下载；关闭后仅提示",
                auto_install_switch,
                (fg, muted, border),
            ))
            .into_any_element()
    }))
}

/// 更新节里的一行选项：左边名字和说明，右边控件，上面一条分隔线。
#[cfg(any(target_os = "macos", windows))]
fn option_row(
    title: &'static str,
    description: &'static str,
    control: impl IntoElement,
    (fg, muted, border): (Hsla, Hsla, Hsla),
) -> Div {
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
                        .child(div().text_sm().text_color(fg).child(title))
                        .child(div().text_xs().text_color(muted).child(description)),
                )
                .child(control),
        )
}

/// 帮助文档、反馈、源码。三个平台都有：Windows、Linux 没有菜单栏，帮助和反馈只能从这里进。
fn help_group() -> SettingGroup {
    SettingGroup::new().items(vec![
        SettingItem::new(
            "使用文档",
            SettingField::render(|_, _, _| {
                Link::new("settings-help-link")
                    .href(DOCS_URL)
                    .text_sm()
                    .child("帮助文档 ↗")
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
                            .small()
                            .label("打开日志")
                            .on_click(|_, _, cx: &mut App| reveal_app_log(cx)),
                    )
                    // 链接只管打开网页，不顺手做别的；找日志是旁边那个按钮。
                    .child(
                        Link::new("settings-report-issue-link")
                            .href(FEEDBACK_URL)
                            .text_sm()
                            .child("反馈问题 ↗"),
                    )
            }),
        )
        .description("先打开日志，再把它拖进反馈页"),
        SettingItem::new(
            "源码",
            SettingField::render(|_, _, _| {
                Link::new("settings-github-link")
                    .href(SOURCE_URL)
                    .text_sm()
                    .child("GitHub ↗")
            }),
        ),
    ])
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
