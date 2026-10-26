//! 插件的日志：把 gpui-shell 的 tracing 事件落进 `~/.smelt/app.log`，来源记作 `plugin`。
//!
//! 插件的 `console.log` / `console.error` 不写 stderr，而是发 tracing 事件（target
//! `gpui_shell::script`）；脚本里没接住的异常、Promise 失败也是 gpui-shell 发的 tracing
//! 事件。不接住它们，插件作者调试时什么都看不见。
//!
//! 只收 gpui-shell 的：脚本输出全收，gpui-shell 自己的只收警告和错误。别的库发的
//! tracing 事件一律不碰。gpui-shell 不在事件里带插件身份，所有插件共用一个脚本运行时，
//! 分不出是哪个插件说的；插件自己在消息里写前缀。

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};

/// 插件 `console.*` 的 target。
const SCRIPT_TARGET: &str = "gpui_shell::script";
const SHELL_TARGET: &str = "gpui_shell";

/// 装上全局的 tracing 接收端。进程启动时调一次。别处已经装过就不装，不抢。
pub(crate) fn install() {
    let subscriber = tracing_subscriber::registry().with(PluginLog::new(|level, message| {
        smelt_sys::app_log::log(level, "plugin", message);
    }));
    let _ = tracing::subscriber::set_global_default(subscriber);
}

struct PluginLog<W> {
    write: W,
}

impl<W: Fn(&str, &str)> PluginLog<W> {
    fn new(write: W) -> Self {
        Self { write }
    }
}

/// 收不收这条：脚本输出全收，gpui-shell 自己的只收警告和错误。
fn wanted(metadata: &Metadata<'_>) -> bool {
    let target = metadata.target();
    if target == SCRIPT_TARGET {
        return true;
    }
    let from_shell = target == SHELL_TARGET || target.starts_with("gpui_shell::");
    from_shell && *metadata.level() <= Level::WARN
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

impl<S, W> Layer<S> for PluginLog<W>
where
    S: Subscriber,
    W: Fn(&str, &str) + Send + Sync + 'static,
{
    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        wanted(metadata)
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        if !wanted(metadata) {
            return;
        }
        let mut message = MessageText::default();
        event.record(&mut message);
        // gpui-shell 自己的事件标上 target，和插件的 console 输出分得开。
        let text = if metadata.target() == SCRIPT_TARGET {
            message.0
        } else {
            format!("{}: {}", metadata.target(), message.0)
        };
        (self.write)(level_name(metadata.level()), &text);
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

    fn capture(body: impl FnOnce()) -> Vec<(String, String)> {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let subscriber = tracing_subscriber::registry().with(PluginLog::new(move |level, message| {
            sink.lock()
                .unwrap()
                .push((level.to_string(), message.to_string()));
        }));
        tracing::subscriber::with_default(subscriber, body);
        let lines = lines.lock().unwrap().clone();
        lines
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
                ("debug".to_string(), "starting".to_string()),
                ("info".to_string(), "[forge] 2 sessions".to_string()),
                ("error".to_string(), "boom".to_string()),
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
                (
                    "error".to_string(),
                    "gpui_shell::engine::quickjs::scheduler: unhandled rejection".to_string()
                ),
                ("warn".to_string(), "gpui_shell: careful".to_string()),
            ]
        );
    }

    #[test]
    fn other_crates_are_left_alone() {
        let lines = capture(|| {
            tracing::error!(target: "reqwest::connect", "nope");
            tracing::error!(target: "gpui_shellfish", "not ours either");
        });
        assert!(lines.is_empty());
    }
}
