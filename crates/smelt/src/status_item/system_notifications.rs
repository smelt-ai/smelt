//! Modern macOS system-notification bridge.
//!
//! UserNotifications is asynchronous at every important boundary: authorization state,
//! authorization requests, and request submission. This module owns notifications after the
//! attention coordinator hands them off, and only retires them after the completion handler.

use super::{StatusItemEvent, SystemNotificationAuthorization, SystemNotificationStatus};
use crate::RuntimeKey;
use block2::{DynBlock, RcBlock};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSetting, UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

const AUTOMATION_RUN_IDENTIFIER_PREFIX: &str = "smelt.automation.run.";
/// 远程机器上的运行：前缀后面是 `运行时 id.运行 id`。本机的沿用老格式，已经挂在通知中心
/// 里的旧通知点了照样能跳。
const REMOTE_AUTOMATION_RUN_IDENTIFIER_PREFIX: &str = "smelt.automation.remote-run.";
const AUTOMATION_THREAD_PREFIX: &str = "automation:";
const REMOTE_AUTOMATION_THREAD_PREFIX: &str = "automation@";
const APP_IDENTIFIER: &str = "smelt.app.";
/// 插件给了 id 的通知：前缀后面是插件给的 id。同一个 id 再发换掉上一条，可以撤回。
const PLUGIN_IDENTIFIER_PREFIX: &str = "smelt.notify.";
/// 点通知要执行的命令（[`smelt_plugin_api::NotificationClick`] 的 JSON）存在通知内容的
/// userInfo 里这个键下，应用重启后点也找得到。
const CLICK_COMMAND_KEY: &str = "smelt.click";

fn plugin_notification_identifier(id: &str) -> String {
    format!("{PLUGIN_IDENTIFIER_PREFIX}{id}")
}

fn automation_notification_identifier(runtime: &RuntimeKey, run_id: &str) -> String {
    match runtime {
        RuntimeKey::Local => format!("{AUTOMATION_RUN_IDENTIFIER_PREFIX}{run_id}"),
        RuntimeKey::Remote(id) => format!("{REMOTE_AUTOMATION_RUN_IDENTIFIER_PREFIX}{id}.{run_id}"),
    }
}

/// 同一条自动化的通知在通知中心里归一组。
fn automation_thread_identifier(runtime: &RuntimeKey, automation_id: &str) -> String {
    match runtime {
        RuntimeKey::Local => format!("{AUTOMATION_THREAD_PREFIX}{automation_id}"),
        RuntimeKey::Remote(id) => format!("{REMOTE_AUTOMATION_THREAD_PREFIX}{id}:{automation_id}"),
    }
}

/// 运行时 id 是 UUID，不含 `.` 和 `:`，从第一个分隔符切开就是机器和后半段。
fn split_machine(rest: &str, separator: char) -> Option<(RuntimeKey, &str)> {
    let (machine, tail) = rest.split_once(separator)?;
    (!machine.is_empty() && !tail.is_empty())
        .then(|| (RuntimeKey::Remote(machine.to_string()), tail))
}

fn automation_run_from_identifier(identifier: &str) -> Option<(RuntimeKey, &str)> {
    if let Some(rest) = identifier.strip_prefix(REMOTE_AUTOMATION_RUN_IDENTIFIER_PREFIX) {
        return split_machine(rest, '.');
    }
    identifier
        .strip_prefix(AUTOMATION_RUN_IDENTIFIER_PREFIX)
        .filter(|run_id| !run_id.is_empty())
        .map(|run_id| (RuntimeKey::Local, run_id))
}

