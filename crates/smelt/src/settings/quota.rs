//! 设置页「账户额度」的模型：每家的查询开关和钥匙串拒绝状态。
//!
//! 开关存在运行时，由运行时过滤（读钥匙串发生在那边）。这里只取回状态、把改动交过去。
//! 侧栏隐藏没有额度的账户，所以被拒绝的账户只能在这里手动重试。

use gpui::{App, Entity, Global};
use smelt_app_state::provider_status_global::ProviderStatus;
use smelt_model::control_api::{QuotaSetEnabled, QuotaSettingsGet};
use smelt_model::quota_service::{QuotaProviderSetting, QuotaSetEnabledParams, QuotaTarget};
use smelt_runtime_client::protocol::control::EmptyParams;

use super::remote::run_call;

/// 设置页渲染只读它。
#[derive(Clone, Default)]
pub struct QuotaSettingsState {
    pub providers: Vec<QuotaProviderSetting>,
    /// 最近一次读取或保存的失败原因。
    pub error: Option<String>,
}

impl Global for QuotaSettingsState {}

fn apply_settings(providers: Vec<QuotaProviderSetting>, cx: &mut App) {
    cx.set_global(QuotaSettingsState {
        providers,
        error: None,
    });
}

/// 失败时保留上次的列表：连不上运行时不等于开关变了。
fn apply_error(error: String, cx: &mut App) {
    let mut state = cx
        .try_global::<QuotaSettingsState>()
        .cloned()
        .unwrap_or_default();
    state.error = Some(error);
    cx.set_global(state);
}

/// 取一次开关和拒绝状态。打开设置窗口和每轮额度刷新后调。
pub(crate) fn refresh_quota_settings(cx: &mut App) {
    run_call::<QuotaSettingsGet>(EmptyParams::default(), cx, |result, cx| match result {
        Ok(settings) => apply_settings(settings.providers, cx),
        Err(error) => apply_error(error, cx),
    });
}

/// 打开或关掉一家。范围立刻更新到侧栏；打开时请求一次手动刷新，可能弹钥匙串窗口，这正是
/// 用户刚做的操作。
pub(crate) fn set_quota_enabled(
    target: QuotaTarget,
    enabled: bool,
    workspace: Entity<crate::Workspace>,
    cx: &mut App,
) {
    run_call::<QuotaSetEnabled>(
        QuotaSetEnabledParams { target, enabled },
        cx,
        move |result, cx| match result {
            Ok(settings) => {
                ProviderStatus::restore_quota(&settings.enabled_targets(), Vec::new(), cx);
                apply_settings(settings.providers, cx);
                if enabled {
                    workspace.update(cx, |ws, _| ws.request_provider_quota_refresh(target));
                }
            }
            Err(error) => apply_error(error, cx),
        },
    );
}
