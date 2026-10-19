//! Pi 配置的客户端缓存。
//!
//! 配置文件在运行时那台机器上，读写都走 `pi.*`（[`smelt_model::pi_service`]）。这里
//! 缓存最近一次快照给渲染读；打开设置窗口、进能力库、开智能体编辑器时刷新，每次
//! 修改由运行时回带新快照。
//!
//! 能力库可以切到别的机器去配（[`PiLibrary`]）。那台机器的快照单独存，不动本机那份：
//! 智能体编辑器、记忆和模型下拉只认本机运行时。

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
    /// 远程机器的配置快照。本机的在 `Workspace::pi_config`。
    remote: Option<PiConfig>,
    remote_error: Option<String>,
    /// 同 `pi_config_generation`：比已有快照旧的读取结果丢掉。
    remote_generation: u64,
}

impl Default for PiLibrary {
    fn default() -> Self {
        Self {
            runtime: crate::RuntimeKey::Local,
            epoch: 0,
            remote: None,
            remote_error: None,
            remote_generation: 0,
        }
    }
}

/// 可选模型的全局副本：记忆设置页、智能体定义页的下拉菜单拿不到 `Workspace`。
#[derive(Default)]
pub struct PiModelChoices(pub Vec<PiModelChoiceGroup>);

impl Global for PiModelChoices {}

pub fn pi_model_choice_groups(cx: &App) -> Vec<PiModelChoiceGroup> {
    cx.try_global::<PiModelChoices>()
        .map(|choices| choices.0.clone())
        .unwrap_or_default()
}

impl Workspace {
    /// 还没取到快照时，各块显示的话。
    pub(super) fn pi_config_pending_text(&self) -> String {
        self.pi_config_error
            .clone()
            .unwrap_or_else(|| "正在读取 Pi 配置…".to_string())
    }

    /// 从本机运行时取一次 Pi 配置快照。回来时如果已经有更新的快照（期间有修改回带的、
    /// 或又发了一次读取），这份就丢掉。
    pub(crate) fn refresh_pi_config(&mut self, cx: &mut Context<Self>) {
        self.pi_config_generation += 1;
        let generation = self.pi_config_generation;
        self.run_local_call::<PiConfigGet>(
            EmptyParams::default(),
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            move |this, result, cx| {
                if this.pi_config_generation != generation {
                    return;
                }
                match result {
                    Ok(config) => this.adopt_local_pi_config(config, false, cx),
                    Err(error) => {
                        eprintln!("[settings] 读取 Pi 配置失败：{error}");
                        this.pi_config_error = Some(error);
                        cx.notify();
                    }
                }
            },
        );
    }

    /// 能力库正在配的那台机器的快照；本机就是 `pi_config`。
    pub(crate) fn pi_library_config(&self) -> Option<&PiConfig> {
        match self.pi_library.runtime {
            crate::RuntimeKey::Local => self.pi_config.as_ref(),
            crate::RuntimeKey::Remote(_) => self.pi_library.remote.as_ref(),
        }
    }

    /// 能力库那台机器的快照还没到时，各块显示的话。
    pub(super) fn pi_library_pending_text(&self) -> String {
        match self.pi_library.runtime {
            crate::RuntimeKey::Local => self.pi_config_pending_text(),
            crate::RuntimeKey::Remote(_) => self
                .pi_library
                .remote_error
                .clone()
                .unwrap_or_else(|| "正在读取这台机器的 Pi 配置…".to_string()),
        }
    }

    /// 能力库在配哪台机器。
    pub(crate) fn pi_library_runtime(&self) -> &crate::RuntimeKey {
        &self.pi_library.runtime
    }

    /// 重读能力库那台机器的快照。
    pub(crate) fn refresh_pi_library(&mut self, cx: &mut Context<Self>) {
        if self.pi_library.runtime == crate::RuntimeKey::Local {
            self.refresh_pi_config(cx);
            return;
        }
        self.pi_library.remote_generation += 1;
        let generation = self.pi_library.remote_generation;
        self.run_pi_call::<PiConfigGet>(
            EmptyParams::default(),
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            move |this, result, cx| {
                if this.pi_library.remote_generation != generation {
                    return;
                }
                match result {
                    Ok(config) => this.adopt_pi_config(config, false, cx),
                    Err(error) => {
                        this.pi_library.remote_error = Some(error);
                        cx.notify();
                    }
                }
            },
        );
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
            ..PiLibrary::default()
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

    /// 能力库那台机器的运行时客户端。机器已经被移除就没有。
    pub(super) fn pi_library_client(
        &self,
        cx: &App,
    ) -> Option<smelt_runtime_client::RuntimeClient> {
        match &self.pi_library.runtime {
            crate::RuntimeKey::Local => Some(crate::runtimes::local()),
            key => cx
                .try_global::<crate::runtimes::RuntimeHub>()?
                .remotes
                .iter()
                .find(|remote| remote.runtime.key() == key)
                .map(|remote| remote.runtime.client().clone()),
        }
    }

    /// 换上能力库那台机器的新快照。本机的照旧更新智能体编辑器、模型下拉；远程的只给
    /// 能力库自己看。
    pub(crate) fn adopt_pi_config(
        &mut self,
        config: PiConfig,
        models_changed: bool,
        cx: &mut Context<Self>,
    ) {
        if self.pi_library.runtime == crate::RuntimeKey::Local {
            self.adopt_local_pi_config(config, models_changed, cx);
            return;
        }
        // 修改回带的快照比之前发出、还没回来的读取都新。
        self.pi_library.remote_generation += 1;
        self.pi_library.remote = Some(config);
        self.pi_library.remote_error = None;
        cx.notify();
    }

    /// 换上本机运行时给的新快照。`models_changed`：凭据或模型目录变了，告诉活着的会话
    /// 重新读——登录、登出、改 provider 都汇到这里，散在各个调用点早晚漏一个。
    fn adopt_local_pi_config(
        &mut self,
        config: PiConfig,
        models_changed: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.agent_surface.editor.as_mut() {
            editor.set_pi_catalog(
                config.plugins.clone(),
                config.mcp_servers.clone().unwrap_or_default(),
            );
        }
        cx.set_global(PiModelChoices(config.model_choice_groups.clone()));
        // 修改回带的快照比之前发出、还没回来的读取都新。
        self.pi_config_generation += 1;
        self.pi_config = Some(config);
        self.pi_config_error = None;
        if models_changed {
            smelt_ui::provider_status_global::ProviderStatus::bump_pi_catalog(cx);
        }
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
        let client = self.pi_library_client(cx);
        cx.spawn(async move |this, cx| {
            let result = match client {
                Some(client) => {
                    cx.background_executor()
                        .spawn(async move {
                            client
                                .call::<C>(&params, timeout)
                                .map_err(crate::runtime_call::error_text)
                        })
                        .await
                }
                None => Err("这台机器已经移除".to_string()),
            };
            let _ = this.update(cx, |this, cx| {
                if this.pi_library.epoch == epoch {
                    done(this, result, cx);
                }
            });
        })
        .detach();
    }

    /// 只发给本机运行时的调用：智能体编辑器、记忆、DSH 这些只认本机。
    pub(crate) fn run_local_call<C>(
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
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call::<C>(params, timeout)
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
