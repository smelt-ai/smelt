//! 设置里的「Agent」页：本机 Agent CLI 装没装、Agent hooks、自定义 Agent、项目「+」菜单的
//! 启动项、账户额度的查询开关。
//!
//! 数据都在本机运行时：CLI 探测和安装、hooks 的状态和增删、启动项清单、额度开关都经控制
//! 调用；自定义 Agent 和 hooks 开关在智能体配置里（[`agent_hosts`]）。页面每次露出来时重读
//! 只在这里显示、平时没人盯着的那几样（hooks、启动项、额度开关；CLI 只在还没探测过时探），
//! 结果放在页面上，render 只读它。

use std::rc::Rc;
use std::time::Instant;

use api::client::protocol::control::EmptyParams;
use api::model::acp_conn::{AcpRuntimeDiagnostics, RuntimeExecutable};
use api::model::agent_hooks::AgentHooksStatus;
use api::model::agent_kind::{
    AcpProfile, ConversationAgentKind, format_env_lines, parse_env_lines,
};
use api::model::control_api::{
    AgentCliInspect, AgentCliInstall, AgentCliInstallParams, AgentHooksInstall,
    AgentHooksUninstall, LaunchGet, LaunchReplace, LaunchReplaceParams,
};
use api::model::font_config;
use api::model::launch_settings::launch_entry_is_builtin;
use api::model::new_session::LaunchEntry;
use api::model::quota_service::QuotaProviderSetting;
use api::ui::menu::{Menu, MenuItem, MenuTrigger};
use api::ui::truncated_line::truncated_line;
use api::ui::{
    Button, IconAction as _, IconName, SettingsPage, SettingsRow, SettingsSection, Switch,
    TextArea, TextInput, ui_theme,
};
use api::{HostEvent, PluginView, RuntimeKey, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, Hsla, SharedString, Subscription, Task, Window, div, px, rgb,
};
use rust_i18n::t;
use smelt_plugin_api as api;
use smelt_plugin_api::ui::field::{InputEvent, InputState, TextareaState};

use crate::agent_hosts;
use crate::contract;
use crate::model::save_on_input_event;
use crate::quota;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

/// 装 CLI 要等包管理器下载，比普通调用久得多。
const CLI_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// 设置导航里这一页的视图。
pub(crate) fn view_spec() -> ViewSpec {
    ViewSpec {
        // 视图 id 是 `smelt.agents/settings`（[`contract::SETTINGS_VIEW`]）。
        name: "settings".into(),
        title: api::text!("settings.title"),
        icon: IconName::Bot.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::Agents,
            keywords: [
                "智能体",
                "运行环境",
                "cli",
                "安装",
                "版本",
                "检测",
                "路径",
                "claude",
                "codex",
                "copilot",
                "grok",
                "cursor",
                "opencode",
                "kiro",
                "pi",
                "hook",
                "hooks",
                "Agent Hooks",
                "通知接入",
                "配置文件",
                "自定义 agent",
                "acp",
                "适配器",
                "环境变量",
                "数据目录",
                "config dir",
                "启动命令",
                "参数",
                "快捷启动项",
                "启动",
                "launch",
                "命令",
                "+",
                "加号",
                "新建菜单",
                "项目菜单",
                "额度",
                "用量",
                "钥匙串",
                "keychain",
            ]
            .into_iter()
            .map(SharedString::from)
            .collect(),
        }),
        fills: Vec::new(),
        build: Rc::new(|window, cx| {
            Rc::new(cx.new(|cx| AgentSettingsPage::new(window, cx))) as Rc<dyn ViewHandle>
        }),
    }
}

