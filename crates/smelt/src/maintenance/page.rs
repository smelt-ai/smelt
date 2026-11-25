//! 设置「系统 → 命令行」（macOS）：把 `smelt` 装进 `/usr/local/bin`，终端里直接用。

use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Window,
};
use rust_i18n::t;
use smelt_plugin_api::{
    self as api, PluginView, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec, text,
};
use smelt_ui::{Button, IconName, SettingsPage, SettingsRow, SettingsSection};

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
        build: Rc::new(|_, cx| Rc::new(cx.new(|_| CommandPage::default())) as Rc<dyn ViewHandle>),
    }
}

#[derive(Default)]
struct CommandPage {
    system: SystemCommand,
    installing: bool,
}

impl CommandPage {
    fn check(cx: &mut Context<Self>) {
        let check = cx
            .background_executor()
            .spawn(async { cli_command::system_command() });
        cx.spawn(async move |this, cx| {
            let system = check.await;
            let _ = this.update(cx, |page, cx| {
                page.system = system;
                cx.notify();
            });
        })
        .detach();
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        if self.installing {
            return;
        }
        self.installing = true;
        cx.notify();
        let install = cx.background_executor().spawn(async {
            let result = cli_command::install();
            (result, cli_command::system_command())
        });
        cx.spawn(async move |this, cx| {
            let (result, system) = install.await;
            let _ = this.update(cx, |page, cx| {
                page.system = system;
                page.installing = false;
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
                cx.notify();
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

impl PluginView for CommandPage {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, cx: &mut Context<Self>) {
        Self::check(cx);
    }
}

impl Render for CommandPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Self { system, installing } = *self;
        let description: SharedString = match system {
            SystemCommand::Unknown => t!("maintenance.cli.status.unknown").into(),
            SystemCommand::Installed => {
                t!("maintenance.cli.status.installed", link = SYSTEM_LINK).into()
            }
            SystemCommand::Missing => t!("maintenance.cli.status.missing").into(),
            SystemCommand::Stale => t!("maintenance.cli.status.stale", link = SYSTEM_LINK).into(),
            SystemCommand::Taken => t!("maintenance.cli.status.taken", link = SYSTEM_LINK).into(),
        };
        let label = if installing {
            t!("maintenance.cli.install.running")
        } else if system == SystemCommand::Installed {
            t!("maintenance.cli.install.again")
        } else {
            t!("maintenance.cli.install.action")
        };
        let disabled = installing || system == SystemCommand::Taken;
        let page = cx.entity();
        let row = SettingsRow::new(t!("maintenance.cli.row"))
            .description(description)
            .child(
                Button::new("install-cli-command")
                    .small()
                    .label(label)
                    .loading(installing)
                    .disabled(disabled)
                    .on_click(move |_, _, cx: &mut App| {
                        page.update(cx, |page, cx| page.install(cx));
                    }),
            );
        SettingsPage::new("maintenance-cli").child(SettingsSection::new().child(row))
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
