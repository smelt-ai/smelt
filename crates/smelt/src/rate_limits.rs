//! Codex / Claude 订阅额度读取。
//!
//! Codex 复用官方 CLI 的 app-server JSON-RPC，不直接碰 OAuth token；Claude 读取
//! Claude Code 自己维护的 usage cache。两条路径都可能随上游版本变化，调用方必须
//! 把失败当成“暂不可用”，不能影响工作台启动或会话操作。

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct RateLimitWindow {
    pub used_percent: f64,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct ProviderRateLimits {
    pub session: Option<RateLimitWindow>,
    pub weekly: Option<RateLimitWindow>,
}

#[derive(Clone, Debug, Default)]
pub struct RateLimitSnapshot {
    pub codex: Option<ProviderRateLimits>,
    pub claude: Option<ProviderRateLimits>,
}

pub fn fetch_all() -> RateLimitSnapshot {
    let codex = fetch_codex().map_err(|e| {
        eprintln!("[rate-limits] Codex 用量不可用：{e}");
        e
    });
    let claude = fetch_claude_cache().map_err(|e| {
        eprintln!("[rate-limits] Claude 用量不可用：{e}");
        e
    });
    RateLimitSnapshot {
        codex: codex.ok(),
        claude: claude.ok(),
    }
}

fn parse_timestamp(v: &serde_json::Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(if n > 10_000_000_000 { n / 1000 } else { n });
    }
    let s = v.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(if n > 10_000_000_000 { n / 1000 } else { n });
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp())
}

fn parse_window(
    value: Option<&serde_json::Value>,
    percent_keys: &[&str],
) -> Option<RateLimitWindow> {
    let value = value?;
    let used_percent = percent_keys
        .iter()
        .find_map(|key| value.get(*key).and_then(|v| v.as_f64()))?
        .clamp(0.0, 100.0);
    let resets_at = value
        .get("resetsAt")
        .or_else(|| value.get("resets_at"))
        .and_then(parse_timestamp);
    Some(RateLimitWindow {
        used_percent,
        resets_at,
    })
}

fn parse_codex_response(v: &serde_json::Value) -> Option<ProviderRateLimits> {
    let limits = v.get("result")?.get("rateLimits")?;
    let primary = limits.get("primary");
    let secondary = limits.get("secondary");
    let mut session = None;
    let mut weekly = None;

    for raw in [primary, secondary].into_iter().flatten() {
        let Some(parsed) = parse_window(Some(raw), &["usedPercent"]) else {
            continue;
        };
        match raw.get("windowDurationMins").and_then(|v| v.as_f64()) {
            Some(minutes) if (minutes - 300.0).abs() <= 1.0 => session = Some(parsed),
            Some(minutes) if (minutes - 10_080.0).abs() <= 1.0 => weekly = Some(parsed),
            _ if session.is_none() && primary == Some(raw) => session = Some(parsed),
            _ if weekly.is_none() && secondary == Some(raw) => weekly = Some(parsed),
            _ => {}
        }
    }

    (session.is_some() || weekly.is_some()).then_some(ProviderRateLimits { session, weekly })
}

fn fetch_codex() -> Result<ProviderRateLimits, String> {
    let login_path = smelt_core::login_env::login_path();
    let codex = std::env::split_paths(login_path)
        .map(|dir| dir.join("codex"))
        .find(|path| path.is_file())
        .ok_or("登录环境 PATH 中找不到 codex")?;
    let mut command = Command::new(codex);
    command.env("PATH", login_path);
    if let Some(codex_home) = smelt_core::login_env::codex_home() {
        command.env("CODEX_HOME", codex_home);
    }
    let mut child = command
        .args(["-s", "read-only", "-a", "untrusted", "app-server"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动 codex app-server 失败：{e}"))?;
    let mut stdin = child.stdin.take().ok_or("codex app-server 无 stdin")?;
    let stdout = child.stdout.take().ok_or("codex app-server 无 stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                let _ = tx.send(value);
            }
        }
    });

    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "clientInfo": { "name": "smelt", "version": env!("CARGO_PKG_VERSION") } }
        })
    )
    .map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())?;

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut requested_limits = false;
    let result = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err("codex 用量查询超时".to_string());
        }
        let value = match rx.recv_timeout(remaining) {
            Ok(value) => value,
            Err(_) => break Err("codex 用量查询超时".to_string()),
        };
        if value.get("id").and_then(|v| v.as_i64()) == Some(1) && !requested_limits {
            writeln!(
                stdin,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","method":"initialized","params":{}})
            )
            .map_err(|e| e.to_string())?;
            writeln!(
                stdin,
                "{}",
                serde_json::json!({
                    "jsonrpc":"2.0",
                    "id":2,
                    "method":"account/rateLimits/read",
                    "params":{}
                })
            )
            .map_err(|e| e.to_string())?;
            stdin.flush().map_err(|e| e.to_string())?;
            requested_limits = true;
        } else if value.get("id").and_then(|v| v.as_i64()) == Some(2) {
            break parse_codex_response(&value)
                .ok_or_else(|| format!("codex 未返回可识别的额度窗口：{value}"));
        }
    };

    let _ = child.kill();
    let _ = child.wait();
    result
}

fn fetch_claude_cache() -> Result<ProviderRateLimits, String> {
    let path = dirs::home_dir()
        .ok_or("找不到 home 目录")?
        .join(".claude")
        .join(".anthropic_usage_cache.json");
    let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    let usage = value.get("usage").ok_or("Claude usage cache 缺少 usage")?;
    let session = parse_window(usage.get("five_hour"), &["utilization", "used_percentage"]);
    let weekly = parse_window(usage.get("seven_day"), &["utilization", "used_percentage"]);
    (session.is_some() || weekly.is_some())
        .then_some(ProviderRateLimits { session, weekly })
        .ok_or_else(|| "Claude usage cache 没有额度窗口".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_windows_by_duration() {
        let value = serde_json::json!({
            "result": {
                "rateLimits": {
                    "primary": {
                        "usedPercent": 12.5,
                        "windowDurationMins": 300,
                        "resetsAt": 1_770_000_000
                    },
                    "secondary": {
                        "usedPercent": 34.0,
                        "windowDurationMins": 10_080,
                        "resetsAt": 1_770_604_800
                    }
                }
            }
        });
        let parsed = parse_codex_response(&value).unwrap();
        assert_eq!(parsed.session.unwrap().used_percent, 12.5);
        assert_eq!(parsed.weekly.unwrap().used_percent, 34.0);
    }

    #[test]
    fn parses_codex_weekly_window_with_null_secondary() {
        let value = serde_json::json!({
            "result": {
                "rateLimits": {
                    "primary": {
                        "usedPercent": 2,
                        "windowDurationMins": 10_080,
                        "resetsAt": 1_785_678_275
                    },
                    "secondary": null
                }
            }
        });
        let parsed = parse_codex_response(&value).unwrap();
        assert!(parsed.session.is_none());
        assert_eq!(parsed.weekly.unwrap().used_percent, 2.0);
    }

    #[test]
    fn parses_claude_window_shapes() {
        let value = serde_json::json!({
            "utilization": 23.5,
            "resets_at": "2026-07-27T10:00:00Z"
        });
        let parsed = parse_window(Some(&value), &["utilization", "used_percentage"]).unwrap();
        assert_eq!(parsed.used_percent, 23.5);
        assert!(parsed.resets_at.is_some());
    }
}