struct AgentSettingsPage {
    /// 上次看到的露没露着：从没露变成露着时重读一遍。
    shown: bool,
    cli: Cli,
    hooks: Hooks,
    launch: Launch,
    /// 本机每个自定义 Agent 一行（配置和输入框），和配置里的顺序一致。配置变了才更新，
    /// render 只借用它画，不复制配置。
    profiles: Rc<Vec<ProfileRow>>,
    /// 本机配置的状态：收到了是 `Ok`；给不了是 `Err(原因)`；还没收到是 `Err(None)`。
    profiles_state: Result<(), Option<String>>,
    _profile_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

/// 本机 Agent CLI 的探测结果。探测只跑 `--version`，不起 agent、不上网。
#[derive(Default)]
struct Cli {
    diagnostics: Option<AcpRuntimeDiagnostics>,
    refreshing: bool,
    checked_at: Option<Instant>,
    installing: Option<ConversationAgentKind>,
    /// 最近一次安装的结果：给人看的话、是不是没装成。
    install_result: Option<(String, bool)>,
}

/// 运行时报的 hooks 接入状态。
#[derive(Default)]
struct Hooks {
    /// `None`：还没取到。
    status: Option<AgentHooksStatus>,
    /// 安装、移除进行中。
    busy: bool,
}

/// 项目「+」菜单的启动项。清单在运行时，这里是读到的那份加上还没发完的修改。
#[derive(Default)]
struct Launch {
    /// `None`：还没读到，读到之前不接受修改。
    entries: Option<Vec<LaunchEntry>>,
    /// 最近一次读的失败原因。
    error: Option<String>,
    rows: Vec<LaunchRow>,
    _subscriptions: Vec<Subscription>,
    /// 等着发的整表替换（发出时页面上的那份、改完的那份）。按顺序一份一份发：每份的底都是
    /// 上一份改完的样子，一起发出去会互相判成冲突。
    pending: std::collections::VecDeque<(Vec<LaunchEntry>, Vec<LaunchEntry>)>,
    sending: bool,
    /// 正在读的那次；换掉或清掉就是不要它的结果了。
    loading: Option<Task<()>>,
}

struct LaunchRow {
    label: Entity<InputState>,
    command: Entity<InputState>,
    /// 内置启动项固定存在，不能删。
    removable: bool,
}

/// 一个自定义 Agent 的输入框。环境变量写不对时不存，错误挂在这里给页面显示。
#[derive(Clone)]
struct ProfileRow {
    /// 配置里的这一项。换了类型（占位字跟着类型走）或换了 id 就整排重建。
    profile: AcpProfile,
    label: Entity<InputState>,
    command: Entity<InputState>,
    env: Entity<TextareaState>,
    dir: Entity<InputState>,
    env_error: Option<String>,
}

impl LaunchRow {
    /// 输入框的占位文字。建的时候和换语言时各设一次。
    fn label_inputs(&self, window: &mut Window, cx: &mut App) {
        self.label.update(cx, |input, cx| {
            input.set_placeholder(t!("launch.label_placeholder"), window, cx)
        });
        self.command.update(cx, |input, cx| {
            input.set_placeholder(t!("launch.command_placeholder"), window, cx)
        });
    }
}

impl ProfileRow {
    /// 输入框的占位文字。建的时候和换语言时各设一次。
    fn label_inputs(&self, window: &mut Window, cx: &mut App) {
        let command = match self.profile.kind() {
            Some(ConversationAgentKind::Custom) | None => t!("profile.command_placeholder"),
            Some(kind) => t!("profile.command_inherit_placeholder", agent = kind.label()),
        };
        self.label.update(cx, |input, cx| {
            input.set_placeholder(t!("profile.label_placeholder"), window, cx)
        });
        self.command
            .update(cx, |input, cx| input.set_placeholder(command, window, cx));
        self.env.update(cx, |input, cx| {
            input.set_placeholder(t!("profile.env_placeholder"), window, cx)
        });
        self.dir.update(cx, |input, cx| {
            input.set_placeholder(t!("profile.dir_placeholder"), window, cx)
        });
    }
}

impl AgentSettingsPage {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe_in(
                &api::events(cx),
                window,
                |this, _, event: &HostEvent, window, cx| match event {
                    HostEvent::ShownViewsChanged => this.sync_shown(window, cx),
                    // 输入框的占位文字存在输入框里，换语言时重设。
                    HostEvent::LocaleChanged => this.relabel_inputs(window, cx),
                    _ => {}
                },
            ),
            cx.observe_global_in::<agent_hosts::AgentHosts>(window, |this, window, cx| {
                this.sync_profile_rows(window, cx);
                cx.notify();
            }),
            cx.observe_global::<quota::QuotaSettings>(|_, cx| cx.notify()),
        ];
        let mut page = Self {
            shown: false,
            cli: Cli::default(),
            hooks: Hooks::default(),
            launch: Launch::default(),
            profiles: Rc::new(Vec::new()),
            profiles_state: Err(None),
            _profile_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        page.sync_profile_rows(window, cx);
        page.sync_shown(window, cx);
        page
    }

