//! `dsh.*` 的线上数据。业务执行在运行时。

use crate::agent_kind::{AcpProfile, DshPlugin, DshSettingsSummary};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DshConfig {
    pub settings_summary: Result<DshSettingsSummary, String>,
    /// 桥 `read` 的输出。
    pub model_settings: Result<serde_json::Value, String>,
    /// 装好了 Smelt Host、能用的 profile。
    pub profiles: Vec<AcpProfile>,
    /// 有目录但还没装好 Smelt Host 的 profile。
    pub missing_profiles: Vec<DshMissingProfile>,
    /// 模型目录由 Smelt 照端点维护的自定义 provider。
    pub auto_provider_ids: Vec<String>,
    pub paths: DshConfigPaths,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DshMissingProfile {
    pub name: String,
    /// 目录里有旧版或没注册的桥。
    pub has_bridge: bool,
}

/// 运行时那台机器上的路径，给人看。定位不到 `DSH_HOME` 时为空。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DshConfigPaths {
    pub settings: Option<String>,
    pub credentials: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveDefaultModelParams {
    /// 桥 `save` 的载荷（`model`、`reasoningEffort`、`baseURL`、`apiKeyEnv`、`apiKey`）。
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveCustomProviderParams {
    pub previous_id: Option<String>,
    pub id: String,
    pub display_name: String,
    pub api: String,
    pub base_url: String,
    pub api_key_env: String,
    /// 新的 API key；空表示保持已存的那份。
    pub api_key: String,
    /// 桥的模型行（`id`、`name`、`contextWindow`、`maxTokens`）。为空表示「照端点上的
    /// 来」：先问端点，问到了才落盘，并标成自动维护。
    pub models: Vec<serde_json::Value>,
    pub set_default: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnsureConfigFileParams {
    /// `true`：凭据文件；否则设置文件。
    pub credentials: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnsureConfigFileResult {
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileParams {
    pub profile: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginsResult {
    pub plugins: Result<Vec<DshPlugin>, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginAction {
    Install,
    Update,
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginActionParams {
    pub profile: String,
    pub action: PluginAction,
    /// 安装时是包名加版本范围；更新、卸载时是包名。
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginActionResult {
    /// pnpm 的结果。失败也可能已经改了一半依赖树，所以清单照样重读。
    pub outcome: Result<(), String>,
    pub plugins: Result<Vec<DshPlugin>, String>,
}
