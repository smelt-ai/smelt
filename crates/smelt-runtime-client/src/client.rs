//! 运行时客户端：桌面、手机连 smeltd 都走这一个入口。
//!
//! 只做连接和一问一答的帧收发，不做自动重试。有副作用的请求结果未知时能否重放，
//! 只有调用方知道。

use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::net::Shutdown;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::RuntimeStream;
use crate::connector::Connector;
use crate::protocol::control::{
    CONTROL_API_VERSION, ControlCall, ControlClientError, ControlError, ControlErrorCode,
    ControlMethod, EmptyParams, SystemDescribe, SystemDescribeResult, decode_method_response,
    decode_response_with_version, encode_method_request, encode_request_with_version,
};

/// 短请求的取消句柄：取消时关闭正在等待的流，让后台阻塞读立即返回。
#[derive(Clone, Default)]
pub struct CallCancel {
    inner: Arc<Mutex<CallCancelState>>,
}

#[derive(Default)]
struct CallCancelState {
    cancelled: bool,
    stream: Option<RuntimeStream>,
}

impl fmt::Debug for CallCancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallCancel")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl CallCancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        let stream = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.cancelled = true;
            guard.stream.take()
        };
        if let Some(stream) = stream {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancelled
    }

    fn attach(&self, stream: RuntimeStream) -> bool {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if guard.cancelled {
            drop(guard);
            let _ = stream.shutdown(Shutdown::Both);
            return false;
        }
        guard.stream = Some(stream);
        true
    }

    fn clear(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).stream = None;
    }
}

struct CancelStreamGuard<'a> {
    cancel: Option<&'a CallCancel>,
}

impl Drop for CancelStreamGuard<'_> {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel {
            cancel.clear();
        }
    }
}

/// 短请求的默认读写时限。调用方都在后台线程，但运行时不回包时仍要及时放掉线程。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct RuntimeClient {
    connector: Arc<dyn Connector>,
}

impl fmt::Debug for RuntimeClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeClient").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum RequestError {
    Transport(io::Error),
    /// 运行时读完请求就断开，没有回一行。老运行时不认识的 op 就是这样。
    Closed,
    InvalidResponse(String),
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "smeltd transport: {error}"),
            Self::Closed => f.write_str("smeltd closed the request without a response"),
            Self::InvalidResponse(message) => write!(f, "invalid smeltd response: {message}"),
        }
    }
}

impl std::error::Error for RequestError {}

impl RuntimeClient {
    pub fn new(connector: impl Connector + 'static) -> Self {
        Self {
            connector: Arc::new(connector),
        }
    }

    /// 新开一条不设超时的连接，首帧由调用方写。长连接（订阅、ACP 会话、终端流）用它。
    pub fn connect(&self) -> io::Result<RuntimeStream> {
        self.connector.connect()
    }

    /// 新开一条连接并设好读写时限。短请求用它。
    pub fn connect_with_timeout(&self, timeout: Duration) -> io::Result<RuntimeStream> {
        let stream = self.connector.connect()?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(stream)
    }

    /// 一问一答的首帧请求：写一行 JSON，读一行 JSON。
    ///
    /// 不解释 `ok` 字段：各 op 的失败形状不统一，由调用方判断。
    pub fn request(&self, request: &Value, timeout: Duration) -> Result<Value, RequestError> {
        let stream = self
            .connect_with_timeout(timeout)
            .map_err(RequestError::Transport)?;
        let line = exchange_line(stream, &request.to_string()).map_err(RequestError::Transport)?;
        if line.trim().is_empty() {
            return Err(RequestError::Closed);
        }
        serde_json::from_str(line.trim())
            .map_err(|error| RequestError::InvalidResponse(error.to_string()))
    }

    /// 首帧 op 的一问一答，再看回包的 `ok`：`false` 时把 `error` 当失败原因。
    pub fn request_ok(&self, request: &Value, timeout: Duration) -> Result<Value, String> {
        let response = self
            .request(request, timeout)
            .map_err(|error| error.to_string())?;
        if response.get("ok").and_then(Value::as_bool) == Some(false) {
            return Err(response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("session operation failed")
                .to_string());
        }
        Ok(response)
    }

    pub fn call<C: ControlCall>(
        &self,
        params: &C::Params,
        timeout: Duration,
    ) -> Result<C::Result, ControlClientError> {
        self.call_with_version::<C>(CONTROL_API_VERSION, params, timeout)
    }

    /// 可被 [`CallCancel`] 中途打断的 [`Self::call`]。
    pub fn call_cancellable<C: ControlCall>(
        &self,
        params: &C::Params,
        timeout: Duration,
        cancel: &CallCancel,
    ) -> Result<C::Result, ControlClientError> {
        let request_id = new_request_id();
        let request = encode_request_with_version::<C>(CONTROL_API_VERSION, &request_id, params)?;
        let response = self.exchange_control_with_cancel(&request, timeout, Some(cancel))?;
        decode_response_with_version::<C>(CONTROL_API_VERSION, &request_id, response)
    }

