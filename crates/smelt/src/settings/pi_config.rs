//! Pi 配置的客户端缓存。
//!
//! 配置文件在运行时那台机器上，读写都走 `pi.*`（[`smelt_model::pi_service`]）。这里
//! 按机器缓存最近一次快照给渲染读；打开设置窗口、进能力库、开智能体编辑器时刷新，每次
//! 修改由运行时回带新快照。
//!
//! 能力库可以切到别的机器去配（[`PiLibrary`]），智能体编辑器读智能体所在那台机器的：
//! 都从同一份按机器的缓存取。

use super::*;
use smelt_model::control_api::PiConfigGet;
use smelt_model::pi_model_settings::PiModelChoiceGroup;
use smelt_model::pi_service::PiConfig;
use smelt_runtime_client::protocol::control::{ControlCall, EmptyParams};

/// 能力库眼下在配哪台机器的 Pi。
pub(crate) struct PiLibrary {
    pub(crate) runtime: crate::RuntimeKey,
    /// 换一次机器加一：换之前发出去、还没回来的调用作废，免得上一台的结果落到这一台。
    epoch: u64,
}

impl Default for PiLibrary {
    fn default() -> Self {
        Self {
            runtime: crate::RuntimeKey::Local,
            epoch: 0,
        }
    }
}

/// 一台机器的 Pi 配置快照。
#[derive(Default)]
pub(crate) struct PiConfigSlot {
    config: Option<PiConfig>,
    /// 最近一次读取失败的原因。
    error: Option<String>,
    /// 读取的代数：晚回来的旧读取不能盖掉更新的快照。
    generation: u64,
}

/// 各台机器的可选模型：记忆设置页、智能体定义页的下拉菜单拿不到 `Workspace`。
#[derive(Default)]
pub struct PiModelChoices(std::collections::HashMap<crate::RuntimeKey, Vec<PiModelChoiceGroup>>);

impl Global for PiModelChoices {}

/// 那台机器上可选的模型；快照还没到是空的。
pub fn pi_model_choice_groups(runtime: &crate::RuntimeKey, cx: &App) -> Vec<PiModelChoiceGroup> {
    cx.try_global::<PiModelChoices>()
        .and_then(|choices| choices.0.get(runtime).cloned())
        .unwrap_or_default()
}

impl Workspace {
    /// 那台机器的 Pi 配置快照；还没取到是 `None`。
    pub(crate) fn pi_config_on(&self, runtime: &crate::RuntimeKey) -> Option<&PiConfig> {
        self.pi_configs.get(runtime)?.config.as_ref()
    }

    /// 那台机器的快照还没取到时，各块显示的话。
    pub(super) fn pi_config_pending_text(&self, runtime: &crate::RuntimeKey) -> String {
        self.pi_configs
            .get(runtime)
            .and_then(|slot| slot.error.clone())
            .unwrap_or_else(|| match runtime {
                crate::RuntimeKey::Local => "正在读取 Pi 配置…".to_string(),
                crate::RuntimeKey::Remote(_) => "正在读取这台机器的 Pi 配置…".to_string(),
            })
    }

    /// 从那台机器取一次 Pi 配置快照。回来时如果已经有更新的快照（期间有修改回带的、
    /// 或又发了一次读取），这份就丢掉。
    pub(crate) fn refresh_pi_config(&mut self, runtime: crate::RuntimeKey, cx: &mut Context<Self>) {
        let slot = self.pi_configs.entry(runtime.clone()).or_default();
        slot.generation += 1;
        let generation = slot.generation;
        let target = runtime.clone();
        self.run_call_on::<PiConfigGet>(
            &runtime,
            EmptyParams::default(),
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            move |this, result, cx| {
                if this
                    .pi_configs
                    .get(&target)
                    .is_none_or(|slot| slot.generation != generation)
                {
                    return;
                }
                match result {
                    Ok(config) => this.adopt_pi_config_on(target, config, false, cx),
                    Err(error) => {
                        eprintln!("[settings] 读取 Pi 配置失败：{error}");
                        if let Some(slot) = this.pi_configs.get_mut(&target) {
                            slot.error = Some(error);
                        }
                        cx.notify();
                    }
                }
            },
        );
    }

    /// 能力库正在配的那台机器的快照。
    pub(crate) fn pi_library_config(&self) -> Option<&PiConfig> {
        self.pi_config_on(&self.pi_library.runtime)
    }

    /// 能力库那台机器的快照还没到时，各块显示的话。
    pub(super) fn pi_library_pending_text(&self) -> String {
        self.pi_config_pending_text(&self.pi_library.runtime)
    }

    /// 能力库在配哪台机器。
    pub(crate) fn pi_library_runtime(&self) -> &crate::RuntimeKey {
        &self.pi_library.runtime
    }

    /// 能力库配的那台远程机器已经移除时，回到本机。
    pub(crate) fn ensure_pi_library_runtime(&mut self, cx: &mut Context<Self>) {
        if crate::runtimes::client(&self.pi_library.runtime, cx).is_some() {
            return;
        }
        self.select_pi_library_runtime(crate::RuntimeKey::Local, cx);
    }

    /// 重读能力库那台机器的快照。
    pub(crate) fn refresh_pi_library(&mut self, cx: &mut Context<Self>) {
        self.refresh_pi_config(self.pi_library.runtime.clone(), cx);
    }

