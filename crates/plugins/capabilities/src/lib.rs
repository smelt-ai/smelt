//! 能力库：给某台机器上的 Pi 配模型与凭据、MCP 服务器、技能与扩展、记忆召回。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道，宿主里没有它的类型。视图 id 是
//! `smelt.capabilities/main`，打开时的参数：`{"tab": "models" | "plugins" | "mcp" | "memory",
//! "machine": …}`（机器的写法见 [`api::machine_to_json`]），都可以不给。每台机器的菜单里也
//! 登记一项「能力库」（[`MACHINE_MENU`]），点了打开时带上那台机器。
//!
//! 状态全在 [`CapabilitiesPage`] 上：在配哪台机器、那台机器的 Pi 配置快照、各表单。
//! 页面一直留着，离开再回来表单还在；每次打开重读快照。

use std::rc::Rc;

use api::contracts::sessions::{MACHINE_MENU, MachineMenuContext};
use api::model::daemon_state::CORE_TOPIC_PI_CONFIG_CHANGED;
use api::serde_json;
use api::ui::{Button, Segmented};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings};
use gpui_component::*;
use smelt_plugin_api::{
    self as api, ContextMenuAction, ContextMenuItem, HostEvent, PluginView, RuntimeKey,
    TopicMessage, ViewHandle, ViewSpec,
};

mod auth;
mod config;
mod mcp;
mod memory;
mod models;
mod render;
mod state;

use config::PiConfigSlot;
use state::*;

const OWNER: &str = "smelt.capabilities";

/// 机器菜单里那一项点了执行的命令：打开能力库，配右键的那台机器。
const OPEN_ON_MACHINE: api::Command<MachineMenuContext, ()> =
    api::Command::new("smelt.capabilities/open-on-machine");

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    api::register_views(
        OWNER,
        vec![ViewSpec {
            name: "main".into(),
            title: "能力库".into(),
            icon: IconName::LayoutDashboard.path(),
            draws_top_bar: false,
            instances: false,
            settings: None,
            build: Rc::new(|_window, cx| {
                Rc::new(cx.new(CapabilitiesPage::new)) as Rc<dyn ViewHandle>
            }),
        }],
        cx,
    )
    .expect("能力库的视图表写错了");
    api::register_commands(
        OWNER,
        "能力库",
        vec![
            OPEN_ON_MACHINE.spec_async("在这台机器上打开能力库", |args, _, cx| {
                open_on_machine(args, cx)
            }),
        ],
        cx,
    )
    .expect("能力库的命令表写错了");
    let item = ContextMenuItem {
        label: "能力库".into(),
        image: None,
        icon: Some(IconName::LayoutDashboard.path()),
        machine: None,
        action: ContextMenuAction::Run {
            command: OPEN_ON_MACHINE.id().to_string(),
            args: serde_json::Value::Null,
        },
    };
    api::register_context_menu_items(OWNER, MACHINE_MENU, vec![item], cx)
        .expect("能力库的菜单项写错了");
}

fn open_on_machine(args: MachineMenuContext, cx: &mut App) -> Task<Result<(), String>> {
    let opened = api::OPEN_VIEW.run(
        &api::OpenView {
            view: api::contracts::capabilities::VIEW.to_string(),
            args: serde_json::json!({ "machine": api::machine_to_json(&args.machine) }),
        },
        cx,
    );
    cx.spawn(async move |_| match opened.await? {
        true => Ok(()),
        false => Err("没有能放能力库的地方".to_string()),
    })
}

/// 页面里平级的几个页签。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CapabilitiesTab {
    #[default]
    Models,
    Plugins,
    Mcp,
    Memory,
}

impl CapabilitiesTab {
    const ALL: [Self; 4] = [Self::Models, Self::Plugins, Self::Mcp, Self::Memory];

    fn label(self) -> &'static str {
        match self {
            Self::Models => "模型",
            Self::Plugins => "插件",
            Self::Mcp => "MCP",
            Self::Memory => "记忆",
        }
    }

    /// 打开参数里的名字，也拿来拼元素 id。
    fn key(self) -> &'static str {
        match self {
            Self::Models => "models",
            Self::Plugins => "plugins",
            Self::Mcp => "mcp",
            Self::Memory => "memory",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.key() == key)
    }
}

