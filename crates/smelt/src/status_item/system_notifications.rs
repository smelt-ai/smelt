//! Modern macOS system-notification bridge.
//!
//! UserNotifications is asynchronous at every important boundary: authorization state,
//! authorization requests, and request submission. This module owns notifications after the
//! attention coordinator hands them off, and only retires them after the completion handler.

use super::{StatusItemEvent, SystemNotificationAuthorization, SystemNotificationStatus};
use block2::{DynBlock, RcBlock};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

const SESSION_IDENTIFIER_PREFIX: &str = "smelt.session.";
const AUTOMATION_RUN_IDENTIFIER_PREFIX: &str = "smelt.automation.run.";
const APP_IDENTIFIER: &str = "smelt.app";

fn notification_identifier(session_id: &str) -> String {
    format!("{SESSION_IDENTIFIER_PREFIX}{session_id}")
}

fn automation_notification_identifier(run_id: &str) -> String {
    format!("{AUTOMATION_RUN_IDENTIFIER_PREFIX}{run_id}")
}

fn session_id_from_identifier(identifier: &str) -> Option<&str> {
    identifier
        .strip_prefix(SESSION_IDENTIFIER_PREFIX)
        .filter(|session_id| !session_id.is_empty())
}

fn automation_run_id_from_identifier(identifier: &str) -> Option<&str> {
    identifier
        .strip_prefix(AUTOMATION_RUN_IDENTIFIER_PREFIX)
        .filter(|run_id| !run_id.is_empty())
}

fn automation_id_from_thread_identifier(identifier: &str) -> Option<&str> {
    identifier
        .strip_prefix("automation:")
        .filter(|automation_id| !automation_id.is_empty())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct QueuedNotification {
    identifier: String,
    thread_identifier: String,
    subtitle: String,
    body: String,
    generation: u64,
}

/// Owns requests until UserNotifications confirms that it accepted them. Pending requests are
/// coalesced by stable session identifier, while generations serialize updates racing a callback.
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
    fn enqueue(&mut self, session_id: &str, subtitle: &str, body: &str) {
        self.enqueue_with_identifier(
            notification_identifier(session_id),
            session_id.to_string(),
            subtitle,
            body,
        );
    }

    fn enqueue_automation_run(
        &mut self,
        automation_id: &str,
        run_id: &str,
        subtitle: &str,
        body: &str,
    ) {
        self.enqueue_with_identifier(
            automation_notification_identifier(run_id),
            format!("automation:{automation_id}"),
            subtitle,
            body,
        );
    }

    fn enqueue_app(&mut self, subtitle: &str, body: &str) {
        self.enqueue_with_identifier(
            APP_IDENTIFIER.to_string(),
            "app".to_string(),
            subtitle,
            body,
        );
    }

    fn enqueue_with_identifier(
        &mut self,
        identifier: String,
        thread_identifier: String,
        subtitle: &str,
        body: &str,
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
            if self.active_identifiers.contains(identifier) && !has_newer_request {
                self.pending.insert(identifier.to_string(), request);
                // Completion errors can be permanent (for example an invalid app bundle). Do not
                // spin a submit/fail loop; activation or a settings refresh unlocks one retry.
                self.retry_blocked = true;
            }
        } else if self.active_identifiers.contains(identifier) {
            if has_newer_request {
                self.submitted.remove(identifier);
                self.retractions.insert(identifier.to_string());
            } else {
                self.submitted.insert(identifier.to_string());
            }
        } else {
            self.retractions.insert(identifier.to_string());
        }
    }

    fn retain_sessions(&mut self, session_ids: &[String]) {
        let retained = session_ids
            .iter()
            .map(|session_id| notification_identifier(session_id))
            .collect::<HashSet<_>>();
        self.retain_scope(&retained, |identifier| {
            session_id_from_identifier(identifier).is_some()
        });
    }

    fn retain_automation_runs(&mut self, run_ids: &[String]) {
        let retained = run_ids
            .iter()
            .map(|run_id| automation_notification_identifier(run_id))
            .collect::<HashSet<_>>();
        self.retain_scope(&retained, |identifier| {
            automation_run_id_from_identifier(identifier).is_some()
        });
    }

    /// A retention pass owns one identifier namespace only. Session unread updates must never
    /// retract automation results, and automation history pruning must not touch session alerts.
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

    fn remove_session(&mut self, session_id: &str) {
        self.remove_identifier(&notification_identifier(session_id));
    }

    fn remove_automation_run(&mut self, run_id: &str) {
        self.remove_identifier(&automation_notification_identifier(run_id));
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
            let event = if let Some(session_id) =
                session_id_from_identifier(&identifier).map(str::to_string)
            {
                state()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .queue
                    .remove_session(&session_id);
                Some(StatusItemEvent::JumpToDaemonSession(session_id))
            } else if let Some(run_id) =
                automation_run_id_from_identifier(&identifier).map(str::to_string)
            {
                state()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .queue
                    .remove_automation_run(&run_id);
                let automation_id = response
                    .notification()
                    .request()
                    .content()
                    .threadIdentifier()
                    .to_string();
                automation_id_from_thread_identifier(&automation_id).map(|automation_id| {
                    StatusItemEvent::JumpToAutomationRun {
                        automation_id: automation_id.to_string(),
                        run_id,
                    }
                })
            } else if identifier == APP_IDENTIFIER {
                state()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .queue
                    .remove_identifier(&identifier);
                send_state_changed();
                None
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
            let authorization =
                authorization_from_settings(unsafe { settings.as_ref() }.authorizationStatus());
            let mut state = state()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.settings_check_in_flight = false;
            state.authorization = authorization;
            if authorization == SystemNotificationAuthorization::Authorized {
                state.queue.allow_retry();
                state.denial_reported = false;
                if state.queue.pending_count() == 0 {
                    state.last_error = None;
                }
            } else if authorization == SystemNotificationAuthorization::Denied
                && state.queue.pending_count() > 0
                && !state.denial_reported
            {
                state.denial_reported = true;
                state.record_error(
                    "macOS notification permission is denied; reminders remain queued".to_string(),
                );
            }
            drop(state);
            send_state_changed();
        });
    center.getNotificationSettingsWithCompletionHandler(&completion);
}

