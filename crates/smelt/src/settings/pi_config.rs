//! Pi 配置的客户端缓存。
//!
//! 配置文件在运行时那台机器上，读写都走 `pi.*`（[`smelt_core::pi_service`]）。这里
//! 缓存最近一次快照给渲染读；打开设置窗口、进能力库、开智能体编辑器时刷新，每次
//! 修改由运行时回带新快照。

use super::*;
use smelt_core::control_api::PiConfigGet;
use smelt_core::pi_model_settings::PiModelChoiceGroup;
use smelt_core::pi_service::PiConfig;
use smelt_runtime_client::protocol::control::{ControlCall, EmptyParams};

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

    /// 从运行时取一次 Pi 配置快照。回来时如果已经有更新的快照（期间有修改回带的、
    /// 或又发了一次读取），这份就丢掉。
    pub(crate) fn refresh_pi_config(&mut self, cx: &mut Context<Self>) {
        self.pi_config_generation += 1;
        let generation = self.pi_config_generation;
        self.run_pi_call::<PiConfigGet>(
            EmptyParams::default(),
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
            cx,
            move |this, result, cx| {
                if this.pi_config_generation != generation {
                    return;
                }
                match result {
                    Ok(config) => this.adopt_pi_config(config, false, cx),
                    Err(error) => {
                        eprintln!("[settings] 读取 Pi 配置失败：{error}");
                        this.pi_config_error = Some(error);
                        cx.notify();
                    }
                }
            },
        );
    }

    /// 换上运行时给的新快照。`models_changed`：凭据或模型目录变了，告诉活着的会话
    /// 重新读——登录、登出、改 provider 都汇到这里，散在各个调用点早晚漏一个。
    pub(crate) fn adopt_pi_config(
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

    /// 在后台调一个运行时方法，回到 `Workspace` 上处理结果。
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
