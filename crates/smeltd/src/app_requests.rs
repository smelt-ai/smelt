//! 交给本机桌面、要等它回执的请求：外观修改（`settings.update`）和 App 命令
//! （`app.command.*`）。
//!
//! 桌面订了对应话题的那条连接登记成「桌面」（[`AppRequests::register`]），请求单播进它的
//! 信箱，桌面做完用回执交回结果。没有桌面、不止一个桌面、投不进去都当场报错，不会假装成功；
//! 交出去了却没等到回执是「结果未知」。回执只认收到请求的那个进程。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Duration;

use crate::protocol::{ConnectionMail, ConnectionMailbox, PeerIdentity};
use smelt_event_protocol::{
    AppCommandOutcome, AppCommandRequest, AppCommandRequested, AppearanceApplyOutcome,
    AppearanceApplyRequested,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RequestError {
    NoDesktop,
    MultipleDesktops,
    DeliveryFailed,
    OutcomeUnknown,
    Invalid(String),
    Desktop(String),
}

type Reply<T> = mpsc::Sender<Result<T, RequestError>>;

struct State<T> {
    desktops: BTreeMap<String, (PeerIdentity, Weak<ConnectionMailbox>)>,
    pending: BTreeMap<String, (String, Reply<T>)>,
}

/// 一种交给桌面的请求，`T` 是回执里成功时的结果。
pub(crate) struct AppRequests<T> {
    state: Mutex<State<T>>,
}

impl<T> Default for AppRequests<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                desktops: BTreeMap::new(),
                pending: BTreeMap::new(),
            }),
        }
    }
}

/// 外观修改：回执只说成没成。
pub(crate) type AppearanceRequests = AppRequests<()>;
/// App 命令：回执带命令交回的值。
pub(crate) type CommandRequests = AppRequests<serde_json::Value>;

/// 一条连接登记成桌面的凭据，丢掉就注销。
pub(crate) struct AppLease<T> {
    requests: Arc<AppRequests<T>>,
    id: String,
}

pub(crate) struct PendingRequest<T> {
    requests: Arc<AppRequests<T>>,
    id: String,
    receiver: mpsc::Receiver<Result<T, RequestError>>,
}

impl<T> AppRequests<T> {
    pub(crate) fn register(
        self: &Arc<Self>,
        peer: PeerIdentity,
        mailbox: &Arc<ConnectionMailbox>,
    ) -> AppLease<T> {
        let id = uuid::Uuid::new_v4().to_string();
        self.state
            .lock()
            .unwrap()
            .desktops
            .insert(id.clone(), (peer, Arc::downgrade(mailbox)));
        AppLease {
            requests: self.clone(),
            id,
        }
    }

    /// 把 `mail(请求 id)` 投给唯一的桌面。
    fn send(
        self: &Arc<Self>,
        mail: impl FnOnce(String) -> Result<ConnectionMail, RequestError>,
    ) -> Result<PendingRequest<T>, RequestError> {
        let id = uuid::Uuid::new_v4().to_string();
        let mail = mail(id.clone())?;
        let mut state = self.state.lock().unwrap();
        let (owner, mailbox) = match state.desktops.len() {
            0 => return Err(RequestError::NoDesktop),
            1 => {
                let (id, (_, mailbox)) = state.desktops.iter().next().unwrap();
                (
                    id.clone(),
                    mailbox.upgrade().ok_or(RequestError::NoDesktop)?,
                )
            }
            _ => return Err(RequestError::MultipleDesktops),
        };
        let (sender, receiver) = mpsc::channel();
        state.pending.insert(id.clone(), (owner, sender));
        if !mailbox.try_push(mail) {
            state.pending.remove(&id);
            return Err(RequestError::DeliveryFailed);
        }
        Ok(PendingRequest {
            requests: self.clone(),
            id,
            receiver,
        })
    }

