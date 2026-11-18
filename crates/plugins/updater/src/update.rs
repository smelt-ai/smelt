//! 在线更新的状态机：检查、静默下载、暂存、安装，以及 macOS 上「移到『应用程序』」。
//!
//! 状态是整个 App 一份（[`Updater`]，挂在全局上），不跟着主窗口走：主窗口关了再开，
//! 下载、待装的包都还在。检查、下载、拉起安装程序这些 IO 都在后台线程做，设置页只读这里
//! 的内存。平台的事（换 `.app`、拉起 Inno Setup）在 `api::updater` 里。
//!
//! 装更新要退出 App；macOS 还要先停掉本机后台服务（smeltd），正在输出的对话会被打断，
//! 所以有进行中的会话时先问。退出时有待装的包就顺手装上：退出守卫接手 ⌘Q（见
//! [`quit_guard`]），Windows 关掉最后一扇窗的退出在 `on_app_quit` 里接上。

use std::rc::Rc;

use api::model::daemon_state::{DaemonPhase, DaemonSessionState};
use api::ui::dialog::Confirm;
#[cfg(target_os = "macos")]
use api::ui::ui_theme;
use api::updater::{self, UpdateFailure, UpdateStatus};
use gpui::prelude::*;
use gpui::{App, AsyncApp, Context, Entity, Global, Window};
#[cfg(target_os = "macos")]
use gpui::{SharedString, div, rgb};
use rust_i18n::t;
use smelt_plugin_api as api;

use crate::store::{Prefs, UpdaterStore};
use crate::{ABOUT_VIEW, OWNER};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

/// 确认框里最多列出几个会话的名字。
const NAME_LIMIT: usize = 8;

/// 谁让装的：用户点「立即重启更新」的装完重新打开；退出时顺手装的不重开。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    User,
    Quit,
}

pub(crate) struct Updater {
    pub(crate) status: UpdateStatus,
    /// 每次开始安装加一，安装失败的回调对得上才改状态，旧的不覆盖新的。
    generation: u64,
    /// App 放的位置装不上更新（被系统转移、只读），启动时查一次。有问题时更新入口都停在
    /// 「需要先移到『应用程序』」。
    #[cfg(target_os = "macos")]
    pub(crate) location_issue: Option<api::app_location::LocationIssue>,
    /// 正在移到「应用程序」；移好就退出，从新位置重开。
    #[cfg(target_os = "macos")]
    pub(crate) moving_app: bool,
    /// 上次没移成的原因，确认框和设置页都显示。
    #[cfg(target_os = "macos")]
    pub(crate) move_app_error: Option<SharedString>,
    pub(crate) prefs: Prefs,
    store: UpdaterStore,
}

struct UpdaterGlobal(Entity<Updater>);

impl Global for UpdaterGlobal {}

/// 更新的状态。插件登记前是 `None`。
pub(crate) fn updater(cx: &App) -> Option<Entity<Updater>> {
    cx.try_global::<UpdaterGlobal>()
        .map(|global| global.0.clone())
}