    fn relabel_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for row in &self.launch.rows {
            row.label_inputs(window, cx);
        }
        for row in self.profiles.iter() {
            row.label_inputs(window, cx);
        }
    }

    /// 页面露出来时重读只在这里显示的状态：外面手改了 hooks 配置、别的设备改了启动项，重开
    /// 这页就能看到。
    fn sync_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = api::host(cx).view_shown(contract::SETTINGS_VIEW, cx);
        if shown == self.shown {
            return;
        }
        self.shown = shown;
        if !shown {
            return;
        }
        self.refresh_hooks(cx);
        self.load_launch(window, cx);
        quota::refresh_settings(cx);
        if self.cli.diagnostics.is_none() {
            self.refresh_cli(cx);
        }
    }

    // —— 本机 Agent CLI ——

    /// 请运行时探测各家 CLI。连不上时保留上一次的结果。
    fn refresh_cli(&mut self, cx: &mut Context<Self>) {
        if self.cli.refreshing || self.cli.installing.is_some() {
            return;
        }
        self.cli.refreshing = true;
        cx.notify();
        let inspected = inspect_clis(cx);
        cx.spawn(async move |this, cx| {
            let inspected = inspected.await;
            let _ = this.update(cx, |this, cx| {
                match inspected {
                    Ok(diagnostics) => this.cli.diagnostics = Some(diagnostics),
                    Err(error) => eprintln!("[agents] 探测 agent CLI 失败：{error}"),
                }
                this.cli.refreshing = false;
                this.cli.checked_at = Some(Instant::now());
                cx.notify();
            });
        })
        .detach();
    }

    /// 用运行时那台机器上的包管理器装缺的 CLI。装完不论成败都重新探测一次：PATH 没更新、
    /// 包管理器说装好了却跑不起来，都要马上反映出来，不只报一句笼统的「完成」。
    fn install_cli(&mut self, agent: ConversationAgentKind, cx: &mut Context<Self>) {
        if self.cli.refreshing || self.cli.installing.is_some() {
            return;
        }
        self.cli.installing = Some(agent);
        self.cli.install_result = None;
        cx.notify();
        let installed = api::call::<AgentCliInstall>(
            &RuntimeKey::Local,
            AgentCliInstallParams {
                agent: agent.id().to_string(),
            },
            CLI_INSTALL_TIMEOUT,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let (result, diagnostics) = match installed.await {
                Ok(installed) => (Ok(installed.installer), Some(installed.diagnostics)),
                // 装到一半失败也可能已经改了东西，重新探测一次。
                Err(error) => {
                    let Ok(inspected) = this.update(cx, |_, cx| inspect_clis(cx)) else {
                        return;
                    };
                    (Err(error), inspected.await.ok())
                }
            };
            let _ = this.update(cx, |this, cx| {
                // 探测不到的不当作没装：装成了就照直说，别让一次没探到盖掉真实结果。
                let available = diagnostics
                    .as_ref()
                    .and_then(|diagnostics| diagnostics.for_agent(agent))
                    .is_none_or(RuntimeExecutable::is_available);
                this.cli.install_result = Some(match result {
                    Ok(installer) if available => (
                        t!(
                            "cli.installed",
                            agent = agent.label(),
                            installer = installer
                        )
                        .into_owned(),
                        false,
                    ),
                    Ok(installer) => (
                        t!(
                            "cli.installed_not_found",
                            agent = agent.label(),
                            installer = installer
                        )
                        .into_owned(),
                        true,
                    ),
                    Err(error) => (t!("cli.install_failed", error = error).into_owned(), true),
                });
                if diagnostics.is_some() {
                    this.cli.diagnostics = diagnostics;
                }
                this.cli.installing = None;
                this.cli.checked_at = Some(Instant::now());
                cx.notify();
            });
        })
        .detach();
    }

    // —— Agent hooks ——

    fn refresh_hooks(&mut self, cx: &mut Context<Self>) {
        let status = api::call::<api::model::control_api::AgentHooksStatus>(
            &RuntimeKey::Local,
            EmptyParams::default(),
            api::CALL_TIMEOUT,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let status = status.await;
            let _ = this.update(cx, |this, cx| match status {
                Ok(status) => {
                    this.hooks.status = Some(status);
                    cx.notify();
                }
                Err(error) => eprintln!("[agents] 读 agent hooks 状态失败：{error}"),
            });
        })
        .detach();
    }

    /// 装上或移除。先把用户的意思记进配置（启动时按它校正）：装到一半失败下次启动会接着
    /// 装，移除到一半失败重启也不会被装回来。
    fn set_hooks(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.hooks.busy {
            return;
        }
        crate::model::report_edit(
            agent_hosts::edit(
                &RuntimeKey::Local,
                move |settings| settings.hooks_enabled = enabled,
                cx,
            ),
            cx,
        );
        self.hooks.busy = true;
        cx.notify();
        let (done, failed) = if enabled {
            (t!("hooks.installed"), t!("hooks.install_failed"))
        } else {
            (t!("hooks.removed"), t!("hooks.remove_failed"))
        };
        let changed = if enabled {
            api::call::<AgentHooksInstall>(
                &RuntimeKey::Local,
                EmptyParams::default(),
                api::CALL_TIMEOUT,
                cx,
            )
        } else {
            api::call::<AgentHooksUninstall>(
                &RuntimeKey::Local,
                EmptyParams::default(),
                api::CALL_TIMEOUT,
                cx,
            )
        };
        cx.spawn(async move |this, cx| {
            let changed = changed.await;
            let _ = this.update(cx, |this, cx| {
                this.hooks.busy = false;
                match changed {
                    Ok(status) => {
                        this.hooks.status = Some(status);
                        notify(&t!("notify.done"), done.into_owned(), cx);
                    }
                    Err(error) => {
                        eprintln!("[agents] agent hooks：{error}");
                        notify(
                            &t!("notify.error"),
                            t!("error.detail", what = failed, error = error).into_owned(),
                            cx,
                        );
                        // 到一半失败时有的 provider 已经改了，重新取一次真实状态。
                        this.refresh_hooks(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // —— 启动项 ——

    /// 读运行时的启动项：等本机运行时在线再读。读到的和页面上的不一样（别处改过）才重建
    /// 输入框，免得打断正在输入的那一格。
    fn load_launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 还有修改没发完时读回来的不是最终的样子，等发完以运行时回的为准。
        if self.launch.sending {
            return;
        }
        self.launch.loading = Some(cx.spawn_in(window, async move |this, cx| {
            if !api::wait_online(&RuntimeKey::Local, 0, cx).await {
                return;
            }
            let Ok(fetched) = this.update(cx, |_, cx| {
                api::call::<LaunchGet>(
                    &RuntimeKey::Local,
                    EmptyParams::default(),
                    api::CALL_TIMEOUT,
                    cx,
                )
            }) else {
                return;
            };
            let fetched = fetched.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match fetched {
                    Ok(settings) => {
                        this.launch.error = None;
                        if this.launch.entries.as_ref() != Some(&settings.entries) {
                            this.launch.entries = Some(settings.entries);
                            this.rebuild_launch_rows(window, cx);
                        }
                    }
                    Err(error) => this.launch.error = Some(error),
                }
                cx.notify();
            });
        }));
    }

    fn rebuild_launch_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.launch.entries.clone().unwrap_or_default();
        let mut rows = Vec::with_capacity(entries.len());
        let mut subscriptions = Vec::with_capacity(entries.len() * 2);
        for (index, entry) in entries.iter().enumerate() {
            let label = cx.new(|cx| InputState::new(window, cx).default_value(entry.label.clone()));
            let command =
                cx.new(|cx| InputState::new(window, cx).default_value(entry.command.clone()));
            subscriptions.push(cx.subscribe_in(
                &label,
                window,
                move |this, input, event: &InputEvent, window, cx| {
                    if save_on_input_event(event) {
                        let value = input.read(cx).value().to_string();
                        this.edit_launch(
                            move |entries| {
                                if let Some(entry) = entries.get_mut(index) {
                                    entry.label = value;
                                }
                            },
                            window,
                            cx,
                        );
                    }
                },
            ));
            subscriptions.push(cx.subscribe_in(
                &command,
                window,
                move |this, input, event: &InputEvent, window, cx| {
                    if save_on_input_event(event) {
                        let value = input.read(cx).value().to_string();
                        this.edit_launch(
                            move |entries| {
                                if let Some(entry) = entries.get_mut(index) {
                                    entry.command = value;
                                }
                            },
                            window,
                            cx,
                        );
                    }
                },
            ));
            let row = LaunchRow {
                label,
                command,
                removable: !launch_entry_is_builtin(entry),
            };
            row.label_inputs(window, cx);
            rows.push(row);
        }
        self.launch.rows = rows;
        self.launch._subscriptions = subscriptions;
    }

    /// 改启动项：页面上立刻生效，整表排队发给运行时。还没读到时不接受修改。
    fn edit_launch(
        &mut self,
        f: impl FnOnce(&mut Vec<LaunchEntry>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self.launch.entries.clone() else {
            notify(
                &t!("notify.error"),
                t!("launch.not_loaded").into_owned(),
                cx,
            );
            return;
        };
        let mut next = current.clone();
        f(&mut next);
        if next == current {
            return;
        }
        let rebuild = next.len() != current.len();
        self.launch.entries = Some(next.clone());
        if rebuild {
            self.rebuild_launch_rows(window, cx);
        }
        self.launch.pending.push_back((current, next));
        self.send_launch(window, cx);
        cx.notify();
    }

    fn send_launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.launch.sending {
            return;
        }
        let Some((base, entries)) = self.launch.pending.pop_front() else {
            return;
        };
        self.launch.sending = true;
        // 正在读的那份比这次修改旧，作废。
        self.launch.loading = None;
        let replaced = api::call::<LaunchReplace>(
            &RuntimeKey::Local,
            LaunchReplaceParams { base, entries },
            api::CALL_TIMEOUT,
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let replaced = replaced.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.launch.sending = false;
                match replaced {
                    // 后面还排着修改时这一份不是最终的样子，先不盖页面上的。
                    Ok(settings) => {
                        if this.launch.pending.is_empty() {
                            this.launch.entries = Some(settings.entries);
                        }
                        this.send_launch(window, cx);
                    }
                    // 冲突或失败都以运行时为准：排着的修改的底已经不成立，丢掉，重新读一份
                    // 换掉页面上的。
                    Err(error) => {
                        this.launch.pending.clear();
                        notify(
                            &t!("notify.error"),
                            t!("launch.save_failed", error = error).into_owned(),
                            cx,
                        );
                        this.launch.entries = None;
                        this.load_launch(window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add_launch_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_launch(
            |entries| {
                entries.push(LaunchEntry {
                    label: t!("launch.new_entry").into_owned(),
                    command: String::new(),
                    provider: None,
                });
            },
            window,
            cx,
        );
    }

    /// 删一条自定义启动项。内置项运行时也拒绝删，这里先挡住。
    fn remove_launch_entry(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_launch(
            |entries| {
                if entries
                    .get(index)
                    .is_some_and(|entry| !launch_entry_is_builtin(entry))
                {
                    entries.remove(index);
                }
            },
            window,
            cx,
        );
    }

    // —— 自定义 Agent ——

    /// 增删、换类型都是运行时推回新配置后才看得到，那时这一排和配置对不上就重建；只改了字的
    /// 不重建，免得打断正在输入的那一格。
    fn sync_profile_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let local = agent_hosts::settings(&RuntimeKey::Local, cx);
        let profiles = local
            .map(|settings| settings.profiles.clone())
            .unwrap_or_default();
        self.profiles_state = match local {
            Some(_) => Ok(()),
            None => Err(agent_hosts::unavailable(&RuntimeKey::Local, cx).map(str::to_string)),
        };
        let stale = self.profiles.len() != profiles.len()
            || self.profiles.iter().zip(&profiles).any(|(row, profile)| {
                row.profile.id != profile.id || row.profile.kind_id != profile.kind_id
            });
        if !stale {
            // 只改了字：换上新配置（卡片上的校验跟着它），输入框不动。
            if self
                .profiles
                .iter()
                .zip(&profiles)
                .any(|(row, profile)| row.profile != *profile)
            {
                for (row, profile) in Rc::make_mut(&mut self.profiles).iter_mut().zip(profiles) {
                    row.profile = profile;
                }
            }
            return;
        }
        let mut rows = Vec::with_capacity(profiles.len());
        let mut subscriptions = Vec::new();
        for profile in &profiles {
            let label =
                cx.new(|cx| InputState::new(window, cx).default_value(profile.label.clone()));
            let command =
                cx.new(|cx| InputState::new(window, cx).default_value(profile.command.clone()));
            let env = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, 8)
                    .default_value(format_env_lines(&profile.env))
            });
            let dir = cx
                .new(|cx| InputState::new(window, cx).default_value(profile.workspace_dir.clone()));
            subscriptions.push(save_profile_text(&label, &profile.id, cx, |p, value| {
                p.label = value;
            }));
            subscriptions.push(save_profile_text(&command, &profile.id, cx, |p, value| {
                p.command = value;
            }));
            subscriptions.push(save_profile_text(&dir, &profile.id, cx, |p, value| {
                p.workspace_dir = value;
            }));
            let id = profile.id.clone();
            subscriptions.push(
                cx.subscribe(&env, move |this, input, event: &InputEvent, cx| {
                    if !save_on_input_event(event) {
                        return;
                    }
                    // 写到一半的变量名不合法很正常：只在解析得了时存，错误挂在这一行上。
                    let parsed = parse_env_lines(&input.read(cx).value());
                    let error = parsed.as_ref().err().cloned();
                    if this
                        .profiles
                        .iter()
                        .any(|row| row.profile.id == id && row.env_error != error)
                        && let Some(row) = Rc::make_mut(&mut this.profiles)
                            .iter_mut()
                            .find(|row| row.profile.id == id)
                    {
                        row.env_error = error;
                        cx.notify();
                    }
                    if let Ok(env) = parsed {
                        edit_profile(&id, move |p| p.env = env, cx);
                    }
                }),
            );
            let row = ProfileRow {
                profile: profile.clone(),
                label,
                command,
                env,
                dir,
                env_error: None,
            };
            row.label_inputs(window, cx);
            rows.push(row);
        }
        self.profiles = Rc::new(rows);
        self._profile_subscriptions = subscriptions;
    }
}

impl PluginView for AgentSettingsPage {
    fn open(&mut self, _: &api::serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_shown(window, cx);
    }
}

impl Render for AgentSettingsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        SettingsPage::new("agents-settings")
            .child(
                SettingsSection::new()
                    .title(t!("settings.local_agents"))
                    .children(self.cli_cells(entity.clone()))
                    .child(self.hooks_cell(entity.clone())),
            )
            .child(
                SettingsSection::new()
                    .title(t!("settings.custom_agents"))
                    .description(t!("profile.section_description"))
                    .children(self.profile_cells(entity.clone())),
            )
            .child(
                SettingsSection::new()
                    .title(t!("settings.new_menu"))
                    .description(t!("launch.section_description"))
                    .children(self.launch_cells(entity)),
            )
            .child(quota_section(cx))
    }
}

// —— 各分组 ——

impl AgentSettingsPage {
    /// 检测时间和刷新、每家 CLI 一格、安装结果。
    fn cli_cells(&self, page: Entity<Self>) -> Vec<AnyElement> {
        let refreshing = self.cli.refreshing;
        let installing = self.cli.installing;
        let busy = refreshing || installing.is_some();
        let refresh_page = page.clone();
        let refresh = Button::new("refresh-agent-cli")
            .quiet()
            .small()
            .icon(IconName::Refresh)
            .loading(refreshing)
            .disabled(busy)
            .icon_action(t!("cli.refresh"))
            .on_click(move |_, _, cx| {
                refresh_page.update(cx, |page, cx| page.refresh_cli(cx));
            });
        let header = div()
            .flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::muted()))
                    .child(checked_at_text(self.cli.checked_at)),
            )
            .child(refresh)
            .into_any_element();
        let mut cells = vec![header];
        let Some(diagnostics) = &self.cli.diagnostics else {
            cells.push(
                div()
                    .type_style(TYPE.ui)
                    .text_color(rgb(ui_theme::muted()))
                    .child(SharedString::from(if refreshing {
                        t!("cli.detecting")
                    } else {
                        t!("cli.no_status")
                    }))
                    .into_any_element(),
            );
            return cells;
        };
        cells.extend(ConversationAgentKind::ALL.into_iter().filter_map(|agent| {
            let executable = diagnostics.for_agent(agent)?;
            Some(cli_row(agent, executable, installing, busy, page.clone()))
        }));
        cells.extend(self.cli.install_result.clone().map(|(message, failed)| {
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(if failed {
                    ui_theme::bad()
                } else {
                    ui_theme::ok()
                }))
                .child(message)
                .into_any_element()
        }));
        cells
    }

    fn hooks_cell(&self, page: Entity<Self>) -> AnyElement {
        let status = &self.hooks.status;
        let busy = self.hooks.busy;
        let muted: Hsla = rgb(ui_theme::muted()).into();
        let installed = status
            .as_ref()
            .is_some_and(|status| status.providers.iter().all(|provider| provider.installed));
        // 接入项照运行时报的原样列：不是一家 Agent 一项（「Claude / Grok」共用一份配置，
        // Pi 没有 hooks），硬对到上面的 CLI 表反而对不齐。
        let providers = match status {
            Some(status) => div()
                .flex()
                .gap_3()
                .flex_wrap()
                .children(status.providers.iter().map(|provider| {
                    let dot: Hsla = if provider.installed {
                        rgb(ui_theme::ok()).into()
                    } else {
                        muted
                    };
                    div()
                        .flex()
                        .gap_1()
                        .items_center()
                        .child(div().size(px(6.)).rounded_full().bg(dot))
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .child(provider.label.clone()),
                        )
                        .child(div().type_style(TYPE.text_xsmall).text_color(muted).child(
                            SharedString::from(if provider.installed {
                                t!("hooks.connected")
                            } else {
                                t!("hooks.not_connected")
                            }),
                        ))
                }))
                .into_any_element(),
            None => div()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(t!("hooks.loading").into_owned())
                .into_any_element(),
        };
        let uninstall_page = page.clone();
        let install_page = page;
        let row = SettingsRow::new("Hooks")
            .description(t!("hooks.description"))
            .child(
                Button::new("uninstall-agent-hooks")
                    .quiet()
                    .small()
                    .label(t!("hooks.remove"))
                    .disabled(busy)
                    .on_click(move |_, _, cx| {
                        uninstall_page.update(cx, |page, cx| page.set_hooks(false, cx));
                    }),
            )
            .child(
                Button::new("install-agent-hooks")
                    .small()
                    .label(if installed {
                        t!("hooks.reinstall")
                    } else {
                        t!("hooks.install")
                    })
                    .disabled(busy)
                    .on_click(move |_, _, cx| {
                        install_page.update(cx, |page, cx| page.set_hooks(true, cx));
                    }),
            );
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap_2()
            .child(row)
            .child(providers)
            .when_some(
                status.as_ref().map(|status| status.notify_path.clone()),
                |this, path| {
                    this.child(
                        div()
                            .type_style(TYPE.text_xsmall)
                            .text_color(muted)
                            .child(format!("smelt-notify：{path}")),
                    )
                },
            )
            .into_any_element()
    }

    /// 启动项一张表（名称、命令两列），下面一格是「添加」。
    fn launch_cells(&self, page: Entity<Self>) -> Vec<AnyElement> {
        let muted = rgb(ui_theme::muted());
        if self.launch.entries.is_none() {
            let text = match &self.launch.error {
                Some(error) => t!("launch.load_failed", error = error).into_owned(),
                None => t!("launch.loading").into_owned(),
            };
            return vec![
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted)
                    .child(text)
                    .into_any_element(),
            ];
        }
        // 名称和命令并排两列：列的位置、宽度、字体三样都不同，比每格上方一行小字好认，
        // 每项也只占一行。名称短、命令长，命令占剩下的。
        let name_w = px(140.);
        let delete_w = px(28.);
        let mono = font_config::font_family();
        let table = div()
            .flex()
            .flex_col()
            .w_full()
            .gap_2()
            // 列名只在表头出现一次；最后一格给删除按钮占位，让表头和下面的行对齐。
            .child(
                div()
                    .flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted)
                    .child(div().w(name_w).flex_none().child(t!("launch.name_column").into_owned()))
                    .child(div().flex_1().min_w_0().child(t!("launch.command_column").into_owned()))
                    .child(div().w(delete_w).flex_none()),
            )
            .children(self.launch.rows.iter().enumerate().map(|(index, row)| {
                let key = row.label.entity_id();
                let delete = if row.removable {
                    let delete_page = page.clone();
                    Button::new(("delete-launch-entry", key))
                        .quiet()
                        .small()
                        .icon(IconName::Close)
                        .icon_action(t!("launch.delete"))
                        .w(delete_w)
                        .flex_none()
                        .on_click(move |_, window, cx| {
                            delete_page.update(cx, |page, cx| {
                                page.remove_launch_entry(index, window, cx);
                            });
                        })
                        .into_any_element()
                } else {
                    div().size(delete_w).flex_none().into_any_element()
                };
                div()
                    .id(("launch-entry", key))
                    .flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(TextInput::new(&row.label).w(name_w).flex_none())
                    // 命令是 shell 代码，用终端同款等宽字体。
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(TextInput::new(&row.command).font_family(mono.clone())),
                    )
                    .child(delete)
            }));
        let add = Button::new("add-launch-entry")
            .small()
            .icon(IconName::Plus)
            .label(t!("launch.add"))
            .on_click(move |_, window, cx| {
                page.update(cx, |page, cx| page.add_launch_entry(window, cx));
            });
        vec![table.into_any_element(), add.into_any_element()]
    }

    /// 每个自定义 Agent 一格，最后一格是「添加」。
    fn profile_cells(&self, page: Entity<Self>) -> Vec<AnyElement> {
        if let Err(unavailable) = &self.profiles_state {
            let text = unavailable
                .clone()
                .unwrap_or_else(|| t!("profile.loading").into_owned());
            return vec![
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::muted()))
                    .child(text)
                    .into_any_element(),
            ];
        }
        let add = Button::new("add-profile")
            .small()
            .icon(IconName::Plus)
            .label(t!("profile.add"))
            .on_click(move |_, _, cx| {
                page.update(cx, |_, cx| add_profile(cx));
            });
        self.profiles
            .iter()
            .map(profile_cell)
            .chain(std::iter::once(add.into_any_element()))
            .collect()
    }
}