fn automation_from_thread_identifier(identifier: &str) -> Option<(RuntimeKey, &str)> {
    if let Some(rest) = identifier.strip_prefix(REMOTE_AUTOMATION_THREAD_PREFIX) {
        return split_machine(rest, ':');
    }
    identifier
        .strip_prefix(AUTOMATION_THREAD_PREFIX)
        .filter(|automation_id| !automation_id.is_empty())
        .map(|automation_id| (RuntimeKey::Local, automation_id))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct QueuedNotification {
    identifier: String,
    thread_identifier: String,
    subtitle: String,
    body: String,
    /// 点它执行的命令，JSON。
    on_click: Option<String>,
    generation: u64,
}

/// Owns requests until UserNotifications confirms that it accepted them. Pending requests are
/// coalesced by stable identifier, while generations serialize updates racing a callback.
#[derive(Default)]
struct NotificationQueue {
    next_generation: u64,
    active_identifiers: HashSet<String>,
    pending: HashMap<String, QueuedNotification>,
    in_flight: HashMap<String, QueuedNotification>,
    submitted: HashSet<String>,
    retractions: HashSet<String>,
    retry_blocked: bool,
}

impl NotificationQueue {
    fn enqueue_automation_run(
        &mut self,
        runtime: &RuntimeKey,
        automation_id: &str,
        run_id: &str,
        subtitle: &str,
        body: &str,
    ) {
        self.enqueue_with_identifier(
            automation_notification_identifier(runtime, run_id),
            automation_thread_identifier(runtime, automation_id),
            subtitle,
            body,
            None,
        );
    }

    fn enqueue_app(&mut self, subtitle: &str, body: &str, on_click: Option<String>) {
        self.enqueue_with_identifier(
            format!("{APP_IDENTIFIER}{}", uuid::Uuid::new_v4()),
            "app".to_string(),
            subtitle,
            body,
            on_click,
        );
    }

    /// 插件给了 id 的通知：同一个 id 再发换掉上一条（还在排队的直接换，投递过的由系统按同一个
    /// 标识替换），通知中心里同一个 id 归一组。
    fn enqueue_plugin(&mut self, id: &str, subtitle: &str, body: &str, on_click: Option<String>) {
        self.enqueue_with_identifier(
            plugin_notification_identifier(id),
            id.to_string(),
            subtitle,
            body,
            on_click,
        );
    }

    fn enqueue_with_identifier(
        &mut self,
        identifier: String,
        thread_identifier: String,
        subtitle: &str,
        body: &str,
        on_click: Option<String>,
    ) {
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.active_identifiers.insert(identifier.clone());
        self.pending.insert(
            identifier.clone(),
            QueuedNotification {
                identifier,
                thread_identifier,
                subtitle: subtitle.to_string(),
                body: body.to_string(),
                on_click,
                generation: self.next_generation,
            },
        );
    }

    fn take_ready(&mut self) -> Vec<QueuedNotification> {
        if self.retry_blocked {
            return Vec::new();
        }
        let ready_ids = self
            .pending
            .keys()
            .filter(|identifier| !self.in_flight.contains_key(*identifier))
            .cloned()
            .collect::<Vec<_>>();
        ready_ids
            .into_iter()
            .filter_map(|identifier| {
                let request = self.pending.remove(&identifier)?;
                self.in_flight.insert(identifier, request.clone());
                Some(request)
            })
            .collect()
    }

    fn complete(&mut self, identifier: &str, generation: u64, error: Option<&str>) {
        let Some(request) = self.in_flight.get(identifier) else {
            // A read/resolve can retract while addNotificationRequest is still completing.
            // Repeat the idempotent removal after that completion to close the race.
            if !self.active_identifiers.contains(identifier) {
                self.retractions.insert(identifier.to_string());
            }
            return;
        };
        if request.generation != generation {
            return;
        }
        let request = self
            .in_flight
            .remove(identifier)
            .expect("matching in-flight notification must still exist");
        let has_newer_request = self
            .pending
            .get(identifier)
            .is_some_and(|pending| pending.generation > generation);

        if error.is_some() {
            if identifier.starts_with(APP_IDENTIFIER) {
                // 应用通知是即时反馈，投递失败就丢弃，不在恢复后补发。
                self.active_identifiers.remove(identifier);
            } else if self.active_identifiers.contains(identifier) && !has_newer_request {
                self.pending.insert(identifier.to_string(), request);
                // Completion errors can be permanent (for example an invalid app bundle). Do not
                // spin a submit/fail loop; activation or a settings refresh unlocks one retry.
                self.retry_blocked = true;
            }
        } else if self.active_identifiers.contains(identifier) {
            if identifier.starts_with(APP_IDENTIFIER) {
                // 应用通知不再更新或按业务状态撤回，系统接收后不必长期保留队列状态。
                self.active_identifiers.remove(identifier);
            } else if has_newer_request {
                self.submitted.remove(identifier);
                self.retractions.insert(identifier.to_string());
            } else {
                self.submitted.insert(identifier.to_string());
            }
        } else {
            self.retractions.insert(identifier.to_string());
        }
    }

    fn discard_pending_app_notifications(&mut self) {
        let discarded = self
            .pending
            .keys()
            .filter(|id| id.starts_with(APP_IDENTIFIER))
            .cloned()
            .collect::<Vec<_>>();
        for identifier in discarded {
            self.pending.remove(&identifier);
            self.active_identifiers.remove(&identifier);
        }
    }

    /// 只动这台机器的运行通知：别的机器上的运行不在这份快照里，不能当成已删除。
    fn retain_automation_runs(&mut self, runtime: &RuntimeKey, run_ids: &[String]) {
        let retained = run_ids
            .iter()
            .map(|run_id| automation_notification_identifier(runtime, run_id))
            .collect::<HashSet<_>>();
        self.retain_scope(&retained, |identifier| {
            automation_run_from_identifier(identifier).is_some_and(|(owner, _)| owner == *runtime)
        });
    }

    /// A retention pass owns one identifier namespace only: automation history pruning must not
    /// touch other notifications.
    fn retain_scope(&mut self, retained: &HashSet<String>, in_scope: impl Fn(&str) -> bool) {
        let stale = self
            .active_identifiers
            .iter()
            .filter(|identifier| in_scope(identifier) && !retained.contains(*identifier))
            .cloned()
            .collect::<Vec<_>>();
        for identifier in stale {
            self.remove_identifier(&identifier);
        }
    }

    fn remove_automation_run(&mut self, runtime: &RuntimeKey, run_id: &str) {
        self.remove_identifier(&automation_notification_identifier(runtime, run_id));
    }

    fn remove_plugin(&mut self, id: &str) {
        self.remove_identifier(&plugin_notification_identifier(id));
    }

    fn remove_identifier(&mut self, identifier: &str) {
        self.active_identifiers.remove(identifier);
        self.pending.remove(identifier);
        self.submitted.remove(identifier);
        // Keep an in-flight generation until its completion callback, then retract once more.
        self.retractions.insert(identifier.to_string());
    }

    fn take_retractions(&mut self) -> Vec<String> {
        self.retractions.drain().collect()
    }

    fn allow_retry(&mut self) {
        self.retry_blocked = false;
    }

    fn pending_count(&self) -> usize {
        self.pending.len() + self.in_flight.len()
    }
}

#[derive(Default)]
struct BridgeState {
    initialized: bool,
    authorization: SystemNotificationAuthorization,
    settings_check_in_flight: bool,
    authorization_request_in_flight: bool,
    /// 已授权但当初没要角标（早先的版本只要了横幅和声音）：补要一次。系统不会再弹框，只是
    /// 在通知设置里加出角标开关；没有这项，Dock 角标设了也不显示。
    badge_missing: bool,
    /// 这次运行里补要过了：系统照旧不给也不再要，免得来回重查。
    badge_requested: bool,
    denial_reported: bool,
    queue: NotificationQueue,
    last_error: Option<String>,
    errors: VecDeque<String>,
}

impl BridgeState {
    fn record_error(&mut self, error: String) {
        self.last_error = Some(error.clone());
        if self.errors.len() >= 32 {
            self.errors.pop_front();
        }
        self.errors.push_back(error);
    }
}

enum BridgeAction {
    None,
    CheckSettings,
    RequestAuthorization,
    Submit(Vec<QueuedNotification>),
}

static EVENT_TX: OnceLock<smol::channel::Sender<StatusItemEvent>> = OnceLock::new();
static STATE: OnceLock<Mutex<BridgeState>> = OnceLock::new();

fn state() -> &'static Mutex<BridgeState> {
    STATE.get_or_init(|| Mutex::new(BridgeState::default()))
}

