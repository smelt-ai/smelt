//! 运行时客户端：桌面、手机连 smeltd 都走这一个入口。
//!
//! 只做连接和一问一答的帧收发，不做自动重试。有副作用的请求结果未知时能否重放，
//! 只有调用方知道（例如 `agent.send` 要复用同一个 `command_id`）。

use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::connector::Connector;
use crate::protocol::control::{
    CONTROL_API_VERSION, ControlCall, ControlClientError, EmptyParams, SystemDescribe,
    SystemDescribeResult, decode_response_with_version, encode_request_with_version,
};

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
    pub fn connect(&self) -> io::Result<UnixStream> {
        self.connector.connect()
    }

    /// 新开一条连接并设好读写时限。短请求用它。
    pub fn connect_with_timeout(&self, timeout: Duration) -> io::Result<UnixStream> {
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

    pub fn call_with_version<C: ControlCall>(
        &self,
        control_version: u16,
        params: &C::Params,
        timeout: Duration,
    ) -> Result<C::Result, ControlClientError> {
        let request_id = format!("request-{}", uuid::Uuid::new_v4().simple());
        let request = encode_request_with_version::<C>(control_version, &request_id, params)?;
        let transport = |error: io::Error| ControlClientError::Transport(error.to_string());
        let stream = self.connect_with_timeout(timeout).map_err(transport)?;
        let line = exchange_line(stream, &request.to_string()).map_err(transport)?;
        if line.trim().is_empty() {
            return Err(ControlClientError::Transport(
                "smeltd closed the control request without a response".to_string(),
            ));
        }
        let response = serde_json::from_str(&line)
            .map_err(|error| ControlClientError::InvalidResponse(error.to_string()))?;
        decode_response_with_version::<C>(control_version, &request_id, response)
    }

    /// 握手：运行时身份、版本、平台和支持的方法。
    pub fn describe(&self) -> Result<SystemDescribeResult, ControlClientError> {
        self.call::<SystemDescribe>(&EmptyParams::default(), DEFAULT_REQUEST_TIMEOUT)
    }
}

fn exchange_line(mut stream: UnixStream, request: &str) -> io::Result<String> {
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
            },
        }
    }

    /// 每次连接给出一对 socket，另一端交给 `serve` 在新线程里处理一条请求。
    fn client_serving(
        serve: impl Fn(Value) -> Value + Send + Sync + Clone + 'static,
    ) -> RuntimeClient {
        RuntimeClient::new(move || {
            let (client, server) = UnixStream::pair()?;
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

    #[test]
    fn request_reports_a_silent_disconnect_as_closed() {
        let client = RuntimeClient::new(|| {
            let (client, server) = UnixStream::pair()?;
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
}
