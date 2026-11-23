//! 进程的日志接收端：把 NSH 插件的 `log` 事件和 iroh 的 `tracing` 事件落进 `~/.smelt/app.log`。
//!
//! - NSH JS 插件（`nsh-js`）的 `console.*`、未捕获异常走 `log` crate（target `"js"`），全级别收录，
//!   来源记作 `plugin`；NSH 运行时本身（`nsh_*`、`nsh::*`，排除帧统计 `nsh::fps`）只收警告和错误，
//!   同样记作 `plugin`。
//! - iroh（连远程机器的那套，含 `iroh_relay` 等子库）走 `tracing`，只收警告和错误，来源记作 `iroh`。
//! - 别的库发的日志事件一律不碰。

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};

/// 装上全局的 `tracing` 与 `log` 接收端。进程启动时调一次。别处已经装过就不装，不抢。
pub(crate) fn install() {
    let subscriber = tracing_subscriber::registry().with(AppLog::new(|scope, level, message| {
        smelt_sys::app_log::log(level, scope, message);
    }));
    let _ = tracing::subscriber::set_global_default(subscriber);
    if log::set_logger(&NSH_LOG).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
}

static NSH_LOG: NshLog = NshLog;

struct NshLog;

impl log::Log for NshLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        log_source(metadata.target(), metadata.level()).is_some()
    }

    fn log(&self, record: &log::Record<'_>) {
        let Some(scope) = log_source(record.target(), record.level()) else {
            return;
        };
        let message = format_log_record(record.target(), &record.args().to_string());
        smelt_sys::app_log::log(log_level_name(record.level()), scope, &message);
    }

    fn flush(&self) {}
}

fn log_source(target: &str, level: log::Level) -> Option<&'static str> {
    if target == "js" {
        return Some("plugin");
    }
    if target == "nsh::fps" || target.starts_with("nsh::fps::") {
        return None;
    }
    if is_nsh(target) && level <= log::Level::Warn {
        return Some("plugin");
    }
    None
}

fn format_log_record(target: &str, message: &str) -> String {
    if target == "js" {
        message.to_owned()
    } else {
        format!("{target}: {message}")
    }
}

fn is_nsh(target: &str) -> bool {
    let krate = target.split("::").next().unwrap_or(target);
    krate == "nsh" || krate.starts_with("nsh_")
}

fn log_level_name(level: log::Level) -> &'static str {
    match level {
        log::Level::Error => "error",
        log::Level::Warn => "warn",
        log::Level::Info => "info",
        log::Level::Debug => "debug",
        log::Level::Trace => "trace",
    }
}

/// 把要的 tracing 事件交给 `write(来源, 级别, 正文)`。
struct AppLog<W> {
    write: W,
}

impl<W: Fn(&str, &str, &str)> AppLog<W> {
    fn new(write: W) -> Self {
        Self { write }
    }
}

/// 收不收这条 tracing 事件：只收 iroh 及其子库的警告和错误。
fn tracing_source(metadata: &Metadata<'_>) -> Option<&'static str> {
    let target = metadata.target();
    if is_iroh(target) && *metadata.level() <= Level::WARN {
        Some("iroh")
    } else {
        None
    }
}

/// iroh 和它的子库（`iroh_relay`、`iroh_dns` 等）。
fn is_iroh(target: &str) -> bool {
    let krate = target.split("::").next().unwrap_or(target);
    krate == "iroh" || krate.starts_with("iroh_")
}

fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

impl<S, W> Layer<S> for AppLog<W>
where
    S: Subscriber,
    W: Fn(&str, &str, &str) + Send + Sync + 'static,
{
    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        tracing_source(metadata).is_some()
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let Some(source) = tracing_source(metadata) else {
            return;
        };
        let mut message = MessageText::default();
        event.record(&mut message);
        let text = format!("{}: {}", metadata.target(), message.0);
        (self.write)(source, level_name(metadata.level()), &text);
    }
}

/// 只取 `message` 字段，其余字段接在后面。
#[derive(Default)]
struct MessageText(String);

impl Visit for MessageText {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.insert_str(0, &format!("{value:?}"));
        } else {
            self.0.push_str(&format!(" {}={value:?}", field.name()));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn capture(body: impl FnOnce()) -> Vec<(String, String, String)> {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let subscriber =
            tracing_subscriber::registry().with(AppLog::new(move |scope, level, message| {
                sink.lock().unwrap().push((
                    scope.to_string(),
                    level.to_string(),
                    message.to_string(),
                ));
            }));
        tracing::subscriber::with_default(subscriber, body);
        lines.lock().unwrap().clone()
    }

    fn line(scope: &str, level: &str, message: &str) -> (String, String, String) {
        (scope.to_string(), level.to_string(), message.to_string())
    }

    #[test]
    fn nsh_js_logs_are_kept_at_every_level_and_runtime_only_warns() {
        assert_eq!(log_source("js", log::Level::Debug), Some("plugin"));
        assert_eq!(log_source("js", log::Level::Info), Some("plugin"));
        assert_eq!(log_source("js", log::Level::Error), Some("plugin"));
        assert_eq!(format_log_record("js", "[forge] ok"), "[forge] ok");

        assert_eq!(log_source("nsh_engine::updates", log::Level::Info), None);
        assert_eq!(
            log_source("nsh_engine::updates", log::Level::Warn),
            Some("plugin")
        );
        assert_eq!(
            log_source("nsh_platform_gpui::app", log::Level::Error),
            Some("plugin")
        );
        assert_eq!(
            format_log_record("nsh_js::protocol", "bad prop"),
            "nsh_js::protocol: bad prop"
        );
        assert_eq!(log_source("nsh::fps", log::Level::Warn), None);
        assert_eq!(log_source("nshish::foo", log::Level::Error), None);
    }

    #[test]
    fn iroh_keeps_only_warnings_and_errors_under_its_own_source() {
        let lines = capture(|| {
            tracing::info!(target: "iroh::socket", "link change detected");
            tracing::warn!(target: "iroh::socket", "failed to rebind transports");
            tracing::error!(target: "iroh_relay::client", "relay connect failed");
            tracing::warn!(target: "iroh", "endpoint closed");
        });
        assert_eq!(
            lines,
            [
                line("iroh", "warn", "iroh::socket: failed to rebind transports"),
                line("iroh", "error", "iroh_relay::client: relay connect failed"),
                line("iroh", "warn", "iroh: endpoint closed"),
            ]
        );
    }

    #[test]
    fn other_crates_are_left_alone() {
        let lines = capture(|| {
            tracing::error!(target: "reqwest::connect", "nope");
            tracing::error!(target: "irohish::thing", "nor this");
        });
        assert!(lines.is_empty());
    }
}