pub struct CapabilitiesPage {
    /// 在配哪台机器。
    machine: RuntimeKey,
    /// 换一次机器加一：换之前发出去、还没回来的调用作废，免得上一台的结果落到这一台。
    epoch: u64,
    tab: CapabilitiesTab,
    /// 那台机器的 Pi 配置快照。
    pi_config: PiConfigSlot,
    /// 原生 Pi 模型设置表单。
    pi_model_editor: Option<PiModelEditor>,
    /// 原生 Pi 自定义 Provider 表单。
    pi_custom_provider_editor: Option<PiCustomProviderEditor>,
    /// Pi 内置 provider 的凭据现状；进模型页签时才异步填。
    pi_auth_providers: Option<PiAuthProvidersState>,
    /// 正在进行的登录：provider 的订阅/API key，或 MCP 服务器的 OAuth。
    pi_login: Option<PiLoginView>,
    /// 正在为哪个 provider 挑默认模型。
    pi_auth_model_picker: Option<PiAuthModelPicker>,
    /// Pi 凭据操作（列表、注销）的最近一次错误。
    pi_auth_error: Option<String>,
    /// 凭据面板是否展开到全部内置 provider（默认只列可订阅登录的和已配好的）。
    pi_auth_show_all: bool,
    /// 等用户确认移除的 MCP 服务器名。移除会删掉用户手写的配置，所以要两步。
    pi_mcp_pending_remove: Option<String>,
    /// MCP 配置操作（添加、启停、移除、登录）的最近一次错误。
    pi_mcp_error: Option<String>,
    /// 交互式原生 Pi 模型表单错误。
    pi_model_editor_error: Option<String>,
    /// 正在等二次确认删除的插件 id。
    pi_plugin_pending_delete: Option<String>,
    /// 插件导入 / 删除的最近一次错误。
    pi_plugin_error: Option<String>,
    /// 那台机器装了远程桌面（运行时 `system.describe` 报的）：浏览器类 MCP 预设在那里接
    /// 远程桌面里的 Chrome。进页面、换机器、重连时问一次，还没问到当没装。
    remote_desktop: bool,
    _host_events: Subscription,
}

impl CapabilitiesPage {
    fn new(cx: &mut Context<Self>) -> Self {
        let events = api::events(cx);
        let host_events = cx.subscribe(&events, |this, _, event: &HostEvent, cx| {
            this.on_host_event(event, cx);
        });
        Self {
            machine: RuntimeKey::Local,
            epoch: 0,
            tab: CapabilitiesTab::default(),
            pi_config: PiConfigSlot::default(),
            pi_model_editor: None,
            pi_custom_provider_editor: None,
            pi_auth_providers: None,
            pi_login: None,
            pi_auth_model_picker: None,
            pi_auth_error: None,
            pi_auth_show_all: false,
            pi_mcp_pending_remove: None,
            pi_mcp_error: None,
            pi_model_editor_error: None,
            pi_plugin_pending_delete: None,
            pi_plugin_error: None,
            remote_desktop: false,
            _host_events: host_events,
        }
    }

    fn on_host_event(&mut self, event: &HostEvent, cx: &mut Context<Self>) {
        match event {
            HostEvent::RuntimeTopic {
                machine,
                topic,
                message,
            } if *machine == self.machine
                && topic == CORE_TOPIC_PI_CONFIG_CHANGED
                && !matches!(message, TopicMessage::Unavailable(_)) =>
            {
                self.on_pi_config_changed(cx);
            }
            HostEvent::Reconnected(machine) if *machine == self.machine => {
                self.on_pi_config_changed(cx);
                // 重连可能是运行时重装、升级过，装没装远程桌面也可能变了。
                self.refresh_remote_desktop(cx);
            }
            HostEvent::RuntimeTopic { .. }
            | HostEvent::Reconnected(_)
            | HostEvent::ProjectChanged { .. }
            | HostEvent::WindowActivated
            | HostEvent::OverlayChanged
            | HostEvent::SessionsChanged
            | HostEvent::NotificationPermissionChanged => {}
            HostEvent::MachinesChanged => {
                // 在配的那台远程机器已经移除时，回到本机。
                if api::host(cx).client(&self.machine, cx).is_none() {
                    self.select_pi_library_runtime(RuntimeKey::Local, cx);
                }
                cx.notify();
            }
            // 插件页签上「谁在用」从这里算。
            HostEvent::AgentSettingsChanged => cx.notify(),
            HostEvent::ActiveContextChanged
            | HostEvent::ShownViewsChanged
            | HostEvent::ViewsChanged
            | HostEvent::ViewBadgesChanged
            | HostEvent::ThemesChanged
            | HostEvent::LayoutsChanged
            | HostEvent::PagesChanged
            | HostEvent::CommandsChanged
            | HostEvent::Plugin { .. }
            | HostEvent::OpenPaths { .. } => {}
        }
    }