    pub fn call_with_version<C: ControlCall>(
        &self,
        control_version: u16,
        params: &C::Params,
        timeout: Duration,
    ) -> Result<C::Result, ControlClientError> {
        let request_id = new_request_id();
        let request = encode_request_with_version::<C>(control_version, &request_id, params)?;
        let response = self.exchange_control(&request, timeout)?;
        decode_response_with_version::<C>(control_version, &request_id, response)
    }

    /// 按方法名调，参数和结果都是 JSON：调哪个方法要到运行时才知道的（脚本插件）用它，
    /// 编译期知道的用 [`Self::call`]。
    pub fn call_method(
        &self,
        method: ControlMethod,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ControlClientError> {
        let request_id = new_request_id();
        let request = encode_method_request(CONTROL_API_VERSION, &request_id, method, params)?;
        let response = self.exchange_control(&request, timeout)?;
        decode_method_response(CONTROL_API_VERSION, &request_id, response)
    }

    /// 发一个控制请求，读回一行 JSON。
    fn exchange_control(
        &self,
        request: &Value,
        timeout: Duration,
    ) -> Result<Value, ControlClientError> {
        self.exchange_control_with_cancel(request, timeout, None)
    }

    fn exchange_control_with_cancel(
        &self,
        request: &Value,
        timeout: Duration,
        cancel: Option<&CallCancel>,
    ) -> Result<Value, ControlClientError> {
        let transport = |error: io::Error| ControlClientError::Transport(error.to_string());
        if cancel.is_some_and(CallCancel::is_cancelled) {
            return Err(ControlClientError::Transport(
                "control request cancelled".to_string(),
            ));
        }
        let stream = self.connect_with_timeout(timeout).map_err(transport)?;
        let _guard = CancelStreamGuard { cancel };
        if let Some(cancel) = cancel {
            let handle = stream.try_clone().map_err(transport)?;
            if !cancel.attach(handle) {
                return Err(ControlClientError::Transport(
                    "control request cancelled".to_string(),
                ));
            }
        }
        let line = exchange_line(stream, &request.to_string()).map_err(|error| {
            if cancel.is_some_and(CallCancel::is_cancelled) {
                ControlClientError::Transport("control request cancelled".to_string())
            } else {
                transport(error)
            }
        })?;
        if cancel.is_some_and(CallCancel::is_cancelled) {
            return Err(ControlClientError::Transport(
                "control request cancelled".to_string(),
            ));
        }
        if line.trim().is_empty() {
            return Err(ControlClientError::Transport(
                "smeltd closed the control request without a response".to_string(),
            ));
        }
        serde_json::from_str(&line)
            .map_err(|error| ControlClientError::InvalidResponse(error.to_string()))
    }

    /// 握手：运行时身份、版本、平台和支持的方法。
    ///
    /// 运行时不支持这个客户端的控制协议版本时，按它报的最高版本再问一次：
    /// `system.describe` 是冻结契约，哪个版本的回复都解得了。这样版本对不上的运行时也
    /// 读得出它是什么版本，兼容与否交给调用方判断（[`crate::compat`]）。
    pub fn describe(&self) -> Result<SystemDescribeResult, ControlClientError> {
        let params = EmptyParams::default();
        match self.call::<SystemDescribe>(&params, DEFAULT_REQUEST_TIMEOUT) {
            Err(ControlClientError::Protocol(ControlError {
                code: ControlErrorCode::UnsupportedVersion,
                supported_versions: Some(range),
                ..
            })) if range.max_supported_version != CONTROL_API_VERSION => self
                .call_with_version::<SystemDescribe>(
                    range.max_supported_version,
                    &params,
                    DEFAULT_REQUEST_TIMEOUT,
                ),
            result => result,
        }
    }
}

/// 控制请求的 id：一问一答对得上就行，每次新取。
fn new_request_id() -> String {
    format!("request-{}", uuid::Uuid::new_v4().simple())
}

fn exchange_line(mut stream: RuntimeStream, request: &str) -> io::Result<String> {
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(line)
}

#[cfg(test)]
mod tests {
    use std::thread;

    use crate::protocol::control::{
        CONTROL_API_MIN_SUPPORTED_VERSION, ControlProtocolDescription, ControlResponse,
        DaemonRuntimeInfo,
    };

    use super::*;

    fn describe_result(version: u16) -> SystemDescribeResult {
        SystemDescribeResult {
            control: ControlProtocolDescription {
                current_version: version,
                min_supported_version: CONTROL_API_MIN_SUPPORTED_VERSION,
                max_supported_version: version,
                methods: vec!["system.describe".to_string()],
            },
            daemon: DaemonRuntimeInfo {
                version: "test".to_string(),
                exe_mtime: 7,
                exe: None,
                pid: 42,
                started_at: 1,
                session_count: 0,
                agent_event_version: 1,
                runtime_id: Some("runtime-a".to_string()),
                os: Some("linux".to_string()),
                arch: Some("aarch64".to_string()),
                hostname: Some("linux-box".to_string()),
                desktop: None,
                smelt_home: None,
            },
            events: None,
        }
    }

