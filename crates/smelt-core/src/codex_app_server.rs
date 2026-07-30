//! Codex hooks 信任授权。会话本身走标准 ACP（`@agentclientprotocol/codex-acp`，
//! 见 `agent_kind::default_acp_codex_cmd`），不再需要专属的 app-server driver；
//! 这里只保留一段独立的一次性 RPC：直接拉起 `codex app-server` 询问/授予 Smelt
//! 托管 hooks 的信任状态，跟会话走哪条 driver 无关。

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn send(writer: &Arc<Mutex<std::process::ChildStdin>>, value: serde_json::Value) -> bool {
    let mut writer = writer.lock().unwrap();
    writeln!(writer, "{value}").is_ok() && writer.flush().is_ok()
}

fn resolve_program(program: &str) -> std::path::PathBuf {
    if program.contains('/') {
        return program.into();
    }
    std::env::split_paths(crate::login_env::login_path())
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
        .unwrap_or_else(|| program.into())
}

#[derive(Clone, Debug)]
struct HookTrustListing {
    key: String,
    command: String,
    current_hash: String,
    trust_status: String,
}

fn hook_trust_listings(response: &serde_json::Value) -> Vec<HookTrustListing> {
    response
        .pointer("/result/data")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .flat_map(|scope| {
            scope
                .get("hooks")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
        })
        .filter_map(|hook| {
            Some(HookTrustListing {
                key: hook.get("key")?.as_str()?.to_string(),
                command: hook.get("command")?.as_str()?.to_string(),
                current_hash: hook.get("currentHash")?.as_str()?.to_string(),
                trust_status: hook.get("trustStatus")?.as_str()?.to_string(),
            })
        })
        .collect()
}

fn matching_hook_trust_listings(
    response: &serde_json::Value,
    hooks_path: &std::path::Path,
    expected_commands: &[String],
) -> Result<Vec<HookTrustListing>, String> {
    let path_prefix = format!("{}:", hooks_path.to_string_lossy());
    let expected: std::collections::HashSet<&str> =
        expected_commands.iter().map(String::as_str).collect();
    let mut matched: Vec<_> = hook_trust_listings(response)
        .into_iter()
        .filter(|hook| {
            hook.key.starts_with(&path_prefix) && expected.contains(hook.command.as_str())
        })
        .collect();
    matched.sort_by(|a, b| a.key.cmp(&b.key));
    matched.dedup_by(|a, b| a.key == b.key);
    let commands: std::collections::HashSet<&str> =
        matched.iter().map(|hook| hook.command.as_str()).collect();
    if matched.len() != expected.len() || commands != expected {
        return Err(format!(
            "Codex hooks/list 只匹配到 {}/{} 个 Smelt hooks",
            matched.len(),
            expected.len()
        ));
    }
    Ok(matched)
}

