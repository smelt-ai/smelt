//! 本机桌面外观修改的投递和回执。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Duration;

use crate::protocol::{ConnectionMail, ConnectionMailbox, PeerIdentity};
use smelt_event_protocol::{AppearanceApplyOutcome, AppearanceApplyRequested};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RequestError {
    NoDesktop,
    MultipleDesktops,
    DeliveryFailed,
    OutcomeUnknown,
    Invalid(String),
    Desktop(String),
}

#[derive(Default)]
struct State {
    desktops: BTreeMap<String, (PeerIdentity, Weak<ConnectionMailbox>)>,
    pending: BTreeMap<String, (String, mpsc::Sender<Result<(), RequestError>>)>,
}

#[derive(Default)]
pub(crate) struct AppearanceRequests {
    state: Mutex<State>,
}

pub(crate) struct AppearanceLease {
    requests: Arc<AppearanceRequests>,
    id: String,
}

pub(crate) struct PendingRequest {
    requests: Arc<AppearanceRequests>,
    id: String,
    receiver: mpsc::Receiver<Result<(), RequestError>>,
}

impl AppearanceRequests {
    pub(crate) fn register(
        self: &Arc<Self>,
        peer: PeerIdentity,
        mailbox: &Arc<ConnectionMailbox>,
    ) -> AppearanceLease {
        let id = uuid::Uuid::new_v4().to_string();
        self.state
            .lock()
            .unwrap()
            .desktops
            .insert(id.clone(), (peer, Arc::downgrade(mailbox)));
        AppearanceLease {
            requests: self.clone(),
            id,
        }
    }

    pub(crate) fn dispatch(
        self: &Arc<Self>,
        settings: BTreeMap<String, serde_json::Value>,
    ) -> Result<PendingRequest, RequestError> {
        let id = uuid::Uuid::new_v4().to_string();
        let request = AppearanceApplyRequested {
            request_id: id.clone(),
            settings,
        };
        if serde_json::to_vec(&request).map_or(true, |bytes| bytes.len() > 4 * 1024) {
            return Err(RequestError::Invalid("外观请求过大".into()));
        }
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
        if !mailbox.try_push(ConnectionMail::AppearanceRequest(request)) {
            state.pending.remove(&id);
            return Err(RequestError::DeliveryFailed);
        }
        Ok(PendingRequest {
            requests: self.clone(),
            id,
            receiver,
        })
    }

    pub(crate) fn respond(
        &self,
        peer: &PeerIdentity,
        id: &str,
        result: AppearanceApplyOutcome,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let (owner, _) = state.pending.get(id).ok_or("请求已结束或不存在")?;
        let (expected, _) = state.desktops.get(owner).ok_or("桌面连接已断开")?;
        if !same_process(expected, peer) {
            return Err("回执不是请求对应的桌面进程".into());
        }
        let (_, sender) = state.pending.remove(id).unwrap();
        let result = match result {
            AppearanceApplyOutcome::Applied => Ok(()),
            AppearanceApplyOutcome::Invalid { message } => Err(RequestError::Invalid(message)),
            AppearanceApplyOutcome::Failed { message } => Err(RequestError::Desktop(message)),
        };
        let _ = sender.send(result);
        Ok(())
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

impl PendingRequest {
    pub(crate) fn wait(self, timeout: Duration) -> Result<(), RequestError> {
        let result = self
            .receiver
            .recv_timeout(timeout)
            .unwrap_or(Err(RequestError::OutcomeUnknown));
        self.requests.state.lock().unwrap().pending.remove(&self.id);
        result
    }
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.requests.state.lock().unwrap().pending.remove(&self.id);
    }
}

impl Drop for AppearanceLease {
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
}