pub(crate) fn register(cx: &mut App) {
    let store = UpdaterStore::open();
    let prefs = store.prefs();
    let entity = cx.new(|_| Updater {
        status: UpdateStatus::default(),
        generation: 0,
        #[cfg(target_os = "macos")]
        location_issue: None,
        #[cfg(target_os = "macos")]
        moving_app: false,
        #[cfg(target_os = "macos")]
        move_app_error: None,
        prefs,
        store,
    });
    cx.set_global(UpdaterGlobal(entity.clone()));
    // 有新版本（查到、在下、待装）就在「关于」页上挂角标；宿主据此在侧栏「设置」和设置
    // 导航里那一页标红点。
    cx.observe(&entity, |entity, cx| refresh_badge(&entity, cx))
        .detach();
    // 角标上的字跟语言走。
    let badge_owner = entity.downgrade();
    let events = api::events(cx);
    cx.subscribe(&events, move |_, event: &api::HostEvent, cx| {
        if matches!(event, api::HostEvent::LocaleChanged)
            && let Some(entity) = badge_owner.upgrade()
        {
            refresh_badge(&entity, cx);
        }
    })
    .detach();
    api::register_quit_guard(OWNER, Rc::new(quit_guard), cx);
    // Windows 上关掉主窗口就是退出，不经过退出守卫：在进程收尾时把待装的包交给安装程序。
    #[cfg(windows)]
    cx.on_app_quit(|cx| {
        install_ready_update_on_exit(cx);
        async {}
    })
    .detach();

    // 被系统转移、放在只读位置的 App 装不上更新：先提议移到「应用程序」，不恢复也不检查，
    // 免得下载了还是装不上。移过去重开后才走正常流程。
    #[cfg(target_os = "macos")]
    if let Some(issue) = api::app_location::current_issue() {
        entity.update(cx, |this, cx| {
            this.location_issue = Some(issue);
            this.status = UpdateStatus::Failed(UpdateFailure::Location);
            cx.notify();
        });
        // 插件在开主窗口之前登记：排到这一轮启动做完之后再弹。
        cx.spawn(async move |cx: &mut AsyncApp| cx.update(offer_move_app))
            .detach();
        return;
    }
    // 状态恢复、交换事务收敛和旧包清理由 updater 在同一把锁内完成；之后做常规静默检查，
    // 恢复出的待安装作业由检查决定保留还是换成更新源上更新的包。Windows 不是安装程序装的
    // （开发构建等）在检查时拦下，见 `blocked_by_app_location`。
    entity.update(cx, |this, cx| this.recover_update_at_launch(true, cx));
}

impl Updater {
    /// 有新版本要处理：查到了、在下、待装、正在装。
    pub(crate) fn update_available(&self) -> bool {
        self.status.staged_update().is_some()
            || matches!(
                self.status,
                UpdateStatus::Available { .. }
                    | UpdateStatus::Downloading { .. }
                    | UpdateStatus::Installing { .. }
                    | UpdateStatus::Applying { .. }
            )
    }

    /// App 放的位置装不上更新时，所有更新入口都停在「需要先移到『应用程序』」，不检查、
    /// 不下载、不安装：在那儿每一步都白做，最后还是写不进去。
    #[cfg(target_os = "macos")]
    fn blocked_by_app_location(&mut self, cx: &mut Context<Self>) -> bool {
        if self.location_issue.is_none() {
            return false;
        }
        self.status = UpdateStatus::Failed(UpdateFailure::Location);
        cx.notify();
        true
    }

    /// Windows 上不是安装程序装的（开发构建、拷出来的 exe）没有能覆盖的安装，所有更新
    /// 入口都停在「这个 Smelt 不能自动更新」，不检查、不下载、不安装。
    #[cfg(windows)]
    fn blocked_by_app_location(&mut self, cx: &mut Context<Self>) -> bool {
        if updater::installed_by_setup() {
            return false;
        }
        self.status = UpdateStatus::Failed(UpdateFailure::NotInstalled);
        cx.notify();
        true
    }

    /// 换更新通道并马上检查一次。通道只决定之后从哪儿取包，不改当前包显示的版本；新通道
    /// 指向别的包时照常按地址不同下载。进行中的更新锁着通道。
    pub(crate) fn set_channel(&mut self, channel: updater::UpdateChannel, cx: &mut Context<Self>) {
        if self.prefs.channel == channel || !self.status.can_check() {
            return;
        }
        self.prefs.channel = channel;
        self.store.save_prefs(self.prefs);
        self.check_for_update(false, cx);
    }

    pub(crate) fn set_auto_install(&mut self, auto_install: bool, cx: &mut Context<Self>) {
        self.prefs.auto_install = auto_install;
        self.store.save_prefs(self.prefs);
        cx.notify();
    }

    /// 设置页的「检查更新」、命令「检查更新」：能查就查，恢复失败的先重试恢复。
    pub(crate) fn request_check(&mut self, cx: &mut Context<Self>) {
        if self.status.can_check() || self.status.can_retry_recovery() {
            self.check_or_recover_update(false, cx);
        }
    }

