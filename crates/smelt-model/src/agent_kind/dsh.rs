//! DeepSeek Harness 的数据类型：模型能力报告、发现请求、插件清单、设置概况。读写
//! dsh 配置、跑 Node 桥和 pnpm 的部分在运行时（smelt-core）。

/// Smelt Host bundle 的包名与本版 Smelt 要求的版本范围。
///
/// 安装引导、界面提示、诊断文案都从这一处取：以前版本号散落在多处 shell 字符串
/// 里，升一次版就漏一处，用户装到的和我们校验的对不上。
///
/// 这是**下限与兼容范围**，不是升级目标。首装照它解析，装上的版本落在范围外时
/// 面板会提示；而「更新」按钮问的是 registry 的最新版（见 [`dsh_plugin_update`]）
/// ——桥的补丁版发得比 Smelt 勤，要求用户为了升桥先升 Smelt 是把顺序弄反了。
pub const SMELT_HOST_PACKAGE: &str = "@smelt-ai/dsh-acp-rich";

pub const SMELT_HOST_VERSION: &str = "^0.1.7";

/// 一档推理强度。`id` 发给 dsh，`name` 给人看。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DshEffort {
    pub id: String,
    pub name: String,
}

/// 一个模型实际支持的能力。
///
/// `efforts` 为 `None` 表示这条路由**没有推理强度这个旋钮**——不是"暂时没读到"，
/// 而是助手明确没有广告它。调用方必须据此完全不给选择：把它当成"那就用默认的
/// 四档"，正是这套东西要消灭的老毛病——它会让用户选到一个发一次失败一次的档位。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshModelCapability {
    pub id: String,
    /// 助手给的展示名（如 `DeepSeek-V4-Flash`）。老报告没有这个字段，所以是可选的。
    pub name: Option<String>,
    pub efforts: Option<Vec<DshEffort>>,
    pub default_effort: Option<String>,
}

impl DshModelCapability {
    /// 展示用的名字。助手没给就退回 ID——宁可显示得糙一点，也不能让选项空着。
    pub fn label(&self) -> &str {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.id)
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DshProviderCapability {
    pub id: String,
    pub name: Option<String>,
    pub models: Vec<DshModelCapability>,
}

/// `dsh-model-capabilities` 的完整报告。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DshModelCapabilities {
    pub providers: Vec<DshProviderCapability>,
}

impl DshModelCapabilities {
    /// 查一条 provider/model 路由的能力行。
    ///
    /// 查不到行本身也返回 `None`：报告里没有这个模型，说明它不是助手正在提供的
    /// 路由，我们同样没有依据给它列档位。
    pub fn model(&self, provider: &str, model: &str) -> Option<&DshModelCapability> {
        self.providers
            .iter()
            .find(|candidate| candidate.id == provider)?
            .models
            .iter()
            .find(|candidate| candidate.id == model)
    }

    /// 这条 provider 下助手正在提供的模型。
    ///
    /// 报告里没有这个 provider 就返回空切片，而不是 `None`：调用方要的是"能选
    /// 什么"，"provider 不存在"和"该 provider 没有模型"对它是同一件事。
    pub fn models(&self, provider: &str) -> &[DshModelCapability] {
        self.providers
            .iter()
            .find(|candidate| candidate.id == provider)
            .map(|candidate| candidate.models.as_slice())
            .unwrap_or_default()
    }

    /// 这条路由真正可选的档位。空列表和字段缺席一样，都算"没有"。
    pub fn efforts(&self, provider: &str, model: &str) -> Option<&[DshEffort]> {
        self.model(provider, model)?
            .efforts
            .as_deref()
            .filter(|efforts| !efforts.is_empty())
    }
}

/// 一条被发现的模型。字段和自定义 provider 编辑器的四个输入框一一对应。
///
/// 和 Pi 的直连发现共用一个类型：两条链路问的是同一件事，产出形状不该有两份，
/// 否则设置页的「采纳这个模型」也得写两遍。
pub use crate::provider_api::DiscoveredModel as DshDiscoveredModel;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DshDiscoveryReport {
    pub models: Vec<DshDiscoveredModel>,
}