    /// 正在配的那台机器的 Pi 配置变了，或者重新连上（断开期间可能错过推送）：重读。
    fn on_pi_config_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_pi_library(cx);
    }

    /// 重读快照和当前页签要的东西：进页面、换机器时。
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.refresh_pi_library(cx);
        self.refresh_remote_desktop(cx);
        self.load_tab(cx);
    }

    /// 页签露出来时要读的：模型页签一打开就读内置 Provider 的登录状态，不用再点「查看」
    /// （失败后停在 Failed，由用户点重试）。
    fn load_tab(&mut self, cx: &mut Context<Self>) {
        if self.tab == CapabilitiesTab::Models && self.pi_auth_providers.is_none() {
            self.refresh_pi_auth_providers(cx);
        }
    }

    fn select_tab(&mut self, tab: CapabilitiesTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.load_tab(cx);
            cx.notify();
        }
    }

    /// 在配哪台机器：一个按钮，点开选本机或别的机器。离线的照样能选，选了会说读不到。
    fn render_machine_picker(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let machines = api::host(cx).machines(cx);
        let current = machines
            .iter()
            .find(|machine| machine.key == self.machine)
            .map(|machine| machine.name.clone())
            .unwrap_or_default();
        let selected = self.machine.clone();
        let entity = cx.entity();
        Button::new("capabilities-runtime")
            .quiet()
            .label(format!("机器：{current}"))
            .icon(IconName::ChevronDown)
            .dropdown_menu(move |menu, _window, _cx| {
                machines.iter().fold(menu, |menu, machine| {
                    let label = match &machine.unavailable {
                        Some(reason) => format!("{}（{reason}）", machine.name),
                        None => machine.name.clone(),
                    };
                    let key = machine.key.clone();
                    let entity = entity.clone();
                    menu.item(
                        PopupMenuItem::new(label)
                            .checked(machine.key == selected)
                            .on_click(move |_, _window, cx| {
                                let key = key.clone();
                                entity
                                    .update(cx, |page, cx| page.select_pi_library_runtime(key, cx));
                            }),
                    )
                })
            })
    }
}

impl PluginView for CapabilitiesPage {
    fn open(&mut self, args: &serde_json::Value, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = args
            .get("tab")
            .and_then(serde_json::Value::as_str)
            .and_then(CapabilitiesTab::from_key)
        {
            self.tab = tab;
        }
        let mut machine = args
            .get("machine")
            .and_then(api::machine_from_json)
            .unwrap_or_else(|| self.machine.clone());
        if api::host(cx).client(&machine, cx).is_none() {
            machine = RuntimeKey::Local;
        }
        // 每次进来都重读：绕过运行时的修改（手改配置文件）收不到推送。
        if machine == self.machine {
            self.reload(cx);
        } else {
            self.select_pi_library_runtime(machine, cx);
        }
        cx.notify();
    }
}

impl Render for CapabilitiesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = api::host(cx);
        let entity = cx.entity();
        let tabs = Segmented::new("capabilities-tabs")
            .segments(CapabilitiesTab::ALL.map(|tab| tab.label()))
            .selected(CapabilitiesTab::ALL.iter().position(|tab| *tab == self.tab))
            .on_select(move |ix, _, cx| {
                if let Some(tab) = CapabilitiesTab::ALL.get(ix).copied() {
                    entity.update(cx, |page, cx| page.select_tab(tab, cx));
                }
            });
        // 挂了别的机器，才需要选在配哪台。
        let several_machines = host.machines(cx).len() > 1;
        // 页名在外壳画的标题带（或标签）上，这一行不再写一遍。
        let header = api::ui::page_toolbar()
            .gap_3()
            .child(tabs)
            .when(several_machines, |header| {
                header
                    .child(div().flex_1())
                    .child(self.render_machine_picker(cx))
            });

        v_flex().size_full().child(header).child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(render::content(self, cx)),
        )
    }
}

fn notify_success(message: impl AsRef<str>, cx: &mut App) {
    notify("完成", message.as_ref(), cx);
}

fn notify_error(message: impl AsRef<str>, cx: &mut App) {
    notify("错误", message.as_ref(), cx);
}

fn notify(subtitle: &str, body: &str, cx: &mut App) {
    let notify = api::Notify {
        subtitle: subtitle.to_string(),
        body: body.to_string(),
        ..Default::default()
    };
    api::NOTIFY.run(&notify, cx).detach();
}