fn send_state_changed() {
    if let Some(tx) = EVENT_TX.get() {
        let _ = tx.try_send(StatusItemEvent::SystemNotificationStateChanged);
    }
}

objc2::define_class!(
    // SAFETY: NSObject has no subclassing requirements and this class has no Drop impl.
    #[unsafe(super = NSObject)]
    #[thread_kind = AnyThread]
    #[name = "SmeltUserNotificationDelegate"]
    #[ivars = ()]
    struct NotificationDelegate;

    // SAFETY: NSObjectProtocol has no additional safety requirements.
    unsafe impl NSObjectProtocol for NotificationDelegate {}

    // SAFETY: All method signatures match UNUserNotificationCenterDelegate exactly.
    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present_notification(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion_handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive_notification_response(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            let identifier = response.notification().request().identifier().to_string();
            let event = if let Some((runtime, run_id)) = automation_run_from_identifier(&identifier)
                .map(|(runtime, run_id)| (runtime, run_id.to_string()))
            {
                state()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .queue
                    .remove_automation_run(&runtime, &run_id);
                let thread = response
                    .notification()
                    .request()
                    .content()
                    .threadIdentifier()
                    .to_string();
                automation_from_thread_identifier(&thread)
                    .filter(|(owner, _)| *owner == runtime)
                    .map(|(_, automation_id)| StatusItemEvent::JumpToAutomationRun {
                        runtime,
                        automation_id: automation_id.to_string(),
                        run_id,
                    })
            } else if identifier.starts_with(APP_IDENTIFIER)
                || identifier.starts_with(PLUGIN_IDENTIFIER_PREFIX)
            {
                let mut state = state()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // 应用通知只有投递回执尚未返回时队列才还在跟踪；插件的点过就不在了，撤掉。
                if state.queue.active_identifiers.contains(&identifier) {
                    state.queue.remove_identifier(&identifier);
                    send_state_changed();
                }
                drop(state);
                click_command(response)
            } else {
                None
            };
            if let Some(event) = event {
                if let Some(tx) = EVENT_TX.get() {
                    let _ = tx.try_send(event);
                }
                send_state_changed();
            }
            completion_handler.call(());
        }
    }
);