    /// 每次连接给出一对 socket，另一端交给 `serve` 在新线程里处理一条请求。
    fn client_serving(
        serve: impl Fn(Value) -> Value + Send + Sync + Clone + 'static,
    ) -> RuntimeClient {
        RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let serve = serve.clone();
            thread::spawn(move || {
                let mut line = String::new();
                BufReader::new(server.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let response = serve(serde_json::from_str(&line).unwrap());
                writeln!(&server, "{response}").unwrap();
            });
            Ok(client)
        })
    }

    #[test]
    fn describe_correlates_a_typed_response() {
        let client = client_serving(|request| {
            assert_eq!(request["op"], "control");
            assert_eq!(request["method"], "system.describe");
            let request_id = request["request_id"].as_str().unwrap();
            serde_json::to_value(ControlResponse::ok(
                request_id,
                describe_result(CONTROL_API_VERSION),
            ))
            .unwrap()
        });

        let result = client.describe().unwrap();

        assert_eq!(result.daemon.pid, 42);
        assert_eq!(result.daemon.runtime_id.as_deref(), Some("runtime-a"));
    }

    #[test]
    fn call_with_version_uses_the_negotiated_version() {
        let client = client_serving(|request| {
            assert_eq!(request["control_version"], 7);
            let request_id = request["request_id"].as_str().unwrap();
            serde_json::to_value(ControlResponse::ok_with_version(
                7,
                request_id,
                describe_result(7),
            ))
            .unwrap()
        });

        let result = client
            .call_with_version::<SystemDescribe>(
                7,
                &EmptyParams::default(),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .unwrap();

        assert_eq!(result.control.current_version, 7);
    }

    /// 冻结契约：客户端必须读得懂没有新字段的老运行时回复。
    #[test]
    fn describe_accepts_a_runtime_that_predates_runtime_identity() {
        let client = client_serving(|request| {
            let request_id = request["request_id"].as_str().unwrap();
            let mut response = serde_json::to_value(ControlResponse::ok(
                request_id,
                describe_result(CONTROL_API_VERSION),
            ))
            .unwrap();
            let daemon = response["result"]["daemon"].as_object_mut().unwrap();
            daemon.remove("runtime_id");
            daemon.remove("os");
            daemon.remove("arch");
            response
        });

        let result = client.describe().unwrap();

        assert_eq!(result.daemon.runtime_id, None);
        assert_eq!(result.daemon.os, None);
    }

    /// 只认旧控制协议的运行时先拒绝，再按它报的版本问一次，读得出它的版本。
    #[test]
    fn describe_falls_back_to_the_runtime_control_version() {
        let old = CONTROL_API_VERSION - 1;
        let client = client_serving(move |request| {
            let request_id = request["request_id"].as_str().unwrap();
            let response = if request["control_version"] == old {
                let mut result = describe_result(old);
                result.daemon.version = "0.9.0-rc.3".to_string();
                serde_json::to_value(ControlResponse::ok_with_version(old, request_id, result))
            } else {
                serde_json::to_value(ControlResponse::<SystemDescribeResult>::error_with_version(
                    old,
                    request_id,
                    ControlError::unsupported_version(CONTROL_API_VERSION, old, old),
                ))
            };
            response.unwrap()
        });

        let result = client.describe().unwrap();

        assert_eq!(result.daemon.version, "0.9.0-rc.3");
        assert_eq!(result.control.max_supported_version, old);
    }

    #[test]
    fn request_reports_a_silent_disconnect_as_closed() {
        let client = RuntimeClient::new(|| {
            let (client, server) = RuntimeStream::pair()?;
            thread::spawn(move || {
                let mut line = String::new();
                let _ = BufReader::new(server).read_line(&mut line);
            });
            Ok(client)
        });

        let error = client
            .request(
                &serde_json::json!({ "op": "version" }),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .unwrap_err();

        assert!(matches!(error, RequestError::Closed), "{error}");
    }

    #[test]
    fn call_cancellable_unblocks_immediately_when_cancelled() {
        use std::sync::mpsc;
        use std::time::Instant;

        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (_release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let client = RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let accepted_tx = accepted_tx.clone();
            let release_rx = Arc::clone(&release_rx);
            thread::spawn(move || {
                let mut line = String::new();
                let _ = BufReader::new(server.try_clone().unwrap()).read_line(&mut line);
                let _ = accepted_tx.send(());
                let _ = release_rx.lock().unwrap().recv();
                drop(server);
            });
            Ok(client)
        });

        let cancel = CallCancel::new();
        let call_cancel = cancel.clone();
        let started = Instant::now();
        let handle = thread::spawn(move || {
            client.call_cancellable::<SystemDescribe>(
                &EmptyParams::default(),
                Duration::from_secs(30),
                &call_cancel,
            )
        });
        accepted_rx.recv().unwrap();
        cancel.cancel();

        let err = handle.join().unwrap().unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancel should unblock without waiting for 30s timeout"
        );
        assert!(matches!(err, ControlClientError::Transport(_)), "{err}");
        assert!(cancel.is_cancelled());
    }
}
