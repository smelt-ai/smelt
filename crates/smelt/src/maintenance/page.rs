//! 设置「系统 → 命令行」（macOS）：把 `smelt` 装进 `/usr/local/bin`，终端里直接用；渲染由 NSH 主壳承担。

use gpui::{App, Context, SharedString};
use rust_i18n::t;
use serde_json::{Value, json};
use smelt_plugin_api::{self as api, SettingsEntry, SettingsGroup, ViewSpec, text};
use smelt_ui::IconName;

use crate::Workspace;

use super::cli_command::{self, InstallError, SYSTEM_LINK, SystemCommand};

pub(crate) fn spec() -> ViewSpec {
    let keywords = [
        "smelt 命令",
        "CLI",
        "终端命令",
        "command line",
        "PATH",
        "/usr/local/bin",
    ];
    ViewSpec {
        name: "cli".into(),
        title: text!("maintenance.cli.title"),
        icon: IconName::Terminal.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::System,
            keywords: keywords.into_iter().map(SharedString::from).collect(),
        }),
        fills: Vec::new(),
        build: crate::host_views::nsh_managed_view_builder(),
    }
}

#[derive(Default)]
pub(crate) struct CliState {
    system: SystemCommand,
    installing: bool,
}

impl CliState {
    pub(crate) fn snapshot(&self) -> Value {
        let description = match self.system {
            SystemCommand::Unknown => t!("maintenance.cli.status.unknown").into_owned(),
            SystemCommand::Installed => {
                t!("maintenance.cli.status.installed", link = SYSTEM_LINK).into_owned()
            }
            SystemCommand::Missing => t!("maintenance.cli.status.missing").into_owned(),
            SystemCommand::Stale => {
                t!("maintenance.cli.status.stale", link = SYSTEM_LINK).into_owned()
            }
            SystemCommand::Taken => {
                t!("maintenance.cli.status.taken", link = SYSTEM_LINK).into_owned()
            }
        };
        let button_label = if self.installing {
            t!("maintenance.cli.install.running").into_owned()
        } else if self.system == SystemCommand::Installed {
            t!("maintenance.cli.install.again").into_owned()
        } else {
            t!("maintenance.cli.install.action").into_owned()
        };
        let disabled = self.installing || self.system == SystemCommand::Taken;
        json!({
            "kind": "cli",
            "row_title": t!("maintenance.cli.row").into_owned(),
            "description": description,
            "button_label": button_label,
            "installing": self.installing,
            "disabled": disabled,
        })
    }

    pub(crate) fn check(
        mut accessor: impl FnMut(&mut Workspace) -> &mut Self + 'static,
        cx: &mut Context<Workspace>,
    ) {
        let check = cx
            .background_executor()
            .spawn(async { cli_command::system_command() });
        cx.spawn(async move |this, cx| {
            let system = check.await;
            let _ = this.update(cx, |workspace, cx| {
                accessor(workspace).system = system;
                workspace.sync_host_settings(cx);
            });
        })
        .detach();
    }

    pub(crate) fn install(
        &mut self,
        mut accessor: impl FnMut(&mut Workspace) -> &mut Self + 'static,
        cx: &mut Context<Workspace>,
    ) {
        if self.installing {
            return;
        }
        self.installing = true;
        let install = cx.background_executor().spawn(async {
            let result = cli_command::install();
            (result, cli_command::system_command())
        });
        cx.spawn(async move |this, cx| {
            let (result, system) = install.await;
            let _ = this.update(cx, |workspace, cx| {
                let state = accessor(workspace);
                state.system = system;
                state.installing = false;
                match result {
                    Ok(()) => notify(
                        &t!("maintenance.notify.done"),
                        &t!("maintenance.cli.install.done"),
                        cx,
                    ),
                    Err(error) => {
                        eprintln!("[maintenance] 装 smelt 命令失败：{error:?}");
                        let reason = format_install_error(&error);
                        notify(
                            &t!("maintenance.notify.error"),
                            &t!("maintenance.cli.install.failed", error = reason),
                            cx,
                        );
                    }
                }
                workspace.sync_host_settings(cx);
            });
        })
        .detach();
    }
}

fn format_install_error(error: &InstallError) -> String {
    match error {
        InstallError::Taken => t!("maintenance.cli.status.taken", link = SYSTEM_LINK).into_owned(),
        InstallError::Cancelled => t!("maintenance.cli.install.cancelled").into_owned(),
        InstallError::NoHome => t!("maintenance.cli.install.no_home").into_owned(),
        InstallError::LinkFailed(err) => {
            t!("maintenance.cli.install.link_failed", error = err).into_owned()
        }
        InstallError::OsascriptFailed(err) => {
            t!("maintenance.cli.install.osascript_failed", error = err).into_owned()
        }
        InstallError::Failed(err) => err.clone(),
    }
}

fn notify(subtitle: &str, body: &str, cx: &mut App) {
    let notify = api::Notify {
        subtitle: subtitle.to_string(),
        body: body.to_string(),
        ..Default::default()
    };
    api::NOTIFY.run(&notify, cx).detach();
}
