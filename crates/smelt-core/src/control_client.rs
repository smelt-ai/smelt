//! Control API v1 的阻塞 Unix socket 客户端。
//!
//! wire 类型归 `control_api`；这里仅负责一次连接、一次请求、一次响应，不做自动重试。
//! `agent.send` 的上层调用方若要恢复未知结果，只能复用同一个稳定 `command_id`。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::control_api::{
    CONTROL_API_VERSION, ControlCall, ControlClientError, decode_response_with_version,
    encode_request_with_version,
};

pub const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

pub fn call<C: ControlCall>(params: &C::Params) -> Result<C::Result, ControlClientError> {
    call_at::<C>(
        &crate::daemon_state::smeltd_sock_path(),
        DEFAULT_CONTROL_TIMEOUT,
        params,
    )
}

pub fn call_at<C: ControlCall>(
    socket_path: &Path,
    timeout: Duration,
    params: &C::Params,
) -> Result<C::Result, ControlClientError> {
    call_at_with_version::<C>(socket_path, timeout, CONTROL_API_VERSION, params)
}

pub fn call_at_with_version<C: ControlCall>(
    socket_path: &Path,
    timeout: Duration,
    control_version: u16,
    params: &C::Params,
) -> Result<C::Result, ControlClientError> {
    let request_id = format!("request-{}", uuid::Uuid::new_v4().simple());
    let request = encode_request_with_version::<C>(control_version, &request_id, params)?;
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|error| ControlClientError::Transport(error.to_string()))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| ControlClientError::Transport(error.to_string()))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| ControlClientError::Transport(error.to_string()))?;
    writeln!(stream, "{request}")
        .map_err(|error| ControlClientError::Transport(error.to_string()))?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| ControlClientError::Transport(error.to_string()))?;
    if line.trim().is_empty() {
        return Err(ControlClientError::Transport(
            "smeltd closed the control request without a response".to_string(),
        ));
    }
    let response = serde_json::from_str(&line)
        .map_err(|error| ControlClientError::InvalidResponse(error.to_string()))?;
    decode_response_with_version::<C>(control_version, &request_id, response)
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::thread;

    use crate::control_api::{
        CONTROL_API_MIN_SUPPORTED_VERSION, CONTROL_API_VERSION, ControlProtocolDescription,
        ControlResponse, DaemonRuntimeInfo, EmptyParams, SystemDescribe, SystemDescribeResult,
    };

    use super::*;

    #[test]
    fn call_at_correlates_a_typed_response() {
        // macOS sockaddr_un.sun_path 只有 104 字节；系统 temp_dir 可能已经很深。
        let socket_path =
            std::path::PathBuf::from(format!("/tmp/smc-{}.sock", uuid::Uuid::new_v4().simple()));
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request_line)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&request_line).unwrap();
            let request_id = request["request_id"].as_str().unwrap();
            let response = ControlResponse::ok(
                request_id,
                SystemDescribeResult {
                    control: ControlProtocolDescription {
                        current_version: CONTROL_API_VERSION,
                        min_supported_version: CONTROL_API_MIN_SUPPORTED_VERSION,
                        max_supported_version: CONTROL_API_VERSION,
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
                    },
                },
            );
            writeln!(&stream, "{}", serde_json::to_value(response).unwrap()).unwrap();
        });

        let result = call_at::<SystemDescribe>(
            &socket_path,
            Duration::from_secs(1),
            &EmptyParams::default(),
        )
        .unwrap();
        assert_eq!(result.daemon.pid, 42);
        server.join().unwrap();
        std::fs::remove_file(socket_path).unwrap();
    }

    #[test]
    fn call_at_with_version_uses_the_negotiated_version() {
        let socket_path =
            std::path::PathBuf::from(format!("/tmp/smcv-{}.sock", uuid::Uuid::new_v4().simple()));
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request_line)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&request_line).unwrap();
            assert_eq!(request["control_version"], 7);
            let request_id = request["request_id"].as_str().unwrap();
            let response = ControlResponse::ok_with_version(
                7,
                request_id,
                SystemDescribeResult {
                    control: ControlProtocolDescription {
                        current_version: 7,
                        min_supported_version: 1,
                        max_supported_version: 7,
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
                    },
                },
            );
            writeln!(&stream, "{}", serde_json::to_value(response).unwrap()).unwrap();
        });

        let result = call_at_with_version::<SystemDescribe>(
            &socket_path,
            Duration::from_secs(1),
            7,
            &EmptyParams::default(),
        )
        .unwrap();

        assert_eq!(result.control.current_version, 7);
        server.join().unwrap();
        std::fs::remove_file(socket_path).unwrap();
    }
}