    /// 桌面交回回执。只认收到这个请求的那个进程。
    fn answer(
        &self,
        peer: &PeerIdentity,
        id: &str,
        result: Result<T, RequestError>,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let (owner, _) = state
            .pending
            .get(id)
            .ok_or("The request has ended or doesn't exist")?;
        let (expected, _) = state
            .desktops
            .get(owner)
            .ok_or("The desktop disconnected")?;
        if !same_process(expected, peer) {
            return Err("The receipt didn't come from the desktop that got the request".into());
        }
        let (_, sender) = state.pending.remove(id).unwrap();
        let _ = sender.send(result);
        Ok(())
    }
}

impl AppRequests<()> {
    pub(crate) fn dispatch(
        self: &Arc<Self>,
        settings: BTreeMap<String, serde_json::Value>,
    ) -> Result<PendingRequest<()>, RequestError> {
        self.send(|request_id| {
            let request = AppearanceApplyRequested {
                request_id,
                settings,
            };
            if serde_json::to_vec(&request).map_or(true, |bytes| bytes.len() > 4 * 1024) {
                return Err(RequestError::Invalid(
                    "The appearance request is too large".into(),
                ));
            }
            Ok(ConnectionMail::AppearanceRequest(request))
        })
    }

    pub(crate) fn respond(
        &self,
        peer: &PeerIdentity,
        id: &str,
        outcome: AppearanceApplyOutcome,
    ) -> Result<(), String> {
        let result = match outcome {
            AppearanceApplyOutcome::Applied => Ok(()),
            AppearanceApplyOutcome::Invalid { message } => Err(RequestError::Invalid(message)),
            AppearanceApplyOutcome::Failed { message } => Err(RequestError::Desktop(message)),
        };
        self.answer(peer, id, result)
    }
}

impl AppRequests<serde_json::Value> {
    pub(crate) fn dispatch(
        self: &Arc<Self>,
        request: AppCommandRequest,
    ) -> Result<PendingRequest<serde_json::Value>, RequestError> {
        self.send(|request_id| {
            let request = AppCommandRequested {
                request_id,
                request,
            };
            if serde_json::to_vec(&request).map_or(true, |bytes| bytes.len() > 64 * 1024) {
                return Err(RequestError::Invalid(
                    "The command arguments are too large".into(),
                ));
            }
            Ok(ConnectionMail::AppCommandRequest(request))
        })
    }

    pub(crate) fn respond(
        &self,
        peer: &PeerIdentity,
        id: &str,
        outcome: AppCommandOutcome,
    ) -> Result<(), String> {
        let result = match outcome {
            AppCommandOutcome::Done { value } => Ok(value),
            AppCommandOutcome::Failed { message } => Err(RequestError::Desktop(message)),
        };
        self.answer(peer, id, result)
    }
}

fn same_process(a: &PeerIdentity, b: &PeerIdentity) -> bool {
    #[cfg(target_os = "macos")]
    {
        a.audit_token.is_some() && a.audit_token == b.audit_token
    }
    #[cfg(not(target_os = "macos"))]
    {
        a.pid == b.pid
    }
}

impl<T> PendingRequest<T> {
    pub(crate) fn wait(self, timeout: Duration) -> Result<T, RequestError> {
        let result = self
            .receiver
            .recv_timeout(timeout)
            .unwrap_or(Err(RequestError::OutcomeUnknown));
        self.requests.state.lock().unwrap().pending.remove(&self.id);
        result
    }
}

impl<T> Drop for PendingRequest<T> {
    fn drop(&mut self) {
        self.requests.state.lock().unwrap().pending.remove(&self.id);
    }
}