    /// 能力库换一台机器配。上一台上就地展开的表单、登录面板、报错都不属于这一台，清掉；
    /// 登录面板先取消，取消请求还发给上一台。
    pub(crate) fn select_pi_library_runtime(
        &mut self,
        runtime: crate::RuntimeKey,
        cx: &mut Context<Self>,
    ) {
        if self.pi_library.runtime == runtime {
            return;
        }
        self.cancel_pi_login(cx);
        self.pi_library = PiLibrary {
            runtime,
            epoch: self.pi_library.epoch + 1,
        };
        self.pi_model_editor = None;
        self.pi_custom_provider_editor = None;
        self.pi_auth_providers = None;
        self.pi_auth_model_picker = None;
        self.pi_auth_error = None;
        self.pi_mcp_login = None;
        self.pi_mcp_pending_remove = None;
        self.pi_mcp_error = None;
        self.pi_model_editor_error = None;
        self.pi_plugin_pending_delete = None;
        self.pi_plugin_error = None;
        self.refresh_pi_library(cx);
        cx.notify();
    }

    /// 机器菜单里的「能力库」：切到这台机器，打开能力库。
    pub(crate) fn open_capabilities_on(
        &mut self,
        runtime: crate::RuntimeKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_pi_library_runtime(runtime, cx);
        self.activate_workspace_tab(crate::WorkspaceRoute::Capabilities, window, cx);
    }

    /// 能力库那台机器的运行时客户端。机器已经被移除时是可以直接显示的原因。
    pub(super) fn pi_library_client(
        &self,
        cx: &App,
    ) -> Result<smelt_runtime_client::RuntimeClient, String> {
        crate::runtime_call::target(&self.pi_library.runtime, cx)
    }

    /// 换上能力库那台机器的新快照。
    pub(crate) fn adopt_pi_config(
        &mut self,
        config: PiConfig,
        models_changed: bool,
        cx: &mut Context<Self>,
    ) {
        self.adopt_pi_config_on(self.pi_library.runtime.clone(), config, models_changed, cx);
    }

    /// 换上那台机器给的新快照，顺带更新开着的、属于那台机器的智能体编辑器和模型下拉。
    /// `models_changed`：凭据或模型目录变了，告诉活着的会话重新读——登录、登出、改
    /// provider 都汇到这里，散在各个调用点早晚漏一个。会话眼下只跟本机的 Pi 目录走。
    fn adopt_pi_config_on(
        &mut self,
        runtime: crate::RuntimeKey,
        config: PiConfig,
        models_changed: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.agent_surface.editor.as_mut() {
            editor.set_pi_catalog(
                &runtime,
                config.plugins.clone(),
                config.mcp_servers.clone().unwrap_or_default(),
            );
        }
        if !cx.has_global::<PiModelChoices>() {
            cx.set_global(PiModelChoices::default());
        }
        cx.global_mut::<PiModelChoices>()
            .0
            .insert(runtime.clone(), config.model_choice_groups.clone());
        if models_changed && runtime == crate::RuntimeKey::Local {
            smelt_ui::provider_status_global::ProviderStatus::bump_pi_catalog(cx);
        }
        // 修改回带的快照比之前发出、还没回来的读取都新。
        let slot = self.pi_configs.entry(runtime).or_default();
        slot.generation += 1;
        slot.config = Some(config);
        slot.error = None;
        cx.notify();
    }

    /// 能力库的调用：在后台发给能力库眼下那台机器，回到 `Workspace` 上处理结果。回来时
    /// 已经换了机器，结果就丢掉。
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
        let epoch = self.pi_library.epoch;
        let runtime = self.pi_library.runtime.clone();
        self.run_call_on::<C>(&runtime, params, timeout, cx, move |this, result, cx| {
            if this.pi_library.epoch == epoch {
                done(this, result, cx);
            }
        });
    }

    /// 在后台发给那台机器，回到 `Workspace` 上处理结果。
    pub(crate) fn run_call_on<C>(
        &mut self,
        runtime: &crate::RuntimeKey,
        params: C::Params,
        timeout: std::time::Duration,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<C::Result, String>, &mut Context<Self>) + 'static,
    ) where
        C: ControlCall + 'static,
        C::Params: Send + 'static,
        C::Result: Send + 'static,
    {
        let client = crate::runtime_call::target(runtime, cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call_on::<C>(&client?, params, timeout)
                        .map_err(crate::runtime_call::error_text)
                })
                .await;
            let _ = this.update(cx, |this, cx| done(this, result, cx));
        })
        .detach();
    }

    /// 修改类方法的通用收尾：成功换快照，失败交给 `on_error`。
    pub(crate) fn run_pi_mutation<C>(
        &mut self,
        params: C::Params,
        models_changed: bool,
        cx: &mut Context<Self>,
        on_error: impl FnOnce(&mut Self, String, &mut Context<Self>) + 'static,
    ) where
        C: ControlCall<Result = PiConfig> + 'static,
        C::Params: Send + 'static,
    {
        self.run_pi_call::<C>(
            params,
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            move |this, result, cx| match result {
                Ok(config) => this.adopt_pi_config(config, models_changed, cx),
                Err(error) => {
                    on_error(this, error, cx);
                    cx.notify();
                }
            },
        );
    }
}