fn request_authorization() {
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let completion: RcBlock<dyn Fn(Bool, *mut NSError)> =
        RcBlock::new(|granted: Bool, error: *mut NSError| {
            let error = error_description(error);
            let mut state = state()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.authorization_request_in_flight = false;
            if let Some(error) = error {
                state.authorization = SystemNotificationAuthorization::Unavailable;
                state.record_error(format!(
                    "failed to request macOS notification permission: {error}"
                ));
            } else if granted.as_bool() {
                // Query settings again so provisional/authorized details come from one source.
                state.authorization = SystemNotificationAuthorization::Unknown;
                state.denial_reported = false;
            } else {
                state.authorization = SystemNotificationAuthorization::Denied;
                if !state.denial_reported {
                    state.denial_reported = true;
                    state.record_error(
                        "macOS notification permission was denied; reminders remain queued"
                            .to_string(),
                    );
                }
            }
            drop(state);
            send_state_changed();
        });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
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

/// Queues synchronously; authorization and native submission continue asynchronously.
pub fn deliver_notification(session_id: &str, subtitle: &str, body: &str) {
    {
        let mut state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.queue.enqueue(session_id, subtitle, body);
        prepare_queued_notification(&mut state);
    }
    sync_system_notifications();
}

pub fn deliver_app_notification(subtitle: &str, body: &str) {
    {
        let mut state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.queue.enqueue_app(subtitle, body);
        prepare_queued_notification(&mut state);
    }
    sync_system_notifications();
}

pub fn notify_error(message: impl AsRef<str>) {
    deliver_app_notification("错误", message.as_ref());
}

pub fn notify_success(message: impl AsRef<str>) {
    deliver_app_notification("完成", message.as_ref());
}

pub fn notify_info(message: impl AsRef<str>) {
    deliver_app_notification("提示", message.as_ref());
}

pub fn deliver_automation_notification(
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
            .enqueue_automation_run(automation_id, run_id, subtitle, body);
        prepare_queued_notification(&mut state);
    }
    sync_system_notifications();
}

/// Retires system-channel intent for sessions that are no longer unread.
pub fn retain_system_notifications(session_ids: &[String]) {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .queue
        .retain_sessions(session_ids);
    sync_system_notifications();
}

pub fn retain_automation_notifications(run_ids: &[String]) {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .queue
        .retain_automation_runs(run_ids);
    sync_system_notifications();
}

pub fn remove_system_notification(session_id: &str) {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .queue
        .remove_session(session_id);
    sync_system_notifications();
}

