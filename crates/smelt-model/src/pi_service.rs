//! `pi.*` 的线上数据。业务执行在运行时。

use crate::pi_mcp::PiMcpServer;
use crate::pi_model_settings::{
    PiCustomProvider, PiDefaultModelConfig, PiModelChoiceGroup, PiModelSettings, PiSettingsSummary,
};
use crate::pi_plugin_catalog::{PiPlugin, PiPluginKind};
use crate::provider_api::DiscoveredModel;
use serde::{Deserialize, Serialize};

/// 能力库里 Pi 相关的全部展示数据。读不出的部分各自带错误，不连累其它部分。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PiConfig {
    pub model_settings: Result<PiModelSettings, String>,
    pub settings_summary: Result<PiSettingsSummary, String>,
    pub mcp_servers: Result<Vec<PiMcpServer>, String>,
    pub plugins: Vec<PiPlugin>,
    /// 智能体定义页、记忆重排模型下拉用的可选模型。
    pub model_choice_groups: Vec<PiModelChoiceGroup>,
    /// 模型目录由 Smelt 照端点维护的自定义 provider。
    pub auto_provider_ids: Vec<String>,
    pub paths: PiConfigPaths,
}

/// 各配置文件在运行时那台机器上的路径，给人看。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PiConfigPaths {
    pub settings: String,
    pub models: String,
    pub auth: String,
    pub mcp: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiConfigFile {
    Settings,
    Models,
    Credentials,
    Mcp,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingLevelParams {
    pub level: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveDefaultModelParams {
    pub config: PiDefaultModelConfig,
    /// 新的 API key；`None` 表示保持已存的那份。
    pub api_key: Option<String>,
    /// 手动填写表单里地址留空 = 用官方端点：清掉以前写进 `models.json` 的地址覆盖。
    /// 从菜单里挑模型时不能清，自定义 provider 的地址就存在那里。
    #[serde(default)]
    pub reset_base_url: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverModelsParams {
    pub base_url: String,
    pub api: String,
    /// 表单里现填的 key。空时用 `saved_provider_id` 那份已存的。
    pub api_key: String,
    /// 正在编辑的已有 provider：没现填 key 时用它已存的凭据，客户端看不到这份凭据。
    pub saved_provider_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverModelsResult {
    pub models: Vec<DiscoveredModel>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveCustomProviderParams {
    /// 编辑已有 provider 时它原来的 id（可能改名）。
    pub previous_id: Option<String>,
    /// `models` 为空表示「照端点上的来」：先问端点，问到了才落盘，并标成自动维护。
    pub provider: PiCustomProvider,
    /// 新的 API key；`None` 表示保持已存的那份。
    pub api_key: Option<String>,
    pub set_default: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderIdParams {
    pub provider_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnsureConfigFileParams {
    pub file: PiConfigFile,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnsureConfigFileResult {
    /// 运行时那台机器上的路径。
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpPresetParams {
    pub id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerParams {
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpEnabledParams {
    pub name: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginIdParams {
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPluginParams {
    pub kind: PiPluginKind,
    /// 运行时那台机器上的路径。
    pub source: String,
}
