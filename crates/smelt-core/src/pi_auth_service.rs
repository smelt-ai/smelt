//! `pi.auth.*`：Pi 内置 provider 的登录、注销、模型目录。
//!
//! 凭据助手是运行时那台机器上的子进程，凭据也写在那里。登录是一场有来有回的会话
//! （授权链接、device code、要用户粘回来的 code），这里把它挂在一个登录 id 上：客户端
//! 长轮询 [`events`] 取事件，用 [`answer`] 回答提问，[`cancel`] 放弃。打开浏览器是
//! 客户端的事——授权链接要在用户面前的那台机器上打开。
//!
//! 客户端走掉不说一声（窗口关了、断线）时，登录不能一直挂着占回调端口：超过
//! [`ABANDONED_AFTER`] 没人来取事件就杀掉。

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::pi_auth::{PiAuthProvider, PiLoginEvent, PiLoginMethod, PiLoginSession};
use crate::provider_api::DiscoveredModel;

/// 长轮询最多等这么久就空手返回，客户端接着再来。比控制调用的超时短得多。
const POLL_WAIT: Duration = Duration::from_secs(20);
/// 这么久没人来取事件，就当客户端走了。
const ABANDONED_AFTER: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidersResult {
    pub providers: Vec<PiAuthProvider>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsResult {
    pub models: Vec<DiscoveredModel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStartParams {
    pub provider_id: String,
    pub method: PiLoginMethod,
    /// 关掉助手的提问代答，每一问都交回给用户。
    pub ask_all: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginIdParams {
    pub login_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginEventsParams {
    pub login_id: String,
    /// 已经拿到的事件数；返回从这之后的。
    pub after: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginEventsResult {
    pub events: Vec<PiLoginEvent>,
    /// 登录已经结束，这批之后不会再有事件。
    pub finished: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginAnswerParams {
    pub login_id: String,
    pub prompt_id: String,
    pub value: String,
}

pub fn providers() -> Result<ProvidersResult, String> {
    Ok(ProvidersResult {
        providers: crate::pi_auth::list_providers(&|_| {})?,
    })
}

pub fn models(provider_id: &str) -> Result<ModelsResult, String> {
    Ok(ModelsResult {
        models: crate::pi_auth::list_models(provider_id, &|_| {})?,
    })
}

pub fn logout(provider_id: &str) -> Result<(), String> {
    crate::pi_auth::logout(provider_id, &|_| {})
}

struct Login {
    /// 丢掉它就杀掉子进程。事件流结束后由读事件的线程清掉。
    session: Option<PiLoginSession>,
    events: Vec<PiLoginEvent>,
    finished: bool,
    last_poll: Instant,
}

struct Logins {
    entries: Mutex<HashMap<String, Login>>,
    changed: Condvar,
}

fn logins() -> &'static Logins {
    static LOGINS: OnceLock<Logins> = OnceLock::new();
    LOGINS.get_or_init(|| Logins {
        entries: Mutex::new(HashMap::new()),
        changed: Condvar::new(),
    })
}

/// 清掉没人要的登录：子进程跟着 `PiLoginSession` 一起被杀。
fn drop_abandoned(entries: &mut HashMap<String, Login>) {
    entries.retain(|_, login| login.last_poll.elapsed() < ABANDONED_AFTER);
}

pub fn start(params: LoginStartParams) -> Result<String, String> {
    let (session, receiver) =
        PiLoginSession::start(&params.provider_id, params.method, params.ask_all, &|_| {})?;
    let login_id = uuid::Uuid::new_v4().simple().to_string();
    {
        let mut entries = logins().entries.lock().map_err(|e| e.to_string())?;
        drop_abandoned(&mut entries);
        entries.insert(
            login_id.clone(),
            Login {
                session: Some(session),
                events: Vec::new(),
                finished: false,
                last_poll: Instant::now(),
            },
        );
    }
    let id = login_id.clone();
    std::thread::spawn(move || {
        for event in futures::executor::block_on_stream(receiver) {
            let logins = logins();
            let Ok(mut entries) = logins.entries.lock() else {
                return;
            };
            let Some(login) = entries.get_mut(&id) else {
                // 已取消或被当成没人要清掉了。
                return;
            };
            login.events.push(event);
            logins.changed.notify_all();
        }
        let logins = logins();
        if let Ok(mut entries) = logins.entries.lock()
            && let Some(login) = entries.get_mut(&id)
        {
            login.finished = true;
            login.session = None;
            logins.changed.notify_all();
        }
    });
    Ok(login_id)
}

/// 取 `after` 之后的事件；没有新事件时最多等 [`POLL_WAIT`]。结束且取完后登录被清掉，
/// 再来取就是「不存在」。
pub fn events(params: LoginEventsParams) -> Result<LoginEventsResult, String> {
    let logins = logins();
    let deadline = Instant::now() + POLL_WAIT;
    let mut entries = logins.entries.lock().map_err(|e| e.to_string())?;
    loop {
        let login = entries
            .get_mut(&params.login_id)
            .ok_or_else(|| "这次登录已经结束或被取消".to_string())?;
        login.last_poll = Instant::now();
        if login.events.len() > params.after || login.finished {
            let events = login
                .events
                .get(params.after..)
                .unwrap_or_default()
                .to_vec();
            let finished = login.finished;
            if finished {
                entries.remove(&params.login_id);
            }
            return Ok(LoginEventsResult { events, finished });
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(LoginEventsResult::default());
        }
        entries = logins
            .changed
            .wait_timeout(entries, deadline - now)
            .map_err(|e| e.to_string())?
            .0;
    }
}

pub fn answer(params: LoginAnswerParams) -> Result<(), String> {
    let entries = logins().entries.lock().map_err(|e| e.to_string())?;
    let session = entries
        .get(&params.login_id)
        .and_then(|login| login.session.as_ref())
        .ok_or_else(|| "这次登录已经结束或被取消".to_string())?;
    session.answer(&params.prompt_id, &params.value);
    Ok(())
}

/// 放弃登录：子进程撤掉回调服务器并退出。没有这次登录也算成功——要的结果已经是了。
pub fn cancel(params: LoginIdParams) -> Result<(), String> {
    let removed = logins()
        .entries
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&params.login_id);
    if let Some(session) = removed.and_then(|login| login.session) {
        session.cancel();
    }
    logins().changed.notify_all();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 运行时转给客户端的事件，客户端按同一份协议读回来。
    #[test]
    fn login_events_round_trip_on_the_wire() {
        let events = vec![
            PiLoginEvent::DeviceCode {
                user_code: "ABCD-1234".into(),
                verification_uri: "https://github.com/login/device".into(),
                interval_seconds: Some(5),
                expires_in_seconds: None,
            },
            PiLoginEvent::Prompt {
                id: "p1".into(),
                prompt_type: crate::pi_auth::PiPromptKind::ManualCode,
                message: "粘贴 code".into(),
                placeholder: None,
                options: Vec::new(),
            },
            PiLoginEvent::Done,
        ];
        let wire = serde_json::to_value(LoginEventsResult {
            events: events.clone(),
            finished: true,
        })
        .unwrap();
        assert_eq!(wire["events"][0]["event"], "device_code");
        assert_eq!(wire["events"][0]["userCode"], "ABCD-1234");
        let decoded: LoginEventsResult = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.events, events);
    }

    #[test]
    fn polling_an_unknown_login_is_an_error_and_cancelling_it_is_not() {
        let missing = LoginIdParams {
            login_id: "no-such-login".into(),
        };
        assert!(
            events(LoginEventsParams {
                login_id: missing.login_id.clone(),
                after: 0,
            })
            .is_err()
        );
        assert!(cancel(missing).is_ok());
    }
}