/// 通过 Codex app-server 自己的 RPC 信任 Smelt 管理的 hooks。只接受指定 hooks.json
/// 路径下、命令与 expected_commands 完全一致的条目；hash 只使用 hooks/list 返回值，
/// 写入后再次 list 验证，不复制 Codex 的私有 hash 算法。
pub fn grant_codex_hook_trust(
    hooks_path: &std::path::Path,
    cwd: &std::path::Path,
    expected_commands: &[String],
) -> Result<usize, String> {
    if expected_commands.is_empty() {
        return Err("没有待信任的 Codex hooks".into());
    }
    let mut command = Command::new(resolve_program("codex"));
    command
        .arg("app-server")
        .current_dir(cwd)
        .env("PATH", crate::login_env::login_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(codex_home) = crate::login_env::codex_home() {
        command.env("CODEX_HOME", codex_home);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动 Codex app-server 信任会话失败：{error}"))?;
    let result = (|| {
        let stdin = child.stdin.take().ok_or("Codex app-server 没有 stdin")?;
        let stdout = child.stdout.take().ok_or("Codex app-server 没有 stdout")?;
        let stderr = child.stderr.take().ok_or("Codex app-server 没有 stderr")?;
        std::thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
        let writer = Arc::new(Mutex::new(stdin));
        let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = line_tx.send(line);
            }
        });

        if !send(
            &writer,
            serde_json::json!({
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {"name":"smelt", "title":"Smelt", "version":env!("CARGO_PKG_VERSION")},
                    "capabilities": {"experimentalApi":true}
                }
            }),
        ) {
            return Err("写入 Codex app-server initialize 失败".into());
        }
        wait_response(&line_rx, 1)?;
        if !send(
            &writer,
            serde_json::json!({"method":"initialized","params":{}}),
        ) {
            return Err("写入 Codex app-server initialized 失败".into());
        }
        if !send(
            &writer,
            serde_json::json!({"id":2,"method":"hooks/list","params":{"cwds":[cwd]}}),
        ) {
            return Err("写入 Codex app-server hooks/list 失败".into());
        }
        let before = wait_response(&line_rx, 2)?;
        let matched = matching_hook_trust_listings(&before, hooks_path, expected_commands)?;
        let needing_trust: Vec<_> = matched
            .iter()
            .filter(|hook| hook.trust_status != "trusted")
            .collect();
        if !needing_trust.is_empty() {
            let value = needing_trust
                .iter()
                .map(|hook| {
                    (
                        hook.key.clone(),
                        serde_json::json!({"trusted_hash":hook.current_hash}),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            if !send(
                &writer,
                serde_json::json!({
                    "id":3,
                    "method":"config/batchWrite",
                    "params":{
                        "edits":[{"keyPath":"hooks.state","value":value,"mergeStrategy":"upsert"}],
                        "reloadUserConfig":true
                    }
                }),
            ) {
                return Err("写入 Codex app-server config/batchWrite 失败".into());
            }
            wait_response(&line_rx, 3)?;
        }
        let verify_id = if needing_trust.is_empty() { 3 } else { 4 };
        if !send(
            &writer,
            serde_json::json!({"id":verify_id,"method":"hooks/list","params":{"cwds":[cwd]}}),
        ) {
            return Err("写入 Codex app-server hooks/list 复核请求失败".into());
        }
        let after = wait_response(&line_rx, verify_id)?;
        let verified = matching_hook_trust_listings(&after, hooks_path, expected_commands)?;
        if verified.iter().any(|hook| hook.trust_status != "trusted") {
            return Err("Codex hooks/list 复核后仍有 Smelt hook 未信任".into());
        }
        Ok(needing_trust.len())
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn wait_response(
    lines: &std::sync::mpsc::Receiver<String>,
    id: i64,
) -> Result<serde_json::Value, String> {
    loop {
        let line = lines
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| format!("等待 Codex app-server 响应 {id} 超时"))?;
        let value: serde_json::Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if value.get("id").and_then(|v| v.as_i64()) == Some(id) {
            if value.get("error").is_some() {
                return Err(value.to_string());
            }
            return Ok(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_trust_matching_requires_exact_path_and_commands() {
        let response = serde_json::json!({"result":{"data":[{"hooks":[
            {
                "key":"/tmp/smelt-hooks.json:Stop:0:0",
                "command":"SMELT_HOOK_EVENT=Stop smelt-notify",
                "currentHash":"sha256:stop",
                "trustStatus":"pending"
            },
            {
                "key":"/tmp/smelt-hooks.json:SessionStart:0:0",
                "command":"SMELT_HOOK_EVENT=SessionStart smelt-notify",
                "currentHash":"sha256:start",
                "trustStatus":"trusted"
            },
            {
                "key":"/tmp/other-hooks.json:Stop:0:0",
                "command":"SMELT_HOOK_EVENT=Stop smelt-notify",
                "currentHash":"sha256:other",
                "trustStatus":"pending"
            },
            {
                "key":"/tmp/smelt-hooks.json:Stop:1:0",
                "command":"curl example.invalid",
                "currentHash":"sha256:foreign",
                "trustStatus":"pending"
            }
        ]}]}});
        let commands = vec![
            "SMELT_HOOK_EVENT=Stop smelt-notify".to_string(),
            "SMELT_HOOK_EVENT=SessionStart smelt-notify".to_string(),
        ];
        let matched = matching_hook_trust_listings(
            &response,
            std::path::Path::new("/tmp/smelt-hooks.json"),
            &commands,
        )
        .unwrap();
        assert_eq!(matched.len(), 2);
        assert!(matched.iter().all(|hook| {
            hook.key.starts_with("/tmp/smelt-hooks.json:") && commands.contains(&hook.command)
        }));

        let missing = vec!["SMELT_HOOK_EVENT=Missing smelt-notify".to_string()];
        assert!(
            matching_hook_trust_listings(
                &response,
                std::path::Path::new("/tmp/smelt-hooks.json"),
                &missing,
            )
            .is_err()
        );
    }
}
