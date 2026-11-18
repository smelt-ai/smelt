//! 各家 agent 引擎本身：命令行工具装没装（`smelt engine`）、给它们装的提醒钩子
//! （`smelt hooks`）、账户用量（`smelt quota`）。都在运行时那台机器上查和改。

use crate::{control, control_within, id_arg, respond, usage};
use rust_i18n::t;
use smelt_model::control_api::{
    AgentCliInspect, AgentCliInstall, AgentCliInstallParams, AgentHooksInstall, AgentHooksStatus,
    AgentHooksUninstall, QuotaGet, QuotaRefresh, QuotaSetEnabled, QuotaSettingsGet,
};
use smelt_model::quota_service::{QuotaRefreshParams, QuotaSetEnabledParams, QuotaTarget};
use smelt_runtime_client::protocol::control::EmptyParams;
use std::time::Duration;

/// 包管理器装一个命令行工具，和桌面设置页给的一样长。
const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// 查一轮用量是每家一次网络往返，和桌面一样等两分钟。
const REFRESH_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => respond(control::<AgentCliInspect>(EmptyParams::default())),
        Some("install") => match id_arg(rest) {
            Some(agent) => respond(control_within::<AgentCliInstall>(
                AgentCliInstallParams {
                    agent: agent.to_string(),
                },
                INSTALL_TIMEOUT,
            )),
            None => usage("smelt engine install <id>"),
        },
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
        None => usage("smelt engine list|install"),
    }
}

pub(crate) fn run_hooks(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("status") => respond(control::<AgentHooksStatus>(EmptyParams::default())),
        Some("install") => respond(control::<AgentHooksInstall>(EmptyParams::default())),
        Some("uninstall") => respond(control::<AgentHooksUninstall>(EmptyParams::default())),
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
        None => usage("smelt hooks status|install|uninstall"),
    }
}

pub(crate) fn run_quota(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        None => respond(control::<QuotaGet>(EmptyParams::default())),
        Some("refresh") => {
            let target = match rest.first() {
                None => None,
                Some(id) => match target(id) {
                    Ok(target) => Some(target),
                    Err(message) => return usage(&message),
                },
            };
            respond(control_within::<QuotaRefresh>(
                QuotaRefreshParams { target },
                REFRESH_TIMEOUT,
            ))
        }
        Some("settings") => respond(control::<QuotaSettingsGet>(EmptyParams::default())),
        Some(verb @ ("enable" | "disable")) => match id_arg(rest).map(target) {
            Some(Ok(target)) => respond(control::<QuotaSetEnabled>(QuotaSetEnabledParams {
                target,
                enabled: verb == "enable",
            })),
            Some(Err(message)) => usage(&message),
            None => usage(&format!("smelt quota {verb} <id>")),
        },
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
    }
}

/// 账户 id：`claude`、`codex` 这样的 agent id，Pi 底下的是 `pi:<provider>`。
fn target(id: &str) -> Result<QuotaTarget, String> {
    QuotaTarget::from_id(id).ok_or_else(|| t!("quota.unknown_target", id = id).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_targets_use_the_settings_ids() {
        assert_eq!(target("claude").unwrap().id(), "claude");
        assert!(target("nope").is_err());
        assert!(target("claude:anthropic").is_err());
    }
}