/// 一家 CLI 一行：名字、版本或问题、路径，缺的给个安装按钮。
fn cli_row(
    agent: ConversationAgentKind,
    executable: &RuntimeExecutable,
    installing: Option<ConversationAgentKind>,
    busy: bool,
    page: Entity<AgentSettingsPage>,
) -> AnyElement {
    let (status, color) = if !executable.is_available() {
        (t!("cli.not_installed").into_owned(), ui_theme::bad())
    } else if let Some(error) = &executable.error {
        (
            t!("cli.detect_failed", error = error).into_owned(),
            ui_theme::need(),
        )
    } else {
        (
            executable
                .version
                .clone()
                .unwrap_or_else(|| t!("cli.installed_no_version").into_owned()),
            ui_theme::ok(),
        )
    };
    let path = executable
        .path
        .clone()
        .unwrap_or_else(|| t!("cli.not_on_path").into_owned());
    let install = (!executable.is_available()).then(|| {
        let installing_this = installing == Some(agent);
        Button::new(SharedString::from(format!(
            "install-agent-cli-{}",
            agent.id()
        )))
        .small()
        .icon(IconName::Plus)
        .label(if installing_this {
            t!("cli.installing")
        } else {
            t!("cli.install")
        })
        .loading(installing_this)
        .disabled(busy)
        .tooltip(t!("cli.install_tooltip", agent = agent.label()))
        .on_click(move |_, _, cx| {
            page.update(cx, |page, cx| page.install_cli(agent, cx));
        })
    });
    div()
        .flex()
        .w_full()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(102.))
                .flex_none()
                .type_style(TYPE.ui)
                .text_color(rgb(ui_theme::text()))
                .child(agent.label()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .overflow_hidden()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(color))
                        .child(truncated_line(status)),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(ui_theme::muted()))
                        .child(truncated_line(path)),
                ),
        )
        .children(install)
        .into_any_element()
}