    /// 「移到『应用程序』并重新打开」：拷过去、安排好等这个进程退出后从新位置重开，再退出。
    /// 后台会话在守护里，重开后照常接上。失败就留在原地，把原因给人看。
    #[cfg(target_os = "macos")]
    pub(crate) fn move_app_to_applications(&mut self, cx: &mut Context<Self>) {
        if self.moving_app {
            return;
        }
        self.moving_app = true;
        self.move_app_error = None;
        cx.notify();
        // 启动时那个确认框不看这份状态，靠重绘跟上。
        cx.refresh_windows();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async {
                    let app = api::app_location::move_to_applications()?;
                    api::app_location::relaunch_after_exit(&app)?;
                    anyhow::Ok(app)
                })
                .await;
            match result {
                Ok(app) => {
                    api::app_log::info(
                        "app-location",
                        &format!("已移到 {}，退出后从那里重新打开", app.display()),
                    );
                    cx.update(quit_now);
                }
                Err(error) => {
                    api::app_log::error(
                        "app-location",
                        &format!("移到「应用程序」失败：{error:#}"),
                    );
                    let _ = this.update(cx, |this, cx| {
                        this.moving_app = false;
                        this.move_app_error =
                            Some(t!("move.failed", error = format!("{error:#}")).into());
                        cx.notify();
                        cx.refresh_windows();
                    });
                }
            }
        })
        .detach();
    }

    /// 检查是否有新版本。`silent` 区分启动时的后台静默检查（离线、失败时不打扰，悄悄退回
    /// Idle）和手动点「检查更新」（失败要如实展示原因）。
    ///
    /// 开着「自动下载安装」时发现新版本直接接上后台静默下载；关掉则停在 `Available`，只亮
    /// 出版本号等用户自己决定何时下载。检查本身两种情况下都照做。
    fn check_for_update(&mut self, silent: bool, cx: &mut Context<Self>) {
        if self.blocked_by_app_location(cx) {
            return;
        }
        if !self.status.can_check() {
            return;
        }
        // 暂存着包（待安装或安装失败）时记下原状态：检查失败、或更新源仍指向它，都回到原状态。
        let staged = self
            .status
            .staged_update()
            .cloned()
            .map(|update| (self.status.clone(), update));
        self.status = UpdateStatus::Checking;
        let channel = self.prefs.channel;
        cx.notify();
        cx.spawn(async move |this, cx| {
            // 本机已装哪个发布要读包里的文件、拿更新锁读状态库，和查网络一样放后台。
            let result = cx
                .background_executor()
                .spawn(async move {
                    let installed = updater::installed_release()?;
                    let latest = api::block_on::block_on_tokio(updater::fetch_latest(channel))
                        .and_then(|r| r);
                    anyhow::Ok((installed, latest))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let (installed, result) = match result {
                    Ok(checked) => checked,
                    Err(error) => {
                        api::app_log::error(
                            "updater",
                            &format!("读取本机更新状态失败，暂停检查：{error:#}"),
                        );
                        this.status = staged.map_or(
                            UpdateStatus::Failed(UpdateFailure::Recovery),
                            |(status, _)| status,
                        );
                        cx.notify();
                        return;
                    }
                };
                match result {
                    Ok(candidate) => {
                        // 读的是此刻的开关而不是发起检查时的：检查期间刚把自动更新关掉，
                        // 就不该再被一个后台下载打脸。
                        let auto_install = this.prefs.auto_install;
                        if let Some((status, update)) = staged {
                            match updater::decide_staged_update_check(
                                candidate,
                                &installed,
                                &update,
                                auto_install,
                            ) {
                                Some(outcome) => {
                                    this.replace_staged_update_after_check(
                                        status, update, outcome, cx,
                                    );
                                }
                                None => {
                                    this.status = status;
                                    cx.notify();
                                }
                            }
                            return;
                        }
                        let outcome =
                            updater::decide_check_outcome(candidate, &installed, auto_install);
                        this.apply_check_outcome(outcome, cx);
                        return;
                    }
                    Err(error) => {
                        api::app_log::error(
                            "updater",
                            &format!("检查 {channel:?} 更新失败：{error:#}"),
                        );
                        this.status = if let Some((status, _)) = staged {
                            status
                        } else if silent {
                            UpdateStatus::Idle
                        } else {
                            UpdateStatus::Failed(UpdateFailure::Check)
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_check_outcome(&mut self, outcome: updater::CheckOutcome, cx: &mut Context<Self>) {
        match outcome {
            updater::CheckOutcome::Download(candidate) => {
                self.start_download(candidate, cx);
            }
            updater::CheckOutcome::Notify(candidate) => {
                self.status = UpdateStatus::Available {
                    version: candidate.version,
                    url: candidate.url,
                };
                cx.notify();
            }
            updater::CheckOutcome::NoUpdate => {
                self.status = UpdateStatus::UpToDate;
                cx.notify();
            }
        }
    }

    /// 只在更新源已经指向另一份发布包后进入。`status` 是检查前的待安装或安装失败状态。
    /// 作废旧作业放到后台线程，成功后才开始新下载，保证 begin_staged_update 永远不会撞上
    /// 旧事务；作废失败就回到原状态。
    fn replace_staged_update_after_check(
        &mut self,
        status: UpdateStatus,
        update: updater::StagedUpdate,
        outcome: updater::CheckOutcome,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let update_for_work = update.clone();
            let result = cx
                .background_executor()
                .spawn(async move { updater::discard_staged_update(&update_for_work) })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    api::app_log::info(
                        "updater",
                        &format!(
                            "更新源已换掉暂存的包 {}，作废旧作业并继续检查结果",
                            update.version
                        ),
                    );
                    this.apply_check_outcome(outcome, cx);
                }
                Err(error) => {
                    api::app_log::error(
                        "updater",
                        &format!("作废暂存的更新 {} 失败：{error:#}", update.version),
                    );
                    this.status = status;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 恢复失败时先收敛原事务；暂存着包（待安装或安装失败）则由检查拉取更新源，确认发布
    /// 地址变了才作废旧作业，不能绕过持久化事务直接另起下载。
    fn check_or_recover_update(&mut self, silent: bool, cx: &mut Context<Self>) {
        if self.blocked_by_app_location(cx) {
            return;
        }
        if self.status.can_retry_recovery() {
            self.recover_update_at_launch(silent, cx);
        } else {
            self.check_for_update(silent, cx);
        }
    }

    /// 后台静默下载新版并准备好（macOS 暂存 `.app`，Windows 校验安装程序），完成后置
    /// `ReadyToInstall`（不重启、不打断）。下载线程经 channel 往回推进度；发送端随下载结束
    /// 而放掉，收不到了就是下载收尾，再等任务拿最终结果。
    pub(crate) fn start_download(
        &mut self,
        candidate: updater::UpdateCandidate,
        cx: &mut Context<Self>,
    ) {
        if self.blocked_by_app_location(cx) {
            return;
        }
        let version = candidate.version;
        let url = candidate.url;
        self.status = UpdateStatus::Downloading {
            version: version.clone(),
            received: 0,
            total: None,
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (tx, rx) = smol::channel::unbounded::<updater::DownloadProgress>();
            let task_version = version.clone();
            let task = cx.background_executor().spawn(async move {
                api::block_on::block_on_tokio(updater::download_and_stage(
                    &url,
                    &task_version,
                    |progress| {
                        let _ = tx.try_send(progress);
                    },
                ))
                .and_then(|r| r)
            });

            while let Ok(progress) = rx.recv().await {
                let version = version.clone();
                let _ = this.update(cx, |this, cx| {
                    this.status = match progress {
                        updater::DownloadProgress::Bytes { received, total } => {
                            UpdateStatus::Downloading {
                                version,
                                received,
                                total,
                            }
                        }
                        updater::DownloadProgress::Installing => {
                            UpdateStatus::Installing { version }
                        }
                    };
                    cx.notify();
                });
            }

            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(update) => UpdateStatus::ReadyToInstall(update),
                    Err(error) => {
                        api::app_log::error(
                            "updater",
                            &format!("下载并准备更新包 {version} 失败：{error:#}"),
                        );
                        UpdateStatus::Failed(UpdateFailure::Download)
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn recover_update_at_launch(&mut self, silent_check: bool, cx: &mut Context<Self>) {
        self.status = UpdateStatus::Checking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { updater::recover_pending_update() })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(Some(update)) => {
                    api::app_log::info(
                        "updater",
                        &format!("恢复待安装更新 {}，等待用户显式重试", update.version),
                    );
                    // 安装程序持续失败不能变成「拉起旧 App → 自动退出 → 再失败」的循环：
                    // 恢复只亮出待装的作业，用户明确点了才开始新的一次。
                    this.status = if update.installer_failed() {
                        UpdateStatus::InstallFailed(update)
                    } else {
                        UpdateStatus::ReadyToInstall(update)
                    };
                    // 暂存后更新源可能又发了新版：照样检查，换了包就作废这条再下新的。
                    this.check_for_update(silent_check, cx);
                }
                Ok(None) => {
                    this.status = UpdateStatus::Idle;
                    this.check_for_update(silent_check, cx);
                }
                Err(error) => {
                    api::app_log::error(
                        "updater",
                        &format!("恢复更新事务失败，暂停新更新作业：{error:#}"),
                    );
                    this.status = UpdateStatus::Failed(UpdateFailure::Recovery);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 拉起安装程序，成功就退出 App，换包在这个进程完全退出后进行。用户点的、退出时顺手装
    /// 的都从这里进同一条安装流程。
    fn start_install(
        &mut self,
        update: updater::StagedUpdate,
        trigger: Trigger,
        cx: &mut Context<Self>,
    ) {
        let version = update.version.clone();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.status = UpdateStatus::Applying {
            version: version.clone(),
        };
        cx.notify();

        cx.spawn(async move |this, cx| {
            let relaunch = trigger == Trigger::User;
            let update_for_installer = update.clone();
            let result = cx
                .background_executor()
                .spawn(async move { updater::launch_installer(&update_for_installer, relaunch) })
                .await;
            match result {
                Ok(ticket) => {
                    api::app_log::info(
                        "updater",
                        &format!("installer 已接管更新 {version}（{ticket}），等待 GUI 退出"),
                    );
                    // 安装程序已经起来了。macOS 先停掉旧守护再退出：换文件时不能还被旧进程
                    // 占着。没起来不走这里。
                    if updater::INSTALL_STOPS_LOCAL_DAEMON {
                        let stopped = cx
                            .background_executor()
                            .spawn(async { updater::shutdown_local_daemon() })
                            .await;
                        if !stopped {
                            api::app_log::error(
                                "updater",
                                "退出前没能确认旧守护已退出，安装器换包前会再停一次",
                            );
                        }
                    }
                    cx.update(quit_now);
                }
                Err(error) => {
                    api::app_log::error(
                        "updater",
                        &format!("启动 installer 失败，当前 App 未替换：{error:#}"),
                    );
                    let _ = this.update(cx, |this, cx| {
                        if this.generation == generation
                            && matches!(
                                &this.status,
                                UpdateStatus::Applying { version: current } if current == &version
                            )
                        {
                            this.status = UpdateStatus::InstallFailed(update);
                            cx.notify();
                        }
                    });
                    // 用户要退出，装不上也照样退出。
                    if trigger == Trigger::Quit {
                        cx.update(quit_now);
                    }
                }
            }
        })
        .detach();
    }
}

/// 设置页「立即重启更新」：没有进行中的会话、或者装更新不停守护（Windows），直接装；否则
/// 先确认。
pub(crate) fn begin_ready_update_install(window: &mut Window, cx: &mut App) {
    let Some(entity) = updater(cx) else {
        return;
    };
    let blocked = entity.update(cx, |this, cx| this.blocked_by_app_location(cx));
    if blocked || entity.read(cx).status.ready_update().is_none() {
        return;
    }
    let names = local_busy_work_names(cx);
    if updater::INSTALL_STOPS_LOCAL_DAEMON && !names.is_empty() {
        Confirm::new(
            t!("confirm.restart_title"),
            t!("confirm.restart"),
            |_, cx| {
                install_ready_update(Trigger::User, cx);
                true
            },
        )
        .danger()
        .description(stop_prompt_body(&names))
        .open(window, cx);
        return;
    }
    install_ready_update(Trigger::User, cx);
}

/// 用户要退出（⌘Q）时宿主先问这里：有待装的包就接手，装上再退出。
fn quit_guard(cx: &mut App) -> bool {
    let Some(entity) = updater(cx) else {
        return false;
    };
    let status = entity.read(cx).status.clone();
    // 安装程序已经起来、App 正在退出，不再起第二条退出或安装流程。
    if matches!(status, UpdateStatus::Applying { .. }) {
        return true;
    }
    if status.ready_update().is_none() {
        return false;
    }
    // 退出时装更新会换包，macOS 还会停掉守护。没有进行中的会话、或者不停守护，就直接走；
    // 否则先确认。宿主正在处理退出，确认框排到这之后再弹。
    let names = local_busy_work_names(cx);
    if !updater::INSTALL_STOPS_LOCAL_DAEMON || names.is_empty() {
        install_ready_update(Trigger::Quit, cx);
        return true;
    }
    let body = stop_prompt_body(&names);
    cx.defer(move |cx| {
        open_confirm(cx, move |window, cx| {
            Confirm::new(t!("confirm.quit_title"), t!("confirm.quit"), |_, cx| {
                install_ready_update(Trigger::Quit, cx);
                true
            })
            .danger()
            .description(body.clone())
            .open(window, cx);
        });
    });
    true
}

/// 装此刻待装的那个包。确认框开着的时候状态可能变了：没有待装的了，用户点的就算了，要退出
/// 的照样退出；已经在装的不再装第二次。
fn install_ready_update(trigger: Trigger, cx: &mut App) {
    let Some(entity) = updater(cx) else {
        return;
    };
    let status = entity.read(cx).status.clone();
    if matches!(status, UpdateStatus::Applying { .. }) {
        return;
    }
    match status.ready_update().cloned() {
        Some(update) => entity.update(cx, |this, cx| this.start_install(update, trigger, cx)),
        None if trigger == Trigger::Quit => quit_now(cx),
        None => {}
    }
}

/// 关掉主窗口时还有待装的更新就拉起安装程序，不重新打开。进程马上要没了，这里同步拉起；
/// Windows 换文件不用停守护（见 `updater::INSTALL_STOPS_LOCAL_DAEMON`）。
#[cfg(windows)]
fn install_ready_update_on_exit(cx: &mut App) {
    let Some(entity) = updater(cx) else {
        return;
    };
    let this = entity.read(cx);
    if matches!(this.status, UpdateStatus::Applying { .. }) {
        return;
    }
    let Some(update) = this.status.ready_update() else {
        return;
    };
    match updater::launch_installer(update, false) {
        Ok(ticket) => api::app_log::info(
            "updater",
            &format!("退出时拉起安装程序装 {}（{ticket}）", update.version),
        ),
        Err(error) => api::app_log::error(
            "updater",
            &format!(
                "退出时拉起安装程序失败，{} 下次再装：{error:#}",
                update.version
            ),
        ),
    }
}

/// 启动时提议把 App 移到「应用程序」：被系统转移或放在只读位置时更新装不上。取消只关这
/// 一次，「关于」页的更新栏里一直留着同一个按钮。
#[cfg(target_os = "macos")]
fn offer_move_app(cx: &mut App) {
    let Some(entity) = updater(cx) else {
        return;
    };
    let description = match entity.read(cx).location_issue {
        None => return,
        Some(api::app_location::LocationIssue::ReadOnly) => t!("move.read_only").into_owned(),
        Some(api::app_location::LocationIssue::Translocated) => {
            t!("move.translocated").into_owned()
        }
    };
    open_confirm(cx, move |window, cx| {
        let mover = entity.clone();
        let state = entity.clone();
        Confirm::new(
            t!("move.confirm_title"),
            t!("move.confirm"),
            move |_, cx| {
                mover.update(cx, |this, cx| this.move_app_to_applications(cx));
                // 框留着：移好了 App 会退出，没移成在框里写原因。
                false
            },
        )
        .description(description)
        .body(move |_, cx| {
            let this = state.read(cx);
            let (text, color) = match (&this.move_app_error, this.moving_app) {
                (_, true) => (t!("move.moving").into(), ui_theme::muted()),
                (Some(error), false) => (error.clone(), ui_theme::bad()),
                (None, false) => (SharedString::default(), ui_theme::muted()),
            };
            div()
                .type_style(TYPE.ui)
                .text_color(rgb(color))
                .child(text)
                .into_any_element()
        })
        .open(window, cx);
    });
}

/// 在主窗口里弹确认框。主窗口没开就算了：更新入口都还在「关于」页。
fn open_confirm(cx: &mut App, open: impl FnOnce(&mut Window, &mut App) + 'static) {
    let Some(window) = api::host(cx).main_window(cx) else {
        api::app_log::warn("updater", "主窗口没开，确认框没地方弹");
        return;
    };
    if let Err(error) = window.update(cx, |_, window, cx| open(window, cx)) {
        api::app_log::warn("updater", &format!("确认框没弹出来：{error:#}"));
    }
}

/// 交给宿主退出，不再问退出守卫。
fn quit_now(cx: &mut App) {
    let quit = api::QUIT_NOW.run(&(), cx);
    cx.spawn(async move |_: &mut AsyncApp| {
        if let Err(error) = quit.await {
            api::app_log::error("updater", &format!("退出失败：{error}"));
        }
    })
    .detach();
}

fn local_busy_work_names(cx: &App) -> Vec<String> {
    busy_work_names(&api::host(cx).session_states(&api::RuntimeKey::Local, cx))
}

/// 本机正在流式输出或执行的会话。空闲的终端、等用户输入的会话不算。
fn busy_work_names(states: &[DaemonSessionState]) -> Vec<String> {
    states
        .iter()
        .filter(|state| {
            state.has_runtime()
                && matches!(
                    state.effective_phase(),
                    DaemonPhase::Thinking | DaemonPhase::ExecutingTool | DaemonPhase::Connecting
                )
        })
        .map(|state| {
            state
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .unwrap_or(state.id.as_str())
                .to_string()
        })
        .collect()
}

/// 停守护装更新的确认框正文：后果，再列出会被打断的会话。
fn stop_prompt_body(names: &[String]) -> String {
    let mut body = t!("stop.sentence").into_owned();
    if !names.is_empty() {
        let shown = names.len().min(NAME_LIMIT);
        let mut listed = names[..shown].join(&*t!("stop.separator"));
        if names.len() > shown {
            listed.push_str(&t!("stop.and_more", count = names.len()));
        }
        body.push('\n');
        body.push_str(&if names.len() == 1 {
            t!("stop.busy.one", names = listed)
        } else {
            t!("stop.busy.other", count = names.len(), names = listed)
        });
    }
    body
}

/// 有新版本就在「关于」页挂角标，没有就摘掉。
fn refresh_badge(entity: &Entity<Updater>, cx: &mut App) {
    let badge = entity
        .read(cx)
        .update_available()
        .then(|| t!("badge").into());
    api::set_view_badge(ABOUT_VIEW, badge, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(
        id: &str,
        title: Option<&str>,
        phase: DaemonPhase,
        runtime: bool,
    ) -> DaemonSessionState {
        DaemonSessionState {
            id: id.to_string(),
            title: title.map(str::to_string),
            phase,
            runtime,
            turn_events: true,
            ..DaemonSessionState::default()
        }
    }

    #[test]
    fn idle_sessions_are_not_busy() {
        let states = vec![
            state("idle", Some("空闲"), DaemonPhase::Idle, true),
            state("ghost", Some("幽灵"), DaemonPhase::Thinking, false),
            state("wait", Some("等输入"), DaemonPhase::WaitingForUser, true),
        ];
        let names = busy_work_names(&states);
        assert!(names.is_empty());
        assert_eq!(stop_prompt_body(&names), t!("stop.sentence"));
    }

    #[test]
    fn busy_sessions_are_listed_up_to_the_limit() {
        let mut states = vec![
            state("a", Some("写文档"), DaemonPhase::Thinking, true),
            state("b", None, DaemonPhase::ExecutingTool, true),
            state("c", Some("  "), DaemonPhase::Connecting, true),
        ];
        for index in 0..8 {
            states.push(state(
                &format!("extra-{index}"),
                Some(&format!("会话{index}")),
                DaemonPhase::Thinking,
                true,
            ));
        }
        let names = busy_work_names(&states);
        assert_eq!(names[..3], ["写文档", "b", "c"]);
        let body = stop_prompt_body(&names);
        assert!(body.starts_with(&*t!("stop.sentence")));
        assert!(body.contains(&format!("{} sessions are in progress", names.len())));
        assert!(body.contains(&format!("{} in all", names.len())));
        assert!(!body.contains("会话6"));
    }
}