/// 要问哪个端点。字段全可选，但 `provider` 和 `base_url` 至少要有一个。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshDiscoveryRequest {
    /// 已存在的路由。dsh 自带目录的 provider 会直接从目录回答，不发网络请求。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 序列化成 `baseURL`：助手那边就是这个拼法，`camelCase` 推出来的 `baseUrl`
    /// 会被静默忽略，表现为"发现永远问不到东西"。
    #[serde(rename = "baseURL", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// 一次性凭据。留空表示"用这条路由已经存着的那个"。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// 一个 profile 里的插件。
///
/// 「声明」和「装上」是两件事，所以分开记：`requested` 是 manifest 里的版本范围，
/// `installed` 是 node_modules 里真实躺着的版本。两者不一致（或 `installed` 为
/// None）正是「add 过但没装成」的现场，界面必须能把它显示出来，而不是假装装好了。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DshPlugin {
    pub name: String,
    /// manifest 中的版本范围；内置 bundle 没有这一项。
    pub requested: Option<String>,
    /// node_modules 中的实际版本。
    pub installed: Option<String>,
    pub description: Option<String>,
    /// 是否已登记进 `dsh.profile.bundles`——没登记 dsh 就不会加载它。
    pub bundled: bool,
    /// 由 dsh 自身提供、不在 profile 依赖里的内置 bundle，不可卸载。
    pub builtin: bool,
    /// 声明了 `dsh.client`（浏览器端 React 包），这部分界面只在 dsh Web 宿主里
    /// 渲染。Smelt 是原生 GPUI，没有 DOM，装上也不会显示。
    pub web_ui: bool,
}

impl DshPlugin {
    /// 装好且已登记，dsh 才真的会加载它。
    pub fn is_active(&self) -> bool {
        self.bundled && (self.builtin || self.installed.is_some())
    }

    /// Smelt Host 是 Smelt 跟 dsh 之间唯一的 ACP 通道，卸掉它这个 profile 下的
    /// DSH 会话就再也起不来了。所以它跟 dsh 自带的 bundle 一样只能更新、不能删。
    pub fn is_essential(&self) -> bool {
        self.name == SMELT_HOST_PACKAGE
    }

    /// 能否卸载。
    pub fn is_removable(&self) -> bool {
        !self.builtin && !self.is_essential()
    }

    /// 界面直接可用的状态描述。
    pub fn status(&self) -> &'static str {
        if self.builtin {
            return "dsh 内置";
        }
        if self.is_essential() {
            return match (self.installed.is_some(), self.bundled) {
                (true, true) => "Smelt 内置",
                (true, false) => "Smelt 内置（未登记为 bundle，DSH 会话无法启动）",
                _ => "Smelt 内置，但未安装；DSH 会话无法启动，请重新安装",
            };
        }
        match (self.installed.is_some(), self.bundled) {
            (true, true) if self.web_ui => "已启用（含 Web 界面，该部分在 Smelt 中不显示）",
            (true, true) => "已启用",
            (true, false) => "已安装（未登记为 bundle，dsh 不会加载）",
            (false, true) => "已声明但未安装，请重新安装",
            (false, false) => "未安装",
        }
    }
}

/// 装上的桥版本是否落在本版 Smelt 验证过的范围里。
///
/// 更新走 registry 的 `latest`，够不到的版本 Smelt 管不着——桥要是发了个破坏
/// 兼容的大版本，用户点一下就会装上它。这个判断不拦人（拦了就又变成"先升
/// Smelt"），只负责让面板说清楚："装上了，但超出本版验证范围"。
pub fn smelt_host_version_is_supported(version: &str) -> bool {
    let (Ok(requirement), Ok(version)) = (
        semver::VersionReq::parse(SMELT_HOST_VERSION),
        semver::Version::parse(version.trim()),
    ) else {
        // 解析不了就不报警：宁可漏一次提示，也不要对着一个我们没看懂的版本号
        // 吓唬用户。
        return true;
    };
    requirement.matches(&version)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DshDefaultModel {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DshSettingsSummary {
    pub path: std::path::PathBuf,
    pub default_model: Option<DshDefaultModel>,
}
