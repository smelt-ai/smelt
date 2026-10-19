//! DeepSeek Harness 配置的客户端缓存。
//!
//! 配置、Node 桥、pnpm 都在运行时那台机器上（`dsh.*`，见 [`smelt_model::dsh_service`]）。
//! 这里缓存最近一次快照给渲染读；打开设置窗口时刷新，修改由运行时回带新快照。

use super::*;
use smelt_model::control_api::DshConfigGet;
use smelt_model::dsh_service::DshConfig;
use smelt_runtime_client::protocol::control::EmptyParams;

impl Workspace {
    /// 从运行时取一次 DSH 配置快照。回来时已有更新的快照就丢掉这份。
    pub(crate) fn refresh_dsh_config(&mut self, cx: &mut Context<Self>) {
        self.dsh_config_generation += 1;
        let generation = self.dsh_config_generation;
        self.run_local_call::<DshConfigGet>(
            EmptyParams::default(),
            DSH_CALL_TIMEOUT,
            cx,
            move |this, result, cx| {
                if this.dsh_config_generation != generation {
                    return;
                }
                match result {
                    Ok(config) => this.adopt_dsh_config(config, cx),
                    Err(error) => {
                        this.dsh_native_model_settings = Some(Err(error.clone()));
                        this.dsh_config_error = Some(error);
                        cx.notify();
                    }
                }
            },
        );
    }

    /// 换上运行时给的新快照。桥的模型设置在这里按桥的协议解析一次，渲染直接用。
    pub(crate) fn adopt_dsh_config(&mut self, config: DshConfig, cx: &mut Context<Self>) {
        self.dsh_config_generation += 1;
        self.dsh_native_model_settings = Some(config.model_settings.clone().and_then(|raw| {
            serde_json::from_value::<NativeDshModelSettings>(raw)
                .map_err(|error| format!("读取 DSH 模型设置响应失败：{error}"))
        }));
        self.dsh_config = Some(config);
        self.dsh_config_error = None;
        cx.notify();
    }

    pub(super) fn dsh_config_pending_text(&self) -> String {
        self.dsh_config_error
            .clone()
            .unwrap_or_else(|| "正在读取 DeepSeek Harness 配置…".to_string())
    }
}

/// 大多数 DSH 操作要起 Node 桥（还可能起完整的 dsh 运行时），几秒到十几秒。
pub(super) const DSH_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// pnpm 装插件要下载依赖，慢网络下几分钟很正常。
pub(super) const DSH_PLUGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20 * 60);
