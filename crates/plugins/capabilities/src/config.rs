//! 能力库在配哪台机器，和那台机器的 Pi 配置快照。
//!
//! 配置文件在运行时那台机器上，读写都走 `pi.*`（[`smelt_model::pi_service`]）。快照只给
//! 这一页渲染读：打开页面、换机器时读一次，每次修改由运行时回带新快照。别处（另一扇窗口、
//! 手机、命令行）经运行时改了配置，运行时推 `pi_config.changed`，宿主原样转成
//! [`HostEvent::RuntimeTopic`]，正在配那台机器就重读。

use super::*;
use api::client::RuntimeClient;
use api::client::protocol::control::{ControlCall, EmptyParams};
use api::model::control_api::PiConfigGet;
use api::model::pi_mcp::PiMcpServer;
use api::model::pi_model_settings::{PiModelChoiceGroup, PiModelSettings, PiSettingsSummary};
use api::model::pi_plugin_catalog::PiPlugin;
use api::model::pi_service::{PiConfig, PiConfigPaths};

/// 正在配的那台机器的 Pi 配置快照。
#[derive(Default)]
pub(crate) struct PiConfigSlot {
    config: Option<PiConfig>,
    /// 最近一次读取失败的原因。
    error: Option<String>,
    /// 读取的代数：晚回来的旧读取不能盖掉更新的快照。
    generation: u64,
}

impl CapabilitiesPage {
    /// 那台机器的快照；还没取到是 `None`。
    pub(crate) fn pi_library_config(&self) -> Option<&PiConfig> {
        self.pi_config.config.as_ref()
    }

    /// 快照还没到时，各块显示的话。
    pub(crate) fn pi_library_pending_text(&self) -> String {
        self.pi_config
            .error
            .clone()
            .unwrap_or_else(|| match self.machine {
                RuntimeKey::Local => "正在读取 Pi 配置…".to_string(),
                RuntimeKey::Remote(_) => "正在读取这台机器的 Pi 配置…".to_string(),
            })
    }

    /// 从那台机器取一次快照。回来时如果已经有更新的快照（期间有修改回带的、或又发了一次
    /// 读取），这份就丢掉。
    pub(crate) fn refresh_pi_library(&mut self, cx: &mut Context<Self>) {
        self.pi_config.generation += 1;
        let generation = self.pi_config.generation;
        self.run_pi_call::<PiConfigGet>(
            EmptyParams::default(),
            api::CALL_TIMEOUT,
            cx,
            move |this, result, cx| {
                if this.pi_config.generation != generation {
                    return;
                }
                match result {
                    Ok(config) => this.adopt_pi_config(config, cx),
                    Err(error) => {
                        eprintln!("[capabilities] 读取 Pi 配置失败：{error}");
                        this.pi_config.error = Some(error);
                        cx.notify();
                    }
                }
            },
        );
    }

    /// 换一台机器配。上一台上就地展开的表单、登录面板、报错、快照都不属于这一台，清掉；
    /// 登录面板先取消，取消请求还发给上一台。
    pub(crate) fn select_pi_library_runtime(
        &mut self,
        runtime: RuntimeKey,
        cx: &mut Context<Self>,
    ) {
        if self.machine == runtime {
            return;
        }
        self.cancel_pi_login(cx);
        self.machine = runtime;
        self.epoch += 1;
        self.pi_config = PiConfigSlot::default();
        self.pi_model_editor = None;
        self.pi_custom_provider_editor = None;
        self.pi_auth_providers = None;
        self.pi_auth_model_picker = None;
        self.pi_auth_error = None;
        self.pi_mcp_pending_remove = None;
        self.pi_mcp_error = None;
        self.pi_model_editor_error = None;
        self.pi_plugin_pending_delete = None;
        self.pi_plugin_error = None;
        self.agent_names_using_plugin.clear();
        self.refresh_pi_library(cx);
        if self.tab == CapabilitiesTab::Models {
            self.refresh_pi_auth_providers(cx);
        }
        cx.notify();
    }

