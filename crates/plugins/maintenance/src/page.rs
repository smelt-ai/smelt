//! 设置「系统 → 命令行」：装 `/usr/local/bin/smelt`。只有 macOS 有（GUI 自己就是命令行）。

use std::rc::Rc;

use api::ui::Button;
use api::{PluginView, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec};
use gpui::{App, AppContext as _, Context, IntoElement, Render, SharedString, Window};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use smelt_plugin_api as api;

use crate::cli_command::{self, SYSTEM_LINK, SystemCommand};

pub(crate) fn register(cx: &mut App) {
    let keywords = ["smelt 命令", "CLI", "终端命令", "PATH", "/usr/local/bin"];
    let spec = ViewSpec {
        name: "cli".into(),
        title: "命令行".into(),
        icon: gpui_kit_assets::IconName::SquareTerminal.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::System,
            keywords: keywords.into_iter().map(SharedString::from).collect(),
        }),
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
                    Ok(()) => notify("完成", "已装好 smelt 命令，新开的终端里就能用", cx),
                    Err(error) => {
                        eprintln!("[maintenance] 装 smelt 命令失败：{error}");
                        notify("错误", &format!("装 smelt 命令失败：{error}"), cx);
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
            SystemCommand::Unknown => "还没检查".into(),
            SystemCommand::Installed => format!("已装好：{SYSTEM_LINK} 指向这个 App").into(),
            SystemCommand::Missing => "在 /usr/local/bin 放一个指向这个 App 的 smelt，终端里就能直接敲 smelt agent list。一般要输一次管理员密码".into(),
            SystemCommand::Stale => {
                format!("{SYSTEM_LINK} 指向的不是这个 App，重新装一次就换过来").into()
            }
            SystemCommand::Taken => format!("{SYSTEM_LINK} 已经是别的程序，没有动它").into(),
        };
        let label = match system {
            SystemCommand::Installed => "重新安装",
            _ => "安装",
        };
        let disabled = installing || system == SystemCommand::Taken;
        let page = cx.entity();
        let group = SettingGroup::new().item(
            SettingItem::new(
                "终端里的 smelt 命令",
                SettingField::render(move |_, _, _| {
                    let page = page.clone();
                    Button::new("install-cli-command")
                        .small()
                        .label(if installing { "安装中…" } else { label })
                        .loading(installing)
                        .disabled(disabled)
                        .on_click(move |_, _, cx: &mut App| {
                            page.update(cx, |page, cx| page.install(cx));
                        })
                }),
            )
            .description(description),
        );
        api::settings_pane(
            "maintenance-cli",
            SettingPage::new("命令行").resettable(false).group(group),
        )
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
