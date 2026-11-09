//! 进程的 tracing 接收端：把插件和 iroh 的 tracing 事件落进 `~/.smelt/app.log`。
//!
//! 插件（gpui-shell）的来源记作 `plugin`。
//! 插件的 `console.log` / `console.error` 不写 stderr，而是发 tracing 事件（target
//! `gpui_shell::script`）；脚本里没接住的异常、Promise 失败也是 gpui-shell 发的 tracing
//! 事件。不接住它们，插件作者调试时什么都看不见。
//!
//! 脚本输出全收，gpui-shell 自己的只收警告和错误。gpui-shell 不在事件里带插件身份，所有
//! 插件共用一个脚本运行时，分不出是哪个插件说的；插件自己在消息里写前缀。
//!
//! iroh（连远程机器的那套，含 `iroh_relay` 等子库）只收警告和错误，来源记作 `iroh`：
//! 不接的话 endpoint 连不上 relay 时 app.log 里只有拨号超时，看不出卡在哪。
//!
//! 别的库发的 tracing 事件一律不碰。

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};

/// 插件 `console.*` 的 target。
const SCRIPT_TARGET: &str = "gpui_shell::script";
const SHELL_TARGET: &str = "gpui_shell";

/// 装上全局的 tracing 接收端。进程启动时调一次。别处已经装过就不装，不抢。
pub(crate) fn install() {
    let subscriber = tracing_subscriber::registry().with(AppLog::new(|scope, level, message| {
        smelt_sys::app_log::log(level, scope, message);
    }));
    let _ = tracing::subscriber::set_global_default(subscriber);
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

/// 收不收这条，收的话记成哪个来源：脚本输出全收；gpui-shell 自己的、iroh 的只收警告和
/// 错误。
fn source(metadata: &Metadata<'_>) -> Option<&'static str> {
    let target = metadata.target();
    if target == SCRIPT_TARGET {
        return Some("plugin");
    }
    let source = if target == SHELL_TARGET || target.starts_with("gpui_shell::") {
        "plugin"
    } else if is_iroh(target) {
        "iroh"
    } else {
        return None;
    };
    (*metadata.level() <= Level::WARN).then_some(source)
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
        source(metadata).is_some()
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let Some(source) = source(metadata) else {
            return;
        };
        let mut message = MessageText::default();
        event.record(&mut message);
        // 库自己的事件标上 target，和插件的 console 输出分得开。
        let text = if metadata.target() == SCRIPT_TARGET {
            message.0
        } else {
            format!("{}: {}", metadata.target(), message.0)
        };
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
        let lines = lines.lock().unwrap().clone();
        lines
    }

    fn line(scope: &str, level: &str, message: &str) -> (String, String, String) {
        (scope.to_string(), level.to_string(), message.to_string())
    }

    fn plugin(level: &str, message: &str) -> (String, String, String) {
        line("plugin", level, message)
    }

    #[test]
    fn script_output_is_kept_at_every_level() {
        let lines = capture(|| {
            tracing::debug!(target: "gpui_shell::script", "{}", "starting");
            tracing::info!(target: "gpui_shell::script", "{}", "[forge] 2 sessions");
            tracing::error!(target: "gpui_shell::script", "{}", "boom");
        });
        assert_eq!(
            lines,
            [
                plugin("debug", "starting"),
                plugin("info", "[forge] 2 sessions"),
                plugin("error", "boom"),
            ]
        );
    }

    #[test]
    fn shell_keeps_only_warnings_and_errors_and_names_the_target() {
        let lines = capture(|| {
            tracing::debug!(target: "gpui_shell::engine", "noise");
            tracing::error!(target: "gpui_shell::engine::quickjs::scheduler", "unhandled rejection");
            tracing::warn!(target: "gpui_shell", "careful");
        });
        assert_eq!(
            lines,
            [
                plugin(
                    "error",
                    "gpui_shell::engine::quickjs::scheduler: unhandled rejection"
                ),
                plugin("warn", "gpui_shell: careful"),
            ]
        );
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
            tracing::error!(target: "gpui_shellfish", "not ours either");
            tracing::error!(target: "irohish::thing", "nor this");
        });
        assert!(lines.is_empty());
    }
}
