//! Copilot 额度的数据。查询（读凭据、钥匙串、调接口）在运行时。

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CopilotQuotaUnit {
    AiCredits,
    PremiumRequests,
}

impl CopilotQuotaUnit {
    pub fn label(self) -> &'static str {
        match self {
            Self::AiCredits => "AI credits",
            Self::PremiumRequests => "premium requests",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CopilotQuota {
    pub unit: CopilotQuotaUnit,
    pub remaining: f64,
    pub limit: Option<f64>,
    pub used: Option<f64>,
}

/// 查询状态始终带时间，即使这次输出没有识别到额度；这样 UI 能结束「查询中」
/// 状态，也不会因为服务端返回了未知格式而反复查询。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CopilotQuotaStatus {
    pub quota: Option<CopilotQuota>,
    pub checked_at_ms: u64,
    /// 最近一次刷新失败原因。失败即失效，此时 quota 为空、侧栏不显示。
    #[serde(default)]
    pub error: Option<String>,
}

pub fn format_amount(value: f64) -> String {
    if (value - value.round()).abs() < 0.005 {
        return value.round().to_string();
    }
    let mut formatted = format!("{value:.2}");
    while formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}