impl<T> Drop for AppLease<T> {
    fn drop(&mut self) {
        let mut state = self.requests.state.lock().unwrap();
        state.desktops.remove(&self.id);
        state.pending.retain(|_, (owner, sender)| {
            if owner == &self.id {
                let _ = sender.send(Err(RequestError::OutcomeUnknown));
                false
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ConnectionMail, ConnectionMailbox, PeerIdentity};
    use std::sync::Arc;
    use std::time::Duration;

    fn peer(pid: u32) -> PeerIdentity {
        PeerIdentity {
            pid,
            executable: None,
            #[cfg(target_os = "macos")]
            audit_token: Some(crate::protocol::PeerAuditToken([
                0, 0, 0, 0, 0, pid, 0, pid,
            ])),
        }
    }

    fn patch() -> std::collections::BTreeMap<String, serde_json::Value> {
        std::collections::BTreeMap::from([(
            "appearance.theme".into(),
            serde_json::json!("unknown theme"),
        )])
    }

    #[test]
    fn no_desktop_is_refused_without_delivery() {
        let requests = Arc::new(AppearanceRequests::default());
        assert!(matches!(
            requests.dispatch(patch()),
            Err(RequestError::NoDesktop)
        ));
    }

    #[test]
    fn multiple_desktops_are_refused_without_delivery() {
        let requests = Arc::new(AppearanceRequests::default());
        let a = Arc::new(ConnectionMailbox::new());
        let b = Arc::new(ConnectionMailbox::new());
        let _a = requests.register(peer(1), &a);
        let _b = requests.register(peer(2), &b);
        assert!(matches!(
            requests.dispatch(patch()),
            Err(RequestError::MultipleDesktops)
        ));
    }

    #[test]
    fn closed_or_full_mailbox_rejects_without_delivery() {
        for full in [false, true] {
            let requests = Arc::new(AppearanceRequests::default());
            let mailbox = Arc::new(ConnectionMailbox::new());
            let _lease = requests.register(peer(1), &mailbox);
            if full {
                for i in 0..crate::protocol::CONNECTION_MAILBOX_LIMIT {
                    assert!(mailbox.try_push(ConnectionMail::AppearanceRequest(
                        AppearanceApplyRequested {
                            request_id: format!("queued-{i}"),
                            settings: patch(),
                        },
                    )));
                }
            } else {
                mailbox.stop();
            }
            assert!(matches!(
                requests.dispatch(patch()),
                Err(RequestError::DeliveryFailed)
            ));
        }
    }

    #[test]
    fn desktop_error_is_returned_instead_of_false_success() {
        let requests = Arc::new(AppearanceRequests::default());
        let mailbox = Arc::new(ConnectionMailbox::new());
        let desktop = peer(1);
        let _lease = requests.register(desktop.clone(), &mailbox);
        let pending = requests.dispatch(patch()).unwrap();
        let ConnectionMail::AppearanceRequest(request) = mailbox.next().unwrap() else {
            panic!("expected appearance request");
        };
        requests
            .respond(
                &desktop,
                &request.request_id,
                AppearanceApplyOutcome::Invalid {
                    message: "未知主题".into(),
                },
            )
            .unwrap();
        assert_eq!(
            pending.wait(Duration::from_secs(1)),
            Err(RequestError::Invalid("未知主题".into()))
        );
    }

    #[test]
    fn persistence_error_reaches_the_caller() {
        let requests = Arc::new(AppearanceRequests::default());
        let mailbox = Arc::new(ConnectionMailbox::new());
        let desktop = peer(1);
        let _lease = requests.register(desktop.clone(), &mailbox);
        let pending = requests.dispatch(patch()).unwrap();
        let ConnectionMail::AppearanceRequest(request) = mailbox.next().unwrap() else {
            panic!("expected appearance request");
        };
        requests
            .respond(
                &desktop,
                &request.request_id,
                AppearanceApplyOutcome::Failed {
                    message: "保存失败".into(),
                },
            )
            .unwrap();
        assert_eq!(
            pending.wait(Duration::from_secs(1)),
            Err(RequestError::Desktop("保存失败".into()))
        );
    }

    #[test]
    fn success_requires_a_receipt_from_the_selected_desktop() {
        let requests = Arc::new(AppearanceRequests::default());
        let mailbox = Arc::new(ConnectionMailbox::new());
        let desktop = peer(1);
        let _lease = requests.register(desktop.clone(), &mailbox);
        let pending = requests.dispatch(patch()).unwrap();
        let ConnectionMail::AppearanceRequest(request) = mailbox.next().unwrap() else {
            panic!("expected appearance request");
        };
        assert!(
            requests
                .respond(
                    &peer(2),
                    &request.request_id,
                    AppearanceApplyOutcome::Applied
                )
                .is_err()
        );
        requests
            .respond(
                &desktop,
                &request.request_id,
                AppearanceApplyOutcome::Applied,
            )
            .unwrap();
        assert_eq!(pending.wait(Duration::from_secs(1)), Ok(()));
    }

    #[test]
    fn timeout_is_unknown_and_a_late_receipt_cannot_complete_another_request() {
        let requests = Arc::new(AppearanceRequests::default());
        let mailbox = Arc::new(ConnectionMailbox::new());
        let desktop = peer(1);
        let _lease = requests.register(desktop.clone(), &mailbox);
        let pending = requests.dispatch(patch()).unwrap();
        let ConnectionMail::AppearanceRequest(request) = mailbox.next().unwrap() else {
            panic!("expected appearance request");
        };
        assert_eq!(
            pending.wait(Duration::ZERO),
            Err(RequestError::OutcomeUnknown)
        );
        assert!(
            requests
                .respond(
                    &desktop,
                    &request.request_id,
                    AppearanceApplyOutcome::Applied
                )
                .is_err()
        );
    }

    #[test]
    fn released_lease_is_no_longer_counted_as_a_desktop() {
        let requests = Arc::new(AppearanceRequests::default());
        let stale = Arc::new(ConnectionMailbox::new());
        let live = Arc::new(ConnectionMailbox::new());
        let stale_lease = requests.register(peer(1), &stale);
        let _live = requests.register(peer(2), &live);
        stale.stop();
        drop(stale_lease);
        let pending = requests.dispatch(patch()).unwrap();
        assert!(matches!(
            live.next(),
            Some(ConnectionMail::AppearanceRequest(_))
        ));
        drop(pending);
        drop(_live);
        assert!(matches!(
            requests.dispatch(patch()),
            Err(RequestError::NoDesktop)
        ));
    }

    #[test]
    fn disconnect_after_dispatch_reports_unknown_outcome() {
        let requests = Arc::new(AppearanceRequests::default());
        let mailbox = Arc::new(ConnectionMailbox::new());
        let lease = requests.register(peer(1), &mailbox);
        let pending = requests.dispatch(patch()).unwrap();
        drop(lease);
        assert_eq!(
            pending.wait(Duration::from_secs(1)),
            Err(RequestError::OutcomeUnknown)
        );
    }

    #[test]
    fn a_command_hands_back_its_value_or_the_desktop_message() {
        let requests = Arc::new(CommandRequests::default());
        let mailbox = Arc::new(ConnectionMailbox::new());
        let desktop = peer(1);
        let _lease = requests.register(desktop.clone(), &mailbox);
        for (outcome, expected) in [
            (
                AppCommandOutcome::Done {
                    value: serde_json::json!(true),
                },
                Ok(serde_json::json!(true)),
            ),
            (
                AppCommandOutcome::Failed {
                    message: "没有这条命令".into(),
                },
                Err(RequestError::Desktop("没有这条命令".into())),
            ),
        ] {
            let pending = requests.dispatch(AppCommandRequest::List).unwrap();
            let ConnectionMail::AppCommandRequest(request) = mailbox.next().unwrap() else {
                panic!("expected command request");
            };
            requests
                .respond(&desktop, &request.request_id, outcome)
                .unwrap();
            assert_eq!(pending.wait(Duration::from_secs(1)), expected);
        }
    }
}
