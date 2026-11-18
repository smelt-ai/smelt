//! `smelt app …`：调本机开着的 Smelt App 里登记的命令（开视图、开页面等）。
//!
//! 运行时把请求转给 App，App 只执行插件明确开给命令行的命令（`commands` 列出来的那些），
//! 等它做完把结果交回。App 没开时报错，不替人去开。

use crate::{control, control_within, id_arg, respond, usage};
use rust_i18n::t;
use smelt_model::control_api::{AppCommandList, AppCommandRun, AppCommandRunParams};
use smelt_runtime_client::protocol::control::EmptyParams;
use std::time::Duration;

/// 运行时最多等 App 30 秒，这边多留几秒收它的结果或「结果未知」。
const RUN_TIMEOUT: Duration = Duration::from_secs(35);

pub(crate) fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("commands") => match rest {
            [] => respond(control::<AppCommandList>(EmptyParams::default())),
            [other, ..] => usage(&t!("extra_argument", arg = other)),
        },
        Some("run") => {
            let Some(command) = id_arg(rest) else {
                return usage(&t!("app.run_usage"));
            };
            match parse_args(&rest[1..]) {
                Ok(args) => respond(
                    control_within::<AppCommandRun>(
                        AppCommandRunParams {
                            command: command.to_string(),
                            args,
                        },
                        RUN_TIMEOUT,
                    )
                    .map(|run| run.result),
                ),
                Err(message) => usage(&message),
            }
        }
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
        None => usage("smelt app commands|run"),
    }
}

/// `--args '<JSON>'`，不给是 `null`（不带参数）。
fn parse_args(args: &[String]) -> Result<serde_json::Value, String> {
    match args {
        [] => Ok(serde_json::Value::Null),
        [flag, json] if flag == "--args" => serde_json::from_str(json)
            .map_err(|error| t!("app.bad_args", error = error.to_string()).into_owned()),
        [flag] if flag == "--args" => Err(t!("app.run_usage").into_owned()),
        [other, ..] => Err(t!("unknown_argument", arg = other).into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn arguments_are_one_json_value() {
        assert_eq!(parse_args(&[]).unwrap(), serde_json::Value::Null);
        assert_eq!(
            parse_args(&args(&["--args", r#"{"view":"hello/main"}"#])).unwrap(),
            serde_json::json!({ "view": "hello/main" })
        );
        assert!(parse_args(&args(&["--args"])).is_err());
        assert!(parse_args(&args(&["--args", "{"])).is_err());
        assert!(parse_args(&args(&["--bogus", "1"])).is_err());
    }
}