/// 一个自定义 Agent：类型、名字、删除，下面启动命令、环境变量、数据目录。
fn profile_cell(inputs: &ProfileRow) -> AnyElement {
    let profile = &inputs.profile;
    let key = inputs.label.entity_id();
    let muted = rgb(ui_theme::muted());
    let mono = font_config::font_family();
    let caption = move |text: std::borrow::Cow<'static, str>| {
        div()
            .w(px(72.))
            .flex_none()
            .type_style(TYPE.text_xsmall)
            .text_color(muted)
            .child(text)
    };
    let kind_label = match profile.kind() {
        Some(ConversationAgentKind::Custom) => t!("profile.kind_generic").into_owned(),
        Some(kind) => t!("profile.kind_based", agent = kind.short_label()).into_owned(),
        None => t!("profile.kind_unknown", id = profile.kind_id).into_owned(),
    };
    let kind_id = profile.id.clone();
    let delete_id = profile.id.clone();
    let kind_button = Button::new(("profile-kind", key))
        .small()
        .w_full()
        .label(kind_label);
    let kind_picker = MenuTrigger::dropdown(("profile-kind-menu", key), kind_button, move |_, _| {
        let pick = |kind: ConversationAgentKind, label: String| {
            let id = kind_id.clone();
            MenuItem::new(label).on_click(move |_, _, cx| set_profile_kind(&id, kind, cx))
        };
        let menu = Menu::new()
            .item(pick(
                ConversationAgentKind::Custom,
                t!("profile.kind_generic").into_owned(),
            ))
            .separator();
        // `ALL` 不含通用 ACP，上面单列了。
        ConversationAgentKind::ALL
            .into_iter()
            .fold(menu, |menu, kind| {
                menu.item(pick(
                    kind,
                    t!("profile.kind_based", agent = kind.label()).into_owned(),
                ))
            })
    })
    // 宽度落在包住按钮的那层上，按钮铺满它。
    .w(px(150.))
    .flex_none();
    let card = div()
        .id(("profile", key))
        .flex()
        .flex_col()
        .w_full()
        .gap_2()
        .child(
            div()
                .flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(kind_picker)
                .child(div().flex_1().child(TextInput::new(&inputs.label)))
                .child(
                    Button::new(("delete-profile", key))
                        .quiet()
                        .small()
                        .icon(IconName::Close)
                        .icon_action(t!("profile.delete"))
                        .w(px(28.))
                        .flex_none()
                        .on_click(move |_, _, cx| remove_profile(&delete_id, cx)),
                ),
        )
        .child(
            div()
                .flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(caption(t!("profile.command")))
                .child(
                    div()
                        .flex_1()
                        .child(TextInput::new(&inputs.command).font_family(mono.clone())),
                ),
        )
        .child(
            div()
                .flex()
                .w_full()
                .gap_2()
                .items_start()
                .child(caption(t!("profile.env")).pt_1())
                .child(div().flex_1().child(TextArea::new(&inputs.env))),
        )
        .when_some(profile.workspace_env_var(), |card, var| {
            card.child(
                div()
                    .flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(caption(t!("profile.dir")))
                    .child(
                        div()
                            .flex_1()
                            .child(TextInput::new(&inputs.dir).font_family(mono.clone())),
                    )
                    .child(
                        div()
                            .type_style(TYPE.text_xsmall)
                            .text_color(muted)
                            .child(var),
                    ),
            )
        });
    // 环境变量写错时没存，先报这条；其余按存下来的配置判断能不能启动。
    let problem = inputs
        .env_error
        .clone()
        .or_else(|| profile.validate().err());
    card.when_some(problem, |card, problem| {
        card.child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::bad()))
                .child(problem),
        )
    })
    .into_any_element()
}

