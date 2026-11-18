//! 设置「系统 → 命令行」：装 `/usr/local/bin/smelt`。只有 macOS 有（GUI 自己就是命令行）。

use std::rc::Rc;

use api::ui::{Button, SettingsPage, SettingsRow, SettingsSection};
use api::{PluginView, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec};
use gpui::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Window,
};
use rust_i18n::t;
use smelt_plugin_api::{self as api, text};

use crate::cli_command::{self, SYSTEM_LINK, SystemCommand};

pub(crate) fn register(cx: &mut App) {
    let keywords = [
        "smelt 命令",
        "CLI",
        "终端命令",
        "command line",
        "PATH",
        "/usr/local/bin",
    ];
    let spec = ViewSpec {
        name: "cli".into(),
        title: text!("page.title"),
        icon: api::ui::IconName::Terminal.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::System,
            keywords: keywords.into_iter().map(SharedString::from).collect(),
        }),
        fills: Vec::new(),
        build: Rc::new(|_, cx| Rc::new(cx.new(|_| CommandPage::default())) as Rc<dyn ViewHandle>),
    };
    if let Err(error) = api::register_views(crate::OWNER, vec![spec], cx) {
        eprintln!("[maintenance] 设置页登记失败：{error}");
    }
}

/// `/usr/local/bin/smelt` 的状态：页建好时在后台查一次，装完再查一次。
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

    /// 要等授权框，放后台；结束后重新查一次状态。
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
                    Ok(()) => notify(&t!("notify.done"), &t!("install.done"), cx),
                    Err(error) => {
                        eprintln!("[maintenance] 装 smelt 命令失败：{error}");
                        notify(
                            &t!("notify.error"),
                            &t!("install.failed", error = error),
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

impl PluginView for CommandPage {
    /// 建好后打开：在后台查一次 `/usr/local/bin/smelt`。
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, cx: &mut Context<Self>) {
        Self::check(cx);
    }
}

impl Render for CommandPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Self { system, installing } = *self;
        let description: SharedString = match system {
            SystemCommand::Unknown => t!("status.unknown").into(),
            SystemCommand::Installed => t!("status.installed", link = SYSTEM_LINK).into(),
            SystemCommand::Missing => t!("status.missing").into(),
            SystemCommand::Stale => t!("status.stale", link = SYSTEM_LINK).into(),
            SystemCommand::Taken => t!("status.taken", link = SYSTEM_LINK).into(),
        };
        let label = if installing {
            t!("install.running")
        } else if system == SystemCommand::Installed {
            t!("install.again")
        } else {
            t!("install.action")
        };
        let disabled = installing || system == SystemCommand::Taken;
        let page = cx.entity();
        let row = SettingsRow::new(t!("row.title"))
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