/// 通知里存的点击命令；没存或存的不认识是 `None`（那就只把应用拿到前面来）。
fn click_command(response: &UNNotificationResponse) -> Option<StatusItemEvent> {
    let user_info = response.notification().request().content().userInfo();
    let key = NSString::from_str(CLICK_COMMAND_KEY);
    let key: &AnyObject = &key;
    let json = user_info.objectForKey(key)?.downcast::<NSString>().ok()?;
    match serde_json::from_str::<smelt_plugin_api::NotificationClick>(&json.to_string()) {
        Ok(click) => Some(StatusItemEvent::RunCommand {
            command: click.command,
            args: click.args,
        }),
        Err(error) => {
            eprintln!("[notifications] 通知里的点击命令认不出：{error}");
            None
        }
    }
}

impl NotificationDelegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: NSObject's init signature and method family are fixed by Foundation.
        unsafe { objc2::msg_send![super(this), init] }
    }
}

pub(super) fn setup(tx: smol::channel::Sender<StatusItemEvent>) {
    let _ = EVENT_TX.set(tx);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let delegate = NotificationDelegate::new();
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // UNUserNotificationCenter.delegate is weak. The process-wide delegate intentionally lives
    // until exit, matching the status-item target lifetime.
    let _ = Retained::into_raw(delegate);
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .initialized = true;
    refresh_system_notification_authorization();
}

fn authorization_from_settings(status: UNAuthorizationStatus) -> SystemNotificationAuthorization {
    if status == UNAuthorizationStatus::NotDetermined {
        SystemNotificationAuthorization::NotDetermined
    } else if status == UNAuthorizationStatus::Denied {
        SystemNotificationAuthorization::Denied
    } else if status == UNAuthorizationStatus::Authorized
        || status == UNAuthorizationStatus::Provisional
        || status == UNAuthorizationStatus::Ephemeral
    {
        SystemNotificationAuthorization::Authorized
    } else {
        SystemNotificationAuthorization::Unavailable
    }
}

fn error_description(error: *mut NSError) -> Option<String> {
    // SAFETY: UserNotifications passes either null or an NSError valid for the callback duration.
    unsafe { error.as_ref() }.map(ToString::to_string)
}

fn read_settings() {
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let completion: RcBlock<dyn Fn(NonNull<UNNotificationSettings>)> =
        RcBlock::new(|settings: NonNull<UNNotificationSettings>| {
            // SAFETY: The binding models this callback argument as NonNull and it remains valid
            // for the duration of the callback.
            let settings = unsafe { settings.as_ref() };
            let authorization = authorization_from_settings(settings.authorizationStatus());
            let mut state = state()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.settings_check_in_flight = false;
            state.authorization = authorization;
            state.badge_missing = authorization == SystemNotificationAuthorization::Authorized
                && settings.badgeSetting() == UNNotificationSetting::NotSupported
                && !state.badge_requested;
            let had_pending = state.queue.pending_count() > 0;
            if matches!(
                authorization,
                SystemNotificationAuthorization::Denied | SystemNotificationAuthorization::Unavailable
            ) {
                state.queue.discard_pending_app_notifications();
            }
            if authorization == SystemNotificationAuthorization::Authorized {
                state.queue.allow_retry();
                state.denial_reported = false;
                if state.queue.pending_count() == 0 {
                    state.last_error = None;
                }
            } else if authorization == SystemNotificationAuthorization::Denied
                && had_pending
                && !state.denial_reported
            {
                state.denial_reported = true;
                state.record_error("macOS notification permission is denied".to_string());
            }
            drop(state);
            send_state_changed();
        });
    center.getNotificationSettingsWithCompletionHandler(&completion);
}