/// 每家账户一行：查询开关，钥匙串被拒时再带一个重试。开关在页面露出来时和每轮刷新后读好，
/// 这里只读那份。
fn quota_section(cx: &App) -> SettingsSection {
    let settings = cx.global::<quota::QuotaSettings>().clone();
    SettingsSection::new()
        .title(t!("quota.title"))
        .children(settings.error.map(|error| {
            SettingsRow::new(t!("quota.read_failed")).child(
                div()
                    .max_w(px(320.))
                    .overflow_hidden()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::muted()))
                    .child(truncated_line(error)),
            )
        }))
        .children(settings.providers.into_iter().map(quota_row))
}

fn quota_row(provider: QuotaProviderSetting) -> SettingsRow {
    let target = provider.target;
    let denied = provider.enabled && provider.keychain_denied;
    let id = target.id();
    let row = SettingsRow::new(target.display_name());
    let row = if denied {
        row.description(t!("quota.keychain_denied")).child(
            Button::new(SharedString::from(format!("quota-retry-{id}")))
                .quiet()
                .small()
                .label(t!("common.retry"))
                .on_click(move |_, _, cx| quota::request_refresh(target, cx)),
        )
    } else {
        row
    };
    row.child(
        Switch::new(SharedString::from(format!("quota-enabled-{id}")))
            .checked(provider.enabled)
            .accessibility_label(t!("quota.enable_label", name = target.display_name()))
            .on_change(move |checked, _, cx| quota::set_enabled(target, checked, cx)),
    )
}

