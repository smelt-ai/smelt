//! 各家 provider 额度的数据和展示口径。查询在运行时。

use crate::copilot_quota::CopilotQuota;

/// 一家 provider 的额度数据。两种模型互不相容——窗口型按时间窗给百分比，
/// Copilot 按本周期剩余额度计数——所以不强行合并成一个结构，由展示层归一。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderQuotaData {
    Windows(ProviderQuota),
    Copilot(CopilotQuota),
}

impl ProviderQuotaData {
    /// 侧栏额度行和模型菜单括号共用的短文本：通常是已用百分比。
    ///
    /// 两处必须是同一个口径，否则同一个账户在两个地方显示出不同的数字。
    pub fn short_text(&self) -> String {
        match self {
            Self::Windows(_) => format!("{}%", self.used_percent().round() as u32),
            Self::Copilot(quota) => match quota.limit {
                Some(_) => format!("{}%", self.used_percent().round() as u32),
                // 只知道剩余量、不知道配额时，百分比无从谈起，如实报剩余量。
                None => crate::copilot_quota::format_amount(quota.remaining),
            },
        }
    }

    /// 已用比例，用于风险配色。无法换算成比例时按 0 处理（不渲染成高风险）。
    pub fn used_percent(&self) -> f64 {
        match self {
            Self::Windows(quota) => quota
                .windows
                .iter()
                .map(|window| window.used_percent)
                .fold(0.0_f64, f64::max),
            Self::Copilot(quota) => {
                let Some(limit) = quota.limit else {
                    return 0.0;
                };
                let used = quota.used.unwrap_or((limit - quota.remaining).max(0.0));
                (used / limit.max(1.0) * 100.0).clamp(0.0, 100.0)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderQuotaWindow {
    /// 短显示名，例如 `5h`、`7d`。
    pub label: String,
    /// 当前窗口已使用百分比。
    pub used_percent: f64,
    /// 服务端给出的绝对重置时间（Unix 毫秒）。
    pub resets_at_ms: Option<u64>,
    /// 没有绝对时间时保留相对重置秒数。
    pub reset_after_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderQuota {
    pub windows: Vec<ProviderQuotaWindow>,
}

/// 查询结果总是有检查时间，即使 provider 返回未知格式或没有本地凭据；这样
/// UI 能结束「查询中」，不会在每个 Idle 快照上无限重试。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderQuotaStatus {
    pub quota: Option<ProviderQuota>,
    pub checked_at_ms: u64,
    /// 最近一次刷新失败原因。失败即失效，此时 quota 为空、侧栏不显示。
    #[serde(default)]
    pub error: Option<String>,
}