fn request_authorization() {
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let completion: RcBlock<dyn Fn(Bool, *mut NSError)> =
        RcBlock::new(|_granted: Bool, error: *mut NSError| {
            let error = error_description(error);
            let mut state = state()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.authorization_request_in_flight = false;
            if let Some(error) = error {
                state.authorization = SystemNotificationAuthorization::Unavailable;
                state.queue.discard_pending_app_notifications();
                state.record_error(format!(
                    "failed to request macOS notification permission: {error}"
                ));
            } else {
                // 给没给、给了哪几项都以设置为准，重查一次（拒绝也在那里报）。`granted` 只说横幅
                // 那几项整体，已授权的 App 补要角标时它说明不了什么。
                state.authorization = SystemNotificationAuthorization::Unknown;
            }
            drop(state);
            send_state_changed();
        });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert
            | UNAuthorizationOptions::Sound
            | UNAuthorizationOptions::Badge,
        &completion,
    );
}

fn submit(notification: QueuedNotification) {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str("smelt"));
    content.setSubtitle(&NSString::from_str(&notification.subtitle));
    content.setBody(&NSString::from_str(&notification.body));
    content.setThreadIdentifier(&NSString::from_str(&notification.thread_identifier));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    if let Some(on_click) = &notification.on_click {
        let key = NSString::from_str(CLICK_COMMAND_KEY);
        let value = NSString::from_str(on_click);
        let user_info = NSDictionary::<NSString, NSString>::from_slices(&[&*key], &[&*value]);
        // SAFETY: userInfo 要的是能存进属性列表的键值，这里键值都是 NSString；只是把泛型参数
        // 换成 setter 声明的 AnyObject。
        unsafe {
            content.setUserInfo(&Retained::cast_unchecked::<NSDictionary>(user_info));
        }
    }

    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&notification.identifier),
        &content,
        None,
    );
    let identifier = notification.identifier.clone();
    let generation = notification.generation;
    let completion: RcBlock<dyn Fn(*mut NSError)> = RcBlock::new(move |error: *mut NSError| {
        let error = error_description(error);
        let mut state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .queue
            .complete(&identifier, generation, error.as_deref());
        if let Some(error) = error {
            state.record_error(format!(
                "failed to submit macOS notification {identifier}: {error}"
            ));
        } else if !state.queue.retry_blocked {
            state.last_error = None;
        }
        drop(state);
        send_state_changed();
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
}

fn retract(identifiers: &[String]) {
    if identifiers.is_empty() {
        return;
    }
    let identifiers = identifiers
        .iter()
        .map(|identifier| NSString::from_str(identifier))
        .collect::<Vec<_>>();
    let identifier_refs = identifiers
        .iter()
        .map(|identifier| &**identifier)
        .collect::<Vec<&NSString>>();
    let identifiers = NSArray::from_slice(&identifier_refs);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.removePendingNotificationRequestsWithIdentifiers(&identifiers);
    center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
}

/// Advances authorization, submission, and retraction from the GPUI/AppKit main thread.
pub fn sync_system_notifications() {
    let (retractions, action) = {
        let mut state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let retractions = state.queue.take_retractions();
        let action = if !state.initialized {
            BridgeAction::None
        } else {
            match state.authorization {
                SystemNotificationAuthorization::Unknown if !state.settings_check_in_flight => {
                    state.settings_check_in_flight = true;
                    BridgeAction::CheckSettings
                }
                SystemNotificationAuthorization::NotDetermined
                    if state.queue.pending_count() > 0
                        && !state.authorization_request_in_flight =>
                {
                    state.authorization_request_in_flight = true;
                    state.badge_requested = true;
                    BridgeAction::RequestAuthorization
                }
                SystemNotificationAuthorization::Authorized
                    if state.badge_missing && !state.authorization_request_in_flight =>
                {
                    state.authorization_request_in_flight = true;
                    state.badge_missing = false;
                    state.badge_requested = true;
                    BridgeAction::RequestAuthorization
                }
                SystemNotificationAuthorization::Authorized => {
                    let requests = state.queue.take_ready();
                    if requests.is_empty() {
                        BridgeAction::None
                    } else {
                        BridgeAction::Submit(requests)
                    }
                }
                _ => BridgeAction::None,
            }
        };
        (retractions, action)
    };

    retract(&retractions);
    match action {
        BridgeAction::None => {}
        BridgeAction::CheckSettings => read_settings(),
        BridgeAction::RequestAuthorization => request_authorization(),
        BridgeAction::Submit(requests) => {
            for request in requests {
                submit(request);
            }
        }
    }
}

fn prepare_queued_notification(state: &mut BridgeState) {
    if matches!(
        state.authorization,
        SystemNotificationAuthorization::Denied | SystemNotificationAuthorization::Unavailable
    ) || state.queue.retry_blocked
    {
        state.authorization = SystemNotificationAuthorization::Unknown;
        state.queue.allow_retry();
    }
}

fn deliver_app_notification(subtitle: &str, body: &str) {
    {
        let mut state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 和会话通知一样先排队：权限还没查清（刚启动）或正在问人时也不丢。被拒、不可用时
        // 查完权限会清掉排着的应用通知（[`NotificationQueue::discard_pending_app_notifications`]）。
        state.queue.enqueue_app(subtitle, body, None);
        prepare_queued_notification(&mut state);
    }
    sync_system_notifications();
}

/// 插件发的通知（`smelt.app/notify`）。没给 id 的同应用通知，一次性；给了 id 的像会话通知
/// 一样留着，投递失败等权限恢复再发，直到插件撤回或人点掉。
pub fn deliver_plugin_notification(notify: &smelt_plugin_api::Notify) {
    let on_click = notify
        .on_click
        .as_ref()
        .and_then(|click| serde_json::to_string(click).ok());
    {
        let mut state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &notify.id {
            Some(id) => state
                .queue
                .enqueue_plugin(id, &notify.subtitle, &notify.body, on_click),
            None => state
                .queue
                .enqueue_app(&notify.subtitle, &notify.body, on_click),
        }
        prepare_queued_notification(&mut state);
    }
    sync_system_notifications();
}

/// 撤回插件给了 id 的通知。应用重启前发的也撤得掉：通知中心按标识撤，不看队列记没记着。
pub fn withdraw_plugin_notification(id: &str) {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .queue
        .remove_plugin(id);
    sync_system_notifications();
}

pub fn notify_error(message: impl AsRef<str>) {
    deliver_app_notification("错误", message.as_ref());
}

pub fn notify_success(message: impl AsRef<str>) {
    deliver_app_notification("完成", message.as_ref());
}

pub fn deliver_automation_notification(
    runtime: &RuntimeKey,
    automation_id: &str,
    run_id: &str,
    subtitle: &str,
    body: &str,
) {
    {
        let mut state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .queue
            .enqueue_automation_run(runtime, automation_id, run_id, subtitle, body);
        prepare_queued_notification(&mut state);
    }
    sync_system_notifications();
}

pub fn retain_automation_notifications(runtime: &RuntimeKey, run_ids: &[String]) {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .queue
        .retain_automation_runs(runtime, run_ids);
    sync_system_notifications();
}

pub fn remove_automation_notification(runtime: &RuntimeKey, run_id: &str) {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .queue
        .remove_automation_run(runtime, run_id);
    sync_system_notifications();
}

/// Rechecks authorization and unlocks one retry after returning from System Settings.
pub fn refresh_system_notification_authorization() {
    {
        let mut state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.initialized
            || state.settings_check_in_flight
            || state.authorization_request_in_flight
        {
            return;
        }
        state.authorization = SystemNotificationAuthorization::Unknown;
        state.queue.allow_retry();
    }
    sync_system_notifications();
}

pub fn system_notification_status() -> SystemNotificationStatus {
    let state = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    SystemNotificationStatus {
        authorization: state.authorization,
        pending_count: state.queue.pending_count(),
        last_error: state.last_error.clone(),
    }
}

pub fn take_system_notification_errors() -> Vec<String> {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .errors
        .drain(..)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        APP_IDENTIFIER, NotificationQueue, authorization_from_settings,
        automation_from_thread_identifier, automation_notification_identifier,
        automation_run_from_identifier, automation_thread_identifier,
        plugin_notification_identifier, prepare_queued_notification,
    };
    use crate::RuntimeKey;
    use crate::status_item::SystemNotificationAuthorization;
    use objc2_user_notifications::UNAuthorizationStatus;

    #[test]
    fn authorization_status_preserves_denied_and_not_determined() {
        assert_eq!(
            authorization_from_settings(UNAuthorizationStatus::NotDetermined),
            SystemNotificationAuthorization::NotDetermined
        );
        assert_eq!(
            authorization_from_settings(UNAuthorizationStatus::Denied),
            SystemNotificationAuthorization::Denied
        );
        assert_eq!(
            authorization_from_settings(UNAuthorizationStatus::Authorized),
            SystemNotificationAuthorization::Authorized
        );
        assert_eq!(
            authorization_from_settings(UNAuthorizationStatus::Provisional),
            SystemNotificationAuthorization::Authorized
        );
    }

    /// 插件给了 id 的通知：同一个 id 再发换掉排着的那条，撤回后不再投递，也撤掉投递过的。
    #[test]
    fn plugin_notifications_replace_by_id_and_withdraw() {
        let mut queue = NotificationQueue::default();
        let click = Some(r#"{"command":"smelt.sessions/activate","args":{}}"#.to_string());
        queue.enqueue_plugin("smelt-notifications/s1", "完成", "第一条", None);
        queue.enqueue_plugin("smelt-notifications/s1", "完成", "第二条", click.clone());
        let ready = queue.take_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(
            ready[0].identifier,
            plugin_notification_identifier("smelt-notifications/s1")
        );
        assert_eq!(ready[0].body, "第二条");
        assert_eq!(ready[0].on_click, click);
        queue.complete(&ready[0].identifier, ready[0].generation, None);
        assert!(queue.take_retractions().is_empty());
        // 投递过的留着，直到插件撤回。
        assert!(queue.active_identifiers.contains(&ready[0].identifier));
        queue.remove_plugin("smelt-notifications/s1");
        assert_eq!(queue.take_retractions(), vec![ready[0].identifier.clone()]);
        assert!(queue.take_ready().is_empty());
        // 重启后队列里没有它，撤回照样交给通知中心去撤。
        let mut fresh = NotificationQueue::default();
        fresh.remove_plugin("smelt-notifications/s1");
        assert_eq!(fresh.take_retractions(), vec![ready[0].identifier.clone()]);
    }

    #[test]
    fn identifier_is_stable_and_reversible() {
        // 本机沿用老格式：升级前发出的通知点了照样能跳。
        let automation =
            automation_notification_identifier(&RuntimeKey::Local, "event:auto-1:evt-9");
        assert_eq!(automation, "smelt.automation.run.event:auto-1:evt-9");
        assert_eq!(
            automation_run_from_identifier(&automation),
            Some((RuntimeKey::Local, "event:auto-1:evt-9"))
        );
        assert_eq!(
            automation_run_from_identifier("smelt.automation.run."),
            None
        );
        assert_eq!(
            automation_from_thread_identifier("automation:automation-1"),
            Some((RuntimeKey::Local, "automation-1"))
        );
        assert_eq!(automation_from_thread_identifier("automation:"), None);

        // 远程机器上的运行带着机器，解回来跳到那台机器。
        let remote = RuntimeKey::Remote("39fafd41-9942-49b0-b9ec-7eb670d2711e".into());
        let identifier = automation_notification_identifier(&remote, "event:auto-1:evt-9");
        assert_eq!(
            automation_run_from_identifier(&identifier),
            Some((remote.clone(), "event:auto-1:evt-9"))
        );
        let thread = automation_thread_identifier(&remote, "auto-1");
        assert_eq!(
            automation_from_thread_identifier(&thread),
            Some((remote, "auto-1"))
        );
        assert_eq!(
            automation_run_from_identifier("smelt.automation.remote-run.no-run"),
            None
        );
    }

    #[test]
    fn app_notifications_are_independent_and_retire_after_delivery() {
        let mut queue = NotificationQueue::default();
        queue.enqueue_plugin("session-1", "done", "session body", None);
        queue.enqueue_app("错误", "模型 ID 不能为空", None);
        queue.enqueue_app("完成", "已保存", None);

        let ready = queue.take_ready();
        assert_eq!(ready.len(), 3);
        let apps: Vec<_> = ready
            .iter()
            .filter(|request| request.identifier.starts_with(APP_IDENTIFIER))
            .collect();
        assert_eq!(apps.len(), 2);
        assert_ne!(apps[0].identifier, apps[1].identifier);
        assert!(
            apps.iter()
                .any(|request| request.subtitle == "错误" && request.body == "模型 ID 不能为空")
        );
        assert!(
            apps.iter()
                .any(|request| request.subtitle == "完成" && request.body == "已保存")
        );
        assert!(apps.iter().all(|request| request.thread_identifier == "app"));

        queue.remove_plugin("session-1");
        assert!(
            queue
                .take_retractions()
                .iter()
                .all(|id| !id.starts_with(APP_IDENTIFIER))
        );
        for app in apps {
            queue.complete(&app.identifier, app.generation, None);
            assert!(!queue.active_identifiers.contains(&app.identifier));
            assert!(!queue.submitted.contains(&app.identifier));
        }
        assert_eq!(queue.pending_count(), 1); // 撤掉的插件通知仍在等待投递回执
    }

    #[test]
    fn failed_app_notification_is_not_retried() {
        let mut queue = NotificationQueue::default();
        queue.enqueue_app("提示", "操作完成", None);
        let request = queue.take_ready().pop().unwrap();
        queue.complete(&request.identifier, request.generation, Some("rejected"));
        assert_eq!(queue.pending_count(), 0);
        assert!(!queue.active_identifiers.contains(&request.identifier));
        assert!(!queue.retry_blocked);
        assert!(queue.take_ready().is_empty());
    }

    #[test]
    fn discarding_undeliverable_app_notifications_keeps_plugin_notifications() {
        let mut queue = NotificationQueue::default();
        queue.enqueue_plugin("session-1", "done", "session body", None);
        queue.enqueue_app("提示", "第一条", None);
        queue.enqueue_app("提示", "第二条", None);
        queue.discard_pending_app_notifications();
        let ready = queue.take_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(
            ready[0].identifier,
            plugin_notification_identifier("session-1")
        );
        assert!(
            queue
                .active_identifiers
                .iter()
                .all(|id| !id.starts_with(APP_IDENTIFIER))
        );
        assert!(queue.take_retractions().is_empty());
    }

    #[test]
    fn app_notifications_wait_for_authorization() {
        // 启动后第一条通知常常赶在权限查清之前：排着等，查清了再发或清掉。
        let mut state = super::BridgeState::default();
        assert_eq!(
            state.authorization,
            SystemNotificationAuthorization::Unknown
        );
        state.queue.enqueue_app("提示", "操作完成", None);
        prepare_queued_notification(&mut state);
        assert_eq!(state.queue.pending_count(), 1);
        state.queue.discard_pending_app_notifications();
        assert_eq!(state.queue.pending_count(), 0);
    }

    #[test]
    fn queue_coalesces_and_retains_failed_delivery() {
        let mut queue = NotificationQueue::default();
        queue.enqueue_plugin("session-1", "first", "old body", None);
        queue.enqueue_plugin("session-1", "second", "new body", None);

        let mut ready = queue.take_ready();
        assert_eq!(ready.len(), 1);
        let request = ready.pop().unwrap();
        assert_eq!(request.subtitle, "second");
        assert_eq!(request.body, "new body");
        queue.complete(&request.identifier, request.generation, Some("rejected"));

        assert_eq!(queue.pending_count(), 1);
        assert!(queue.take_ready().is_empty());
        queue.allow_retry();
        let retried = queue.take_ready();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].generation, request.generation);
    }

    #[test]
    fn failed_old_generation_does_not_block_a_newer_request() {
        let mut queue = NotificationQueue::default();
        queue.enqueue_plugin("session-1", "old", "old body", None);
        let old = queue.take_ready().pop().unwrap();
        queue.enqueue_plugin("session-1", "new", "new body", None);

        queue.complete(&old.identifier, old.generation, Some("rejected"));

        let next = queue.take_ready();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].subtitle, "new");
    }

    #[test]
    fn newer_notification_waits_for_retracted_in_flight_generation() {
        let mut queue = NotificationQueue::default();
        queue.enqueue_plugin("session-1", "old", "old body", None);
        let old = queue.take_ready().pop().unwrap();

        queue.remove_plugin("session-1");
        assert_eq!(queue.take_retractions(), vec![old.identifier.clone()]);
        queue.enqueue_plugin("session-1", "new", "new body", None);
        assert!(queue.take_ready().is_empty());

        queue.complete(&old.identifier, old.generation, None);
        assert_eq!(queue.take_retractions(), vec![old.identifier.clone()]);
        let next = queue.take_ready();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].subtitle, "new");
    }

    #[test]
    fn automation_retention_is_isolated() {
        let mut automation_queue = NotificationQueue::default();
        automation_queue.enqueue_automation_run(
            &RuntimeKey::Local,
            "automation-1",
            "run-1",
            "done",
            "body",
        );
        let request = automation_queue.take_ready().pop().unwrap();
        assert_eq!(
            request.identifier,
            automation_notification_identifier(&RuntimeKey::Local, "run-1")
        );
        assert_eq!(request.thread_identifier, "automation:automation-1");
        automation_queue.complete(&request.identifier, request.generation, None);

        // 别的机器的快照不撤本机的运行通知。
        automation_queue.retain_automation_runs(&RuntimeKey::Remote("box".into()), &[]);
        assert!(automation_queue.take_retractions().is_empty());
        automation_queue.retain_automation_runs(&RuntimeKey::Local, &[]);
        assert_eq!(
            automation_queue.take_retractions(),
            vec![automation_notification_identifier(
                &RuntimeKey::Local,
                "run-1"
            )]
        );

        let mut plugin_queue = NotificationQueue::default();
        plugin_queue.enqueue_plugin("session-1", "done", "body", None);
        let request = plugin_queue.take_ready().pop().unwrap();
        plugin_queue.complete(&request.identifier, request.generation, None);

        plugin_queue.retain_automation_runs(&RuntimeKey::Local, &[]);
        assert!(plugin_queue.take_retractions().is_empty());
    }
}