// —— 小工具 ——

fn checked_at_text(checked_at: Option<Instant>) -> String {
    let Some(checked_at) = checked_at else {
        return t!("cli.never_checked").into_owned();
    };
    let elapsed = checked_at.elapsed().as_secs();
    match elapsed {
        0..=4 => t!("cli.checked_now").into_owned(),
        5..=59 => t!("cli.checked_seconds", count = elapsed).into_owned(),
        60..=3599 => t!("cli.checked_minutes", count = elapsed / 60).into_owned(),
        _ => t!("cli.checked_hours", count = elapsed / 3600).into_owned(),
    }
}

fn inspect_clis(cx: &App) -> Task<Result<AcpRuntimeDiagnostics, String>> {
    api::call::<AgentCliInspect>(
        &RuntimeKey::Local,
        EmptyParams::default(),
        api::CALL_TIMEOUT,
        cx,
    )
}

fn notify(subtitle: &str, body: String, cx: &mut App) {
    let notify = api::Notify {
        subtitle: subtitle.to_string(),
        body,
        ..Default::default()
    };
    api::NOTIFY.run(&notify, cx).detach();
}

/// 按 id 改本机的一个自定义 Agent：配置可能在画和点之间被整份换掉，下标会指到别的。
fn edit_profile(id: &str, f: impl FnOnce(&mut AcpProfile) + 'static, cx: &mut App) {
    let id = id.to_string();
    let edited = agent_hosts::edit(
        &RuntimeKey::Local,
        move |settings| {
            if let Some(profile) = settings.profiles.iter_mut().find(|p| p.id == id) {
                f(profile);
            }
        },
        cx,
    );
    crate::model::report_edit(edited, cx);
}