pub fn remove_automation_notification(run_id: &str) {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .queue
        .remove_automation_run(run_id);
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
        automation_id_from_thread_identifier, automation_notification_identifier,
        automation_run_id_from_identifier, notification_identifier, session_id_from_identifier,
    };
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

    #[test]
    fn identifier_is_stable_and_reversible() {
        let identifier = notification_identifier("daemon/session 42");
        assert_eq!(identifier, "smelt.session.daemon/session 42");
        assert_eq!(
            session_id_from_identifier(&identifier),
            Some("daemon/session 42")
        );
        assert_eq!(session_id_from_identifier("unrelated"), None);
        assert_eq!(session_id_from_identifier("smelt.session."), None);

        let automation = automation_notification_identifier("event:auto-1:evt-9");
        assert_eq!(automation, "smelt.automation.run.event:auto-1:evt-9");
        assert_eq!(
            automation_run_id_from_identifier(&automation),
            Some("event:auto-1:evt-9")
        );
        assert_eq!(
            automation_run_id_from_identifier("smelt.automation.run."),
            None
        );
        assert_eq!(
            automation_id_from_thread_identifier("automation:automation-1"),
            Some("automation-1")
        );
        assert_eq!(automation_id_from_thread_identifier("automation:"), None);
    }

    #[test]
    fn app_feedback_coalesces_without_touching_session_alerts() {
        let mut queue = NotificationQueue::default();
        queue.enqueue("session-1", "done", "session body");
        queue.enqueue_app("错误", "模型 ID 不能为空");
        queue.enqueue_app("完成", "已保存");

        let ready = queue.take_ready();
        assert_eq!(ready.len(), 2);
        let app = ready
            .iter()
            .find(|request| request.identifier == APP_IDENTIFIER)
            .unwrap();
        assert_eq!(app.subtitle, "完成");
        assert_eq!(app.body, "已保存");
        assert_eq!(app.thread_identifier, "app");

        queue.retain_sessions(&[]);
        assert!(
            !queue
                .take_retractions()
                .iter()
                .any(|id| id == APP_IDENTIFIER)
        );
    }

    #[test]
    fn queue_coalesces_and_retains_failed_delivery() {
        let mut queue = NotificationQueue::default();
        queue.enqueue("session-1", "first", "old body");
        queue.enqueue("session-1", "second", "new body");

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
        queue.enqueue("session-1", "old", "old body");
        let old = queue.take_ready().pop().unwrap();
        queue.enqueue("session-1", "new", "new body");

        queue.complete(&old.identifier, old.generation, Some("rejected"));

        let next = queue.take_ready();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].subtitle, "new");
    }

    #[test]
    fn newer_notification_waits_for_retracted_in_flight_generation() {
        let mut queue = NotificationQueue::default();
        queue.enqueue("session-1", "old", "old body");
        let old = queue.take_ready().pop().unwrap();

        queue.remove_session("session-1");
        assert_eq!(queue.take_retractions(), vec![old.identifier.clone()]);
        queue.enqueue("session-1", "new", "new body");
        assert!(queue.take_ready().is_empty());

        queue.complete(&old.identifier, old.generation, None);
        assert_eq!(queue.take_retractions(), vec![old.identifier.clone()]);
        let next = queue.take_ready();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].subtitle, "new");
    }

    #[test]
    fn unread_state_does_not_reactivate_a_non_system_channel() {
        let mut queue = NotificationQueue::default();
        queue.enqueue("session-1", "old", "body");
        let request = queue.take_ready().pop().unwrap();

        queue.remove_session("session-1");
        queue.take_retractions();
        queue.retain_sessions(&["session-1".to_string()]);
        queue.complete(&request.identifier, request.generation, None);

        assert_eq!(
            queue.take_retractions(),
            vec![notification_identifier("session-1")]
        );
    }

    #[test]
    fn queue_retracts_read_or_resolved_sessions() {
        let mut queue = NotificationQueue::default();
        queue.enqueue("session-1", "done", "body");
        let request = queue.take_ready().pop().unwrap();
        queue.complete(&request.identifier, request.generation, None);

        queue.retain_sessions(&[]);
        assert_eq!(
            queue.take_retractions(),
            vec![notification_identifier("session-1")]
        );
        assert_eq!(queue.pending_count(), 0);
    }

    #[test]
    fn session_and_automation_retention_are_isolated() {
        let mut automation_queue = NotificationQueue::default();
        automation_queue.enqueue_automation_run("automation-1", "run-1", "done", "body");
        let request = automation_queue.take_ready().pop().unwrap();
        assert_eq!(
            request.identifier,
            automation_notification_identifier("run-1")
        );
        assert_eq!(request.thread_identifier, "automation:automation-1");
        automation_queue.complete(&request.identifier, request.generation, None);

        automation_queue.retain_sessions(&[]);
        assert!(automation_queue.take_retractions().is_empty());
        automation_queue.retain_automation_runs(&[]);
        assert_eq!(
            automation_queue.take_retractions(),
            vec![automation_notification_identifier("run-1")]
        );

        let mut session_queue = NotificationQueue::default();
        session_queue.enqueue("session-1", "done", "body");
        let request = session_queue.take_ready().pop().unwrap();
        session_queue.complete(&request.identifier, request.generation, None);

        session_queue.retain_automation_runs(&[]);
        assert!(session_queue.take_retractions().is_empty());
        session_queue.retain_sessions(&[]);
        assert_eq!(
            session_queue.take_retractions(),
            vec![notification_identifier("session-1")]
        );
    }
}
