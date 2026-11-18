//! 命令行转来的 App 命令（`smelt app commands`、`smelt app run`）：运行时把请求单播给本机
//! 桌面，这里查命令注册表、执行、把结果回执给运行时。
//!
//! 只转开给命令行的命令（[`smelt_plugin_api::CommandSpec::callable_from_cli`]）；宿主不认识
//! 具体命令，插件登记什么就列什么。

use gpui::App;
use smelt_event_protocol::{
    AppCommandOutcome, AppCommandReceipt, AppCommandRequest, AppCommandRequested,
};
use smelt_model::control_api::AppCommandInfo;

/// 一直收请求，处理完逐个回执。接收端断了（订阅线程都停了）就结束。
pub(crate) fn serve(requests: smol::channel::Receiver<AppCommandRequested>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Ok(AppCommandRequested {
            request_id,
            request,
        }) = requests.recv().await
        {
            let outcome = match request {
                AppCommandRequest::List => cx.update(|cx| AppCommandOutcome::Done {
                    value: serde_json::to_value(listed(cx)).unwrap_or_default(),
                }),
                AppCommandRequest::Run { command, args } => {
                    let task = cx.update(|cx| run(&command, args, cx));
                    // 一条命令可能要等一会儿（读文件、开视图）；别的请求不必排在它后面。
                    cx.spawn(async move |cx| {
                        let outcome = match task.await {
                            Ok(value) => AppCommandOutcome::Done { value },
                            Err(message) => AppCommandOutcome::Failed { message },
                        };
                        send_receipt(request_id, outcome, cx);
                    })
                    .detach();
                    continue;
                }
            };
            send_receipt(request_id, outcome, cx);
        }
    })
    .detach();
}

/// 开给命令行的命令，按 id 排。
fn listed(cx: &App) -> Vec<AppCommandInfo> {
    let mut commands: Vec<_> = smelt_plugin_api::commands(cx)
        .into_iter()
        .filter(|command| command.cli)
        .map(|command| AppCommandInfo {
            id: command.id.to_string(),
            title: command.title.get().to_string(),
            group: command.group.get().to_string(),
            args: command.args.unwrap_or_default(),
        })
        .collect();
    commands.sort_by(|a, b| a.id.cmp(&b.id));
    commands
}

/// 执行一条开给命令行的命令。没开给命令行的和没登记的一样说「没有」：命令行看不到它。
fn run(
    id: &str,
    args: serde_json::Value,
    cx: &mut App,
) -> gpui::Task<smelt_plugin_api::CommandResult> {
    if !smelt_plugin_api::command(id, cx).is_some_and(|command| command.cli) {
        return gpui::Task::ready(Err(
            rust_i18n::t!("app_command.missing", id = id).into_owned()
        ));
    }
    smelt_plugin_api::run(id, args, cx)
}

fn send_receipt(request_id: String, outcome: AppCommandOutcome, cx: &gpui::AsyncApp) {
    let receipt = AppCommandReceipt {
        request_id,
        outcome,
    };
    cx.background_executor()
        .spawn(async move {
            let request = serde_json::json!({
                "op": smelt_runtime_client::protocol::daemon::DaemonOperation::AppCommandReceipt,
                "receipt": receipt,
            });
            if let Err(error) = smelt_client::daemon_state::local_runtime()
                .request_ok(&request, std::time::Duration::from_secs(4))
            {
                eprintln!("[app-command] 发送命令回执失败：{error}");
            }
        })
        .detach();
}