/// 单行输入框：改动和失焦都直接写回对应字段。
fn save_profile_text(
    input: &Entity<InputState>,
    id: &str,
    cx: &mut Context<AgentSettingsPage>,
    apply: fn(&mut AcpProfile, String),
) -> Subscription {
    let id = id.to_string();
    cx.subscribe(input, move |_, input, event: &InputEvent, cx| {
        if save_on_input_event(event) {
            let value = input.read(cx).value().to_string();
            edit_profile(&id, move |profile| apply(profile, value), cx);
        }
    })
}

/// 新加一个自定义 Agent：默认是通用 ACP，要自己填启动命令；想沿用某个内置 Agent 的适配器和
/// 历史，换类型即可。
fn add_profile(cx: &mut App) {
    let added = agent_hosts::edit(
        &RuntimeKey::Local,
        |settings| {
            settings.profiles.push(AcpProfile {
                id: uuid::Uuid::new_v4().to_string(),
                kind_id: ConversationAgentKind::Custom.id().to_string(),
                label: t!("profile.new").into_owned(),
                command: String::new(),
                env: Default::default(),
                workspace_dir: String::new(),
            });
        },
        cx,
    );
    crate::model::report_edit(added, cx);
}

fn remove_profile(id: &str, cx: &mut App) {
    let id = id.to_string();
    let removed = agent_hosts::edit(
        &RuntimeKey::Local,
        move |settings| settings.profiles.retain(|profile| profile.id != id),
        cx,
    );
    crate::model::report_edit(removed, cx);
}

/// 换类型。换到不支持数据目录的类型时一并清掉目录，不然这一项就成了起不来的配置。
fn set_profile_kind(id: &str, kind: ConversationAgentKind, cx: &mut App) {
    edit_profile(
        id,
        move |profile| {
            profile.kind_id = kind.id().to_string();
            if profile.workspace_env_var().is_none() {
                profile.workspace_dir.clear();
            }
        },
        cx,
    );
}