    /// 那台机器的运行时客户端。机器已经被移除时是可以直接显示的原因。
    pub(crate) fn pi_library_client(&self, cx: &App) -> Result<RuntimeClient, String> {
        api::host(cx)
            .client(&self.machine, cx)
            .ok_or_else(|| api::MACHINE_REMOVED.to_string())
    }

    /// 换上运行时给的新快照。修改回带的快照比之前发出、还没回来的读取都新。
    pub(crate) fn adopt_pi_config(&mut self, config: PiConfig, cx: &mut Context<Self>) {
        self.pi_config.generation += 1;
        self.pi_config.config = Some(config);
        self.pi_config.error = None;
        cx.notify();
    }

    /// 在后台发给正在配的那台机器，回到本页处理结果。回来时已经换了机器，结果就丢掉。
    pub(crate) fn run_pi_call<C>(
        &mut self,
        params: C::Params,
        timeout: std::time::Duration,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<C::Result, String>, &mut Context<Self>) + 'static,
    ) where
        C: ControlCall + 'static,
        C::Params: Send + 'static,
        C::Result: Send + 'static,
    {
        let epoch = self.epoch;
        let task = api::call::<C>(&self.machine, params, timeout, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.epoch == epoch {
                    done(this, result, cx);
                }
            });
        })
        .detach();
    }

    /// 修改类方法的通用收尾：成功换快照，失败交给 `on_error`。
    pub(crate) fn run_pi_mutation<C>(
        &mut self,
        params: C::Params,
        cx: &mut Context<Self>,
        on_error: impl FnOnce(&mut Self, String, &mut Context<Self>) + 'static,
    ) where
        C: ControlCall<Result = PiConfig> + 'static,
        C::Params: Send + 'static,
    {
        self.run_pi_call::<C>(
            params,
            api::CALL_TIMEOUT,
            cx,
            move |this, result, cx| match result {
                Ok(config) => this.adopt_pi_config(config, cx),
                Err(error) => {
                    on_error(this, error, cx);
                    cx.notify();
                }
            },
        );
    }

    // 渲染读的几样，快照还没到时给出原因或空值。

    pub(crate) fn pi_model_settings(&self) -> Result<&PiModelSettings, String> {
        match self.pi_library_config() {
            Some(config) => config.model_settings.as_ref().map_err(Clone::clone),
            None => Err(self.pi_library_pending_text()),
        }
    }

    pub(crate) fn pi_settings(&self) -> Result<&PiSettingsSummary, String> {
        match self.pi_library_config() {
            Some(config) => config.settings_summary.as_ref().map_err(Clone::clone),
            None => Err(self.pi_library_pending_text()),
        }
    }

    pub(crate) fn pi_mcp_servers(&self) -> Result<&Vec<PiMcpServer>, String> {
        match self.pi_library_config() {
            Some(config) => config.mcp_servers.as_ref().map_err(Clone::clone),
            None => Err(self.pi_library_pending_text()),
        }
    }

    pub(crate) fn pi_plugins(&self) -> Vec<PiPlugin> {
        self.pi_library_config()
            .map(|config| config.plugins.clone())
            .unwrap_or_default()
    }

    pub(crate) fn pi_paths(&self) -> PiConfigPaths {
        self.pi_library_config()
            .map(|config| config.paths.clone())
            .unwrap_or_default()
    }

    /// 那台机器上可选的模型；快照还没到是空的。
    pub(crate) fn pi_model_choice_groups(&self) -> Vec<PiModelChoiceGroup> {
        self.pi_library_config()
            .map(|config| config.model_choice_groups.clone())
            .unwrap_or_default()
    }

    /// 那台机器装了远程桌面：浏览器类 MCP 预设在那里接远程桌面里的 Chrome。
    pub(crate) fn pi_mcp_remote_desktop(&self, cx: &App) -> bool {
        api::host(cx)
            .machines(cx)
            .iter()
            .any(|machine| machine.key == self.machine && machine.has_desktop)
    }
}
