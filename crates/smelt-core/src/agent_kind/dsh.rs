//! DSH 原生 profile、插件与 CLI 宿主。
//!
//! 身份表仍在父模块；这里只放这家 agent 的安装、发现和启动细节，避免
//! `ConversationAgentKind` 每加一家就继续膨胀。

use super::{AcpProfile, ConversationAgentKind, ConversationLaunchSpec};

/// Native Harness home, following the same `$DSH_HOME` → `~/.dsh` precedence
/// as the official launcher.
pub fn dsh_home() -> Option<std::path::PathBuf> {
    crate::login_env::dsh_home()
        .map(|home| crate::workspace_override::expand_tilde(&home))
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".dsh")))
}

/// Bridge entry inside one native profile. `dsh plugin --profile <name> add`
/// installs the package in this profile-owned dependency tree.
pub const DSH_BRIDGE_ENTRY: &str = "node_modules/@smelt-ai/dsh-acp-rich/bin/dsh-acp-rich.mjs";
pub const DSH_MODEL_SETTINGS_ENTRY: &str =
    "node_modules/@smelt-ai/dsh-acp-rich/bin/dsh-model-settings.mjs";
/// Reports which reasoning efforts each route in a profile actually supports.
///
/// Unlike the settings bridge next to it — a plain YAML reader — this one boots
/// the profile's dsh runtime, because effort availability is a property of the
/// *resolved route*, not of the model id: the same model reports four efforts
/// through one connection and none through a relay. Only the assembled runtime
/// can answer, so this costs a process launch rather than a file read.
pub const DSH_MODEL_CAPABILITIES_ENTRY: &str =
    "node_modules/@smelt-ai/dsh-acp-rich/bin/dsh-model-capabilities.mjs";

/// Asks one endpoint which models it serves, so a user configuring a gateway
/// does not type twenty-nine model ids by hand.
///
/// The discovery itself is the harness's — `ctx.llm.discoverModels` reaches
/// pi-ai's registered `GET /models` reader, which answers a route pi-ai ships
/// a catalog for from that catalog and only interrogates one it does not
/// describe. This entry exists to make that answer reachable from a process
/// that is not cordis.
pub const DSH_DISCOVER_MODELS_ENTRY: &str =
    "node_modules/@smelt-ai/dsh-acp-rich/bin/dsh-discover-models.mjs";

/// 桥的源仓库。搭建步骤、参考 profile、升级回归清单都在它的 README 里。
///
/// 这里给 URL 而不是仓内相对路径：桥已经独立成仓，不再随 smelt 分发；而且这个
/// 字符串会出现在诊断面板里，那是**装好的 app**，本来就没有 smelt 源码目录可指。
pub const DSH_BRIDGE_REPO: &str = "https://github.com/smelt-ai/dsh-acp-rich";

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

pub fn dsh_profile_dir(profile: &str) -> Option<std::path::PathBuf> {
    Some(dsh_home()?.join("profiles").join(profile))
}

pub fn dsh_bridge_entry(profile: &str) -> Option<std::path::PathBuf> {
    let entry = dsh_profile_dir(profile)?.join(DSH_BRIDGE_ENTRY);
    entry.is_file().then_some(entry)
}

pub fn dsh_model_settings_entry(profile: &str) -> Option<std::path::PathBuf> {
    let entry = dsh_profile_dir(profile)?.join(DSH_MODEL_SETTINGS_ENTRY);
    entry.is_file().then_some(entry)
}

/// 跑一次 `dsh-model-settings` 桥。
///
/// 放在 core 而不是设置页：启动时刷新自动模型目录也要用它，而那条路径上没有 UI。
/// `payload` 走 stdin 不走 argv——存自定义 provider 时它带 API key，而 argv 在
/// `ps` 里是公开的。
pub fn dsh_model_settings(
    action: &str,
    payload: Option<&serde_json::Value>,
) -> Result<Vec<u8>, String> {
    use std::io::Write as _;
    use std::process::Stdio;

    let helper = native_dsh_profiles()
        .into_iter()
        .find_map(|profile| dsh_model_settings_entry(&profile.workspace_dir))
        .ok_or_else(|| "未找到原生 DSH 模型管理器，请先安装或更新 Smelt Host bundle".to_string())?;
    let node = crate::login_env::executable_in_login_path("node")
        .ok_or_else(|| "未能在登录 shell 的 PATH 中找到 node；请安装 Node.js 后重试".to_string())?;
    let mut command = std::process::Command::new(node);
    command
        .arg(helper)
        .arg("--action")
        .arg(action)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.stdin(if payload.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    crate::login_env::apply_login_environment(&mut command);
    apply_dsh_cli_path(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动 DSH 模型管理器失败：{error}"))?;
    if let Some(payload) = payload {
        let bytes = serde_json::to_vec(payload)
            .map_err(|error| format!("序列化 DSH 模型设置失败：{error}"))?;
        child
            .stdin
            .take()
            .ok_or_else(|| "DSH 模型管理器未开放标准输入".to_string())?
            .write_all(&bytes)
            .map_err(|error| format!("写入 DSH 模型管理器失败：{error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("等待 DSH 模型管理器失败：{error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("DSH 模型管理器退出：{}", output.status)
        } else {
            // 桥用未捕获异常报错，原样转发会把整条 Node 调用栈塞进通知气泡。
            condense_node_stderr(&detail)
        });
    }
    Ok(output.stdout)
}

/// Node 子进程失败时，从 stderr 里挑出人能看懂的那一行。
///
/// 桥用未捕获异常报错，Node 会把源码片段、整条调用栈和自己的版本号一起打出来。
/// 原样塞进通知气泡，用户看到的是 `at process.processTicksAndRejections` 这种
/// 内部帧——真正的原因反而被淹没。
///
/// 认 `Error: …` 那一行：它是 Node 对任何异常的固定格式，也是唯一带原因的一行。
/// 认不出来就退回原文——宁可显示得啰嗦，也不能把唯一的线索吞掉。
pub fn condense_node_stderr(stderr: &str) -> String {
    let message = stderr
        .lines()
        .map(str::trim)
        .find_map(|line| {
            let (kind, detail) = line.split_once(": ")?;
            // `Error:` / `TypeError:` / `SyntaxError:` …——但不能把源码里的
            // `throw new Error('…')` 也当成它，那行不是以类型名开头的。
            (kind.ends_with("Error") && !kind.contains(' ') && !detail.is_empty())
                .then(|| detail.trim().to_string())
        })
        .unwrap_or_else(|| stderr.trim().to_string());
    if message.is_empty() {
        "执行失败，原因未知".to_string()
    } else {
        message
    }
}

pub fn dsh_model_capabilities_entry(profile: &str) -> Option<std::path::PathBuf> {
    let entry = dsh_profile_dir(profile)?.join(DSH_MODEL_CAPABILITIES_ENTRY);
    entry.is_file().then_some(entry)
}

pub fn dsh_discover_models_entry(profile: &str) -> Option<std::path::PathBuf> {
    let entry = dsh_profile_dir(profile)?.join(DSH_DISCOVER_MODELS_ENTRY);
    entry.is_file().then_some(entry)
}

/// 一档推理强度。`id` 发给 dsh，`name` 给人看。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct DshEffort {
    pub id: String,
    pub name: String,
}

/// 一个模型实际支持的能力。
///
/// `efforts` 为 `None` 表示这条路由**没有推理强度这个旋钮**——不是"暂时没读到"，
/// 而是助手明确没有广告它。调用方必须据此完全不给选择：把它当成"那就用默认的
/// 四档"，正是这套东西要消灭的老毛病——它会让用户选到一个发一次失败一次的档位。
#[derive(Clone, Debug, serde::Deserialize)]
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

#[derive(Clone, Debug, serde::Deserialize)]
pub struct DshProviderCapability {
    pub id: String,
    pub name: Option<String>,
    pub models: Vec<DshModelCapability>,
}

/// `dsh-model-capabilities` 的完整报告。
#[derive(Clone, Debug, serde::Deserialize)]
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

/// 问一次「这个 profile 的每条路由支持哪些推理强度」。
///
/// 会阻塞若干秒——桥要起一个完整的 dsh 运行时——所以只能在后台线程调用。必须带上
/// 登录 shell 的完整环境：桥自己要 spawn `dsh`，而 GUI 进程继承到的环境里通常连
/// node 版本管理器铺的 PATH 都没有。
pub fn dsh_model_capabilities(profile: &str) -> Result<DshModelCapabilities, String> {
    use std::process::Stdio;

    let helper = dsh_model_capabilities_entry(profile).ok_or_else(|| {
        format!("未找到模型能力查询器，请把 Smelt Host bundle 更新到 {SMELT_HOST_VERSION}")
    })?;
    let node = crate::login_env::executable_in_login_path("node")
        .ok_or_else(|| "未能在登录 shell 的 PATH 中找到 node；请安装 Node.js 后重试".to_string())?;
    let mut command = std::process::Command::new(node);
    command
        .arg(helper)
        .arg("--profile")
        .arg(profile)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::login_env::apply_login_environment(&mut command);
    apply_dsh_cli_path(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("启动模型能力查询器失败：{error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("模型能力查询器退出：{}", output.status)
        } else {
            detail
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|error| format!("解析模型能力失败：{error}"))
}

/// 一条被发现的模型。字段和自定义 provider 编辑器的四个输入框一一对应。
///
/// 和 Pi 的直连发现共用一个类型：两条链路问的是同一件事，产出形状不该有两份，
/// 否则设置页的「采纳这个模型」也得写两遍。
pub use crate::provider_api::DiscoveredModel as DshDiscoveredModel;

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct DshDiscoveryReport {
    pub models: Vec<DshDiscoveredModel>,
}

/// 要问哪个端点。字段全可选，但 `provider` 和 `base_url` 至少要有一个。
#[derive(Clone, Debug, Default, serde::Serialize)]
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

/// 问一次「这个端点提供哪些模型」。
///
/// 和能力查询一样要起一个完整 dsh 运行时（约 3-4 秒，还要加一次网络往返），只能
/// 在后台线程调用。请求走 stdin 而不是命令行参数：它可能带着用户刚敲进去、还没
/// 保存的 API key，而命令行参数在 `ps` 里是全局可见的。
pub fn dsh_discover_models(
    profile: &str,
    request: &DshDiscoveryRequest,
) -> Result<DshDiscoveryReport, String> {
    use std::io::Write as _;
    use std::process::Stdio;

    let helper = dsh_discover_models_entry(profile).ok_or_else(|| {
        format!("未找到模型发现器，请把 Smelt Host bundle 更新到 {SMELT_HOST_VERSION}")
    })?;
    let node = crate::login_env::executable_in_login_path("node")
        .ok_or_else(|| "未能在登录 shell 的 PATH 中找到 node；请安装 Node.js 后重试".to_string())?;
    let payload =
        serde_json::to_vec(request).map_err(|error| format!("序列化模型发现请求失败：{error}"))?;
    let mut command = std::process::Command::new(node);
    command
        .arg(helper)
        .arg("--profile")
        .arg(profile)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::login_env::apply_login_environment(&mut command);
    apply_dsh_cli_path(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动模型发现器失败：{error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "模型发现器未开放标准输入".to_string())?
        .write_all(&payload)
        .map_err(|error| format!("写入模型发现请求失败：{error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("等待模型发现器失败：{error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("模型发现器退出：{}", output.status)
        } else {
            condense_node_stderr(&detail)
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|error| format!("解析模型列表失败：{error}"))
}

/// A bridge dependency alone is not enough: dsh only loads packages declared
/// in `dsh.profile.bundles`. This rejects old/manual installs whose package
/// files exist but whose host patch is never applied.
pub fn dsh_profile_has_smelt_host(profile: &str) -> bool {
    dsh_bridge_entry(profile).is_some()
        && dsh_profile_dir(profile).is_some_and(|dir| {
            let manifest = std::fs::read_to_string(dir.join("package.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
            manifest.as_ref().is_some_and(|manifest| {
                manifest_declares_bundle(manifest, "@smelt-ai/dsh-acp-rich")
            })
        })
}

pub(super) fn manifest_declares_bundle(manifest: &serde_json::Value, bundle: &str) -> bool {
    manifest
        .pointer("/dsh/profile/bundles")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|bundles| bundles.iter().any(|value| value.as_str() == Some(bundle)))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:@".contains(&byte))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Launch one native profile with Smelt as its host. The wrapper delegates
/// profile composition, settings, credentials and persistence to official dsh.
pub fn dsh_profile_launch_spec(profile: &str) -> ConversationLaunchSpec {
    let command = match dsh_bridge_entry(profile) {
        Some(entry) => format!(
            "node {} --profile {}",
            shell_quote(&entry.to_string_lossy()),
            shell_quote(profile)
        ),
        None => format!("dsh-acp-rich --profile {}", shell_quote(profile)),
    };
    ConversationLaunchSpec::from_command(command)
}

/// All native DSH profiles, including profiles that do not have the Smelt
/// bundle yet. A valid profile is a directory with its own package manifest.
pub fn dsh_profile_names() -> Vec<String> {
    let Some(root) = dsh_home().map(|home| home.join("profiles")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut profiles = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            (name != "node_modules" && entry.path().join("package.json").is_file()).then_some(name)
        })
        .collect::<Vec<_>>();
    profiles.sort();
    profiles
}

/// 一个 profile 里的插件。
///
/// 「声明」和「装上」是两件事，所以分开记：`requested` 是 manifest 里的版本范围，
/// `installed` 是 node_modules 里真实躺着的版本。两者不一致（或 `installed` 为
/// None）正是「add 过但没装成」的现场，界面必须能把它显示出来，而不是假装装好了。
#[derive(Debug, Clone, PartialEq, Eq)]
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

fn read_json(path: &std::path::Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// 列出一个 profile 的插件。
///
/// 直接读 manifest 与 node_modules，而不是调 `dsh plugin list`：后者要拉起 node、
/// 解析锁文件、跑供应链校验，实测首字节要 6 秒以上，做成界面就是一片空白。文件
/// 读取是毫秒级的，且信息更全（还能看出「声明了但没装上」）。
pub fn dsh_profile_plugins(profile: &str) -> Result<Vec<DshPlugin>, String> {
    let dir = dsh_profile_dir(profile).ok_or_else(|| "无法定位 DSH profile 目录".to_string())?;
    let manifest = read_json(&dir.join("package.json"))
        .ok_or_else(|| format!("读取 profile「{profile}」的 package.json 失败"))?;

    let bundles = manifest
        .pointer("/dsh/profile/bundles")
        .and_then(serde_json::Value::as_array)
        .map(|bundles| {
            bundles
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let dependencies = manifest
        .get("dependencies")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();

    let describe = |name: &str| -> (Option<String>, Option<String>, bool) {
        let Some(installed) = read_json(&dir.join("node_modules").join(name).join("package.json"))
        else {
            return (None, None, false);
        };
        (
            installed
                .get("version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            installed
                .get("description")
                .and_then(serde_json::Value::as_str)
                .filter(|description| !description.trim().is_empty())
                .map(str::to_string),
            installed.pointer("/dsh/client").is_some(),
        )
    };

    let mut plugins = dependencies
        .iter()
        .map(|(name, range)| {
            let (installed, description, web_ui) = describe(name);
            DshPlugin {
                name: name.clone(),
                requested: range.as_str().map(str::to_string),
                installed,
                description,
                bundled: bundles.iter().any(|bundle| bundle == name),
                builtin: false,
                web_ui,
            }
        })
        .collect::<Vec<_>>();

    // dsh 自带的 bundle 不出现在 profile 依赖里，但它们确实在运行。不列出来，
    // 用户看到的就是一份和 `dsh plugin list` 对不上的残缺清单。
    for bundle in &bundles {
        if dependencies.contains_key(bundle) {
            continue;
        }
        let (installed, description, web_ui) = describe(bundle);
        plugins.push(DshPlugin {
            name: bundle.clone(),
            requested: None,
            installed,
            description,
            bundled: true,
            builtin: true,
            web_ui,
        });
    }

    plugins.sort_by(|left, right| {
        left.builtin
            .cmp(&right.builtin)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(plugins)
}

/// 原生 dsh 的启动方式：优先全局 `dsh`，没有就退回锁定版本的 `npx`。
///
/// 插件安装、菜单探测、ACP 会话、模型能力查询必须共用这一份。以前只有插件安装
/// 走 npx，探测和启动死盯 PATH 里的 `dsh`——设置页用 npx 把 profile 配好了，
/// 「+」菜单却因为找不到二进制把按钮藏掉。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DshCli {
    pub program: std::path::PathBuf,
    pub prefix_args: Vec<String>,
}

impl DshCli {
    pub fn is_npx_fallback(&self) -> bool {
        !self.prefix_args.is_empty()
    }

    pub fn npx_spec(&self) -> String {
        format!("npx {DSH_CLI_PACKAGE}@{DSH_CLI_VERSION}")
    }
}

/// 纯解析：给探测和单测用，不读真实 PATH。
pub fn resolve_dsh_cli(
    dsh: Option<std::path::PathBuf>,
    npx: Option<std::path::PathBuf>,
) -> Result<DshCli, String> {
    if let Some(dsh) = dsh {
        return Ok(DshCli {
            program: dsh,
            prefix_args: Vec::new(),
        });
    }
    if let Some(npx) = npx {
        return Ok(DshCli {
            program: npx,
            prefix_args: vec![
                "--yes".to_string(),
                format!("{DSH_CLI_PACKAGE}@{DSH_CLI_VERSION}"),
            ],
        });
    }
    Err(TOOLCHAIN_MISSING_HINT.to_string())
}

fn dsh_cli() -> Result<DshCli, String> {
    resolve_dsh_cli(
        crate::login_env::executable_in_login_path("dsh"),
        crate::login_env::executable_in_login_path("npx"),
    )
}

/// 解析出能跑 `dsh plugin` 的命令。
///
/// GUI 进程的 PATH 是 Finder 给的裁剪版，所以一律按登录 shell 的 PATH 找。没装
/// 全局 dsh 时退回 npx，这样「还没装 dsh 的新机器」也能在界面里装插件。
fn dsh_plugin_command() -> Result<(std::path::PathBuf, Vec<String>), String> {
    let cli = dsh_cli()?;
    Ok((cli.program, cli.prefix_args))
}

fn npx_dsh_shim_script(cli: &DshCli) -> String {
    let program = shell_quote(&cli.program.to_string_lossy());
    let args = cli
        .prefix_args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!("#!/bin/sh\nexec {program} {args} \"$@\"\n")
}

pub(super) fn write_dsh_npx_shim(
    dir: &std::path::Path,
    cli: &DshCli,
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("创建 dsh npx shim 目录失败：{error}"))?;
    let shim = dir.join("dsh");
    let script = npx_dsh_shim_script(cli);
    let unchanged = std::fs::read_to_string(&shim).is_ok_and(|existing| existing == script);
    if !unchanged {
        std::fs::write(&shim, script)
            .map_err(|error| format!("写入 dsh npx shim 失败：{error}"))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&shim)
            .map_err(|error| format!("读取 dsh npx shim 权限失败：{error}"))?
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim, permissions)
            .map_err(|error| format!("设置 dsh npx shim 可执行权限失败：{error}"))?;
    }
    Ok(dir.to_path_buf())
}

fn ensure_npx_dsh_shim(cli: &DshCli) -> Result<std::path::PathBuf, String> {
    let dir = dirs::home_dir()
        .ok_or_else(|| "无法定位 HOME".to_string())?
        .join(".smelt/runtime/dsh-npx");
    write_dsh_npx_shim(&dir, cli)
}

/// `@smelt-ai/dsh-acp-rich` 会 `spawn('dsh')`。没装全局 CLI 时，把 npx 退路
/// 做成 PATH 上的同名 shim，子进程不用改就能找到启动器。
pub fn prepend_dsh_cli_shim(path: &str) -> String {
    let Ok(cli) = dsh_cli() else {
        return path.to_string();
    };
    if !cli.is_npx_fallback() {
        return path.to_string();
    }
    let Ok(shim_dir) = ensure_npx_dsh_shim(&cli) else {
        return path.to_string();
    };
    format!("{}:{path}", shim_dir.display())
}

pub fn apply_dsh_cli_path(command: &mut std::process::Command) {
    command.env("PATH", prepend_dsh_cli_shim(crate::login_env::login_path()));
}

/// 找不到 Node 工具链时给的指引。用户的 node 可能来自 nvm/volta/fnm/asdf/mise/
/// homebrew，路径各不相同，所以不猜路径、也不报"某某目录不存在"——只说清"我们
/// 是按你登录 shell 的环境找的"，这样用户能自己在终端里复现同一次查找。
pub(super) const TOOLCHAIN_MISSING_HINT: &str = "未能在登录 shell 的环境里找到 dsh 或 npx。\
Smelt 用的就是你终端里那套 Node（读取登录 shell 的 PATH 与环境变量），\
请在终端执行 `which npx` 确认 Node 已安装，且其初始化写在了 shell 配置文件里（如 .zshrc）。";

pub const DSH_CLI_PACKAGE: &str = "@deepseek-ai/dsh";
pub const DSH_CLI_VERSION: &str = "0.1.0-rc.8";

/// 插件命令的整体超时。pnpm 在慢网络上拉一个带依赖的包确实要几分钟，所以给得宽；
/// 但**不能没有上限**——没有上限时"网络连不上"和"正在下载"在界面上是同一个状态
/// （永远转圈），用户只能去杀进程。这正是用户报的"卡住"。
pub(super) const PLUGIN_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// 起子进程并等它结束，超时就连同它拉起的整棵进程树一起杀掉。
///
/// 为什么要杀整棵树：dsh 只是个转发器，真正干活的是它 spawn 的 pnpm 和 pnpm 再
/// 拉起的 node。只杀 dsh 会留下 pnpm 在后台跑，下次安装撞上同一个 store 锁，
/// 于是"卡住"从一次偶发变成必然复发。
fn run_with_timeout(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid 是 async-signal-safe 的，符合 fork 与 exec 之间可调用
        // 的约束；这里让子进程自成进程组，超时后才有整组可杀。
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("启动失败：{error}"))?;
    let pid = child.id();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("等待子进程失败：{error}")),
        Err(_) => {
            #[cfg(unix)]
            unsafe {
                // setsid 之后进程组 id 等于子进程 pid，负号表示整组。
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
            Err(format!(
                "操作超过 {} 分钟仍未完成，已中止。常见原因是访问 npm registry 的网络不通：\
请在终端直接执行同一条命令确认，若需要代理，把代理设置写进 shell 配置文件（如 .zshrc 里 export HTTPS_PROXY），\
Smelt 会连同登录 shell 的环境一起带上。",
                timeout.as_secs() / 60
            ))
        }
    }
}

fn run_dsh_plugin(profile: &str, args: &[&str]) -> Result<String, String> {
    let dir = dsh_profile_dir(profile).ok_or_else(|| "无法定位 DSH profile 目录".to_string())?;
    let (program, mut argv) = dsh_plugin_command()?;
    argv.extend(
        ["plugin", "--profile", profile]
            .into_iter()
            .map(str::to_string),
    );
    argv.extend(args.iter().map(|arg| (*arg).to_string()));

    let mut command = std::process::Command::new(program);
    command.args(&argv).current_dir(&dir);
    // 整个登录 shell 环境，不只是 PATH：dsh 会转发给 pnpm，而 pnpm 与用户的
    // Node 版本管理器（volta 的 shim 要 VOLTA_HOME、pnpm 要 PNPM_HOME）以及
    // registry 代理设置全都靠环境变量传递。详见 login_env 模块头注释。
    crate::login_env::apply_login_environment(&mut command);

    let output = run_with_timeout(command, PLUGIN_COMMAND_TIMEOUT)
        .map_err(|error| format!("执行 dsh plugin 失败：{error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        return Ok(if stdout.is_empty() { stderr } else { stdout });
    }
    // 失败原因可能落在任一路：pnpm 把 404/权限之类的细节写 stdout，dsh 自己的
    // 收尾结论写 stderr。只取一路就会丢掉「为什么失败」，所以按发生顺序合起来。
    let detail = [stdout, stderr]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(hint) = toolchain_hint(&detail) {
        return Err(hint);
    }
    Err(if detail.is_empty() {
        format!("dsh plugin 执行失败（退出码 {:?}）", output.status.code())
    } else {
        detail
    })
}

/// 把工具链缺失类的失败翻译成可操作的指引。
///
/// dsh 转发给 pnpm，但**它自己不负责装 pnpm**。要匹配的报错有两种口径，缺一
/// 不可（下面每条都是真机实测抓到的原文，不是照着文档猜的）：
///
/// - dsh 自己的：`pnpm not found on PATH`。只在 spawn 报 ENOENT 时才打印。
/// - volta 的：`Volta error: Could not find executable "pnpm"`。**volta 用户走
///   的是这一条**——volta 在 `~/.volta/bin` 铺了 pnpm shim，文件真实存在，
///   spawn 成功、ENOENT 不触发，于是 dsh 那句话根本不会出现，只留下一句
///   看不出所以然的 volta 报错。
///
/// volta 用户为什么会撞上：它的 pnpm 支持是实验特性，要先 `VOLTA_FEATURE_PNPM=1`
/// 才能 `volta install pnpm`。而这个开关通常写在 `.zshrc` 里——**终端里 pnpm
/// 好好的，一进 GUI 就"不存在"**，这正是这次要修的环境断层。
///
/// **指引必须放在末尾且足够短**：设置页装不下几十行 pnpm 输出，展示前会被截成
/// 最后几行（见 `settings/workspace.rs` 的 `condense_plugin_error`）。指引放开头会被整段丢掉，
/// 用户最终看到的还是那句看不懂的英文。
pub(super) fn toolchain_hint(detail: &str) -> Option<String> {
    let lowered = detail.to_lowercase();
    let mentions_pnpm = lowered.contains("pnpm not found")
        || lowered.contains("pnpm: command not found")
        || (lowered.contains("could not find executable") && lowered.contains("pnpm"));
    let advice = if mentions_pnpm {
        "缺少 pnpm：dsh 通过 pnpm 安装插件，你的 Node 环境里没有它。\n\
请在终端执行 `npm install -g pnpm`；若用 volta 管理 Node，需先在 .zshrc 里 \
`export VOLTA_FEATURE_PNPM=1` 再 `volta install pnpm`（该开关只在终端设过是不够的）。"
    } else if lowered.contains("volta_home") || lowered.contains("node is not available") {
        "volta 没能定位到 Node 工具链。\n\
请确认 `volta install node` 装过默认版本，且 `VOLTA_HOME` 写在 shell 配置文件（如 .zshrc）里\
而不是只在当前终端设过——Smelt 读取的是登录 shell 的环境。"
    } else {
        return None;
    };
    Some(if detail.is_empty() {
        advice.to_string()
    } else {
        format!("{detail}\n{advice}")
    })
}

/// 在 profile 里安装（或升级）一个插件。`spec` 可带版本，如 `pkg@1.2.3`。
pub fn dsh_plugin_add(profile: &str, spec: &str) -> Result<String, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("请填写要安装的插件包名".to_string());
    }
    run_dsh_plugin(profile, &["add", spec, "--save-exact"])
}

/// registry 上这个包的最新版本号。
///
/// 走 profile 目录里的 pnpm，于是用户的 registry、镜像与认证配置自动生效——桥可能
/// 装在私有源上，写死 registry.npmjs.org 的查询在那种机器上一无所获。
pub fn dsh_plugin_latest_version(profile: &str, name: &str) -> Result<String, String> {
    let raw = run_dsh_plugin(profile, &["view", name, "version"])?;
    // 正常只回一行版本号；进度与告警会占前面几行，所以从后往前取第一行非空的。
    let version = raw
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .trim_matches('"')
        .to_string();
    if version.is_empty() {
        return Err(format!("未能从 registry 查到 {name} 的最新版本"));
    }
    Ok(version)
}

/// 把一个已装插件升到 registry 上的最新版。
///
/// 分两步——先问 registry 要版本号，再点名装它——是被 pnpm 的两道行为逼出来的，
/// 每一条都实测过：
///
/// 一、**范围类的 spec 一律不动**。`add <name>`、`add <name>@^0.1.6`、
///    `update <name> --latest` 全部回 "Already up to date" 并退 0：现装版本满足
///    manifest 里的范围，pnpm 就不去 registry 问。按钮点了有反应、日志也干净，
///    版本纹丝不动。
///
/// 二、**刚发布的版本够不到**。pnpm 11 默认把发布不满一天的版本从 `latest` 里
///    滤掉，而那恰恰是用户最想立刻装上的那一版。
///
/// 绕过第二道的办法不止一个，但只有点名版本是安全的：`--config.minimumReleaseAge=0`
/// 确实能装上，却会留下一个 lockfile 与策略不符的 profile——此后**任何**
/// `dsh plugin` 操作都以 ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION 失败，直到那版满
/// 一天为止。点名精确版本走的是 pnpm 自己的放行路径：它会把这一版记进
/// `minimumReleaseAgeExclude`，lockfile 与策略始终一致。
///
/// 目标版本必须在运行时问，不能用 [`SMELT_HOST_VERSION`]：那是编译进本版 Smelt 的
/// 常量，用它当目标就等于要求用户为了升桥先升 Smelt，而桥的补丁版发得比 Smelt 勤。
pub fn dsh_plugin_update(profile: &str, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("请指定要更新的插件".to_string());
    }
    let latest = dsh_plugin_latest_version(profile, name)?;
    run_dsh_plugin(
        profile,
        &["add", &format!("{name}@{latest}"), "--save-exact"],
    )
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

/// 从 profile 里卸载一个插件。
pub fn dsh_plugin_remove(profile: &str, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("请指定要卸载的插件".to_string());
    }
    // 拦在这一层而不是只拦按钮：这是唯一的卸载出口，界面之外的调用方（快捷键、
    // 以后的批量操作）同样不能把 Smelt 自己赖以运行的桥卸掉。
    if name == SMELT_HOST_PACKAGE || name.starts_with(&format!("{SMELT_HOST_PACKAGE}@")) {
        return Err(format!(
            "{SMELT_HOST_PACKAGE} 是 Smelt 内置组件，卸载后该 profile 的 DSH 会话将无法启动；只能更新，不能卸载"
        ));
    }
    run_dsh_plugin(profile, &["remove", name])
}

/// Native profiles that already have the Smelt host bundle installed.
///
/// `label` 带 profile 名，给设置页管理用。新建菜单走产品名，不读这个字段。
pub fn native_dsh_profiles() -> Vec<AcpProfile> {
    let mut profiles = dsh_profile_names()
        .into_iter()
        .filter_map(|name| {
            dsh_profile_has_smelt_host(&name).then_some(())?;
            Some(AcpProfile {
                id: format!("dsh-native-{name}"),
                kind_id: ConversationAgentKind::Dsh.id().to_string(),
                label: format!("{} · {name}", ConversationAgentKind::Dsh.label()),
                workspace_dir: name,
            })
        })
        .collect::<Vec<_>>();
    profiles.sort_by(|left, right| left.label.cmp(&right.label));
    profiles
}

pub fn dsh_sessions_root() -> Option<std::path::PathBuf> {
    dsh_home().map(|home| home.join("sessions"))
}

pub fn dsh_settings_path() -> Option<std::path::PathBuf> {
    dsh_home().map(|home| dsh_settings_path_at(&home))
}

pub fn dsh_credentials_path() -> Option<std::path::PathBuf> {
    dsh_home().map(|home| home.join(".credentials.yaml"))
}

pub(super) fn dsh_settings_path_at(home: &std::path::Path) -> std::path::PathBuf {
    // dsh-settings-file uses settings.yaml by default. Keep legacy/explicit
    // JSON documents discoverable only when the standard YAML document is absent.
    ["settings.yaml", "settings.yml", "settings.json"]
        .into_iter()
        .map(|name| home.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| home.join("settings.yaml"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DshDefaultModel {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DshSettingsSummary {
    pub path: std::path::PathBuf,
    pub default_model: Option<DshDefaultModel>,
}

/// Read the user-visible default model from the native DSH settings document.
/// Missing documents are valid: DSH creates them lazily on the first settings
/// write. Parsing errors are returned so callers never mistake a broken config
/// for an empty one.
pub fn dsh_settings_summary() -> Result<DshSettingsSummary, String> {
    let path = dsh_settings_path().ok_or_else(|| "无法定位 DSH_HOME".to_string())?;
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DshSettingsSummary {
                path,
                default_model: None,
            });
        }
        Err(error) => return Err(format!("读取 {} 失败：{error}", path.display())),
    };
    if raw.trim().is_empty() {
        return Ok(DshSettingsSummary {
            path,
            default_model: None,
        });
    }

    let default_model = parse_dsh_default_model(&raw)
        .map_err(|error| format!("解析 {} 失败：{error}", path.display()))?;
    Ok(DshSettingsSummary {
        path,
        default_model,
    })
}

pub(super) fn parse_dsh_default_model(
    raw: &str,
) -> Result<Option<DshDefaultModel>, serde_yaml::Error> {
    let document = serde_yaml::from_str::<serde_yaml::Value>(raw)?;
    Ok(document
        .as_mapping()
        .and_then(|settings| settings.get(serde_yaml::Value::String("agent-default-model".into())))
        .and_then(serde_yaml::Value::as_mapping)
        .map(|model| DshDefaultModel {
            provider: yaml_string(model, "provider"),
            model: yaml_string(model, "model"),
            reasoning_effort: yaml_string(model, "reasoningEffort"),
        }))
}

fn yaml_string(mapping: &serde_yaml::Mapping, key: &str) -> Option<String> {
    mapping
        .get(serde_yaml::Value::String(key.into()))
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod plugin_listing_tests {
    use super::*;

    /// `DSH_HOME` 是进程级的，多个用例并行改它会互相看到对方的临时目录。
    /// 凡是要覆盖它的用例都必须先拿到这把锁。
    static DSH_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 在临时 DSH_HOME 下列出插件，结束后恢复现场。
    ///
    /// 锁一直持有到读完为止：提前释放的话，另一个用例会在我们读之前就把变量改掉。
    fn plugins_under(root: &std::path::Path, profile: &str) -> Result<Vec<DshPlugin>, String> {
        let _guard = DSH_HOME_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let previous = std::env::var_os("DSH_HOME");
        // SAFETY: 由 DSH_HOME_LOCK 串行化；dsh_home 每次都重新读取该变量。
        unsafe { std::env::set_var("DSH_HOME", root) };
        let listed = dsh_profile_plugins(profile);
        // SAFETY: 同上，仍持有锁。
        unsafe {
            match previous {
                Some(value) => std::env::set_var("DSH_HOME", value),
                None => std::env::remove_var("DSH_HOME"),
            }
        }
        listed
    }

    /// 插件清单必须同时说清「声明了什么」和「实际装了什么」，并且内置 bundle 要
    /// 单独标出来——它们不在依赖里，也不能被卸载。
    #[test]
    fn profile_plugins_report_declared_installed_and_builtin() {
        let root = std::env::temp_dir().join(format!(
            "smelt-dsh-plugins-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        let profile = root.join("profiles").join("web");
        std::fs::create_dir_all(&profile).expect("create profile dir");
        std::fs::write(
            profile.join("package.json"),
            r#"{
              "dependencies": {
                "@smelt-ai/dsh-acp-rich": "^0.1.2",
                "@vendor/declared-but-missing": "^2.0.0"
              },
              "dsh": { "profile": { "bundles": [
                "@deepseek-ai/dsh-base",
                "@smelt-ai/dsh-acp-rich"
              ] } }
            }"#,
        )
        .expect("write manifest");
        let installed = profile
            .join("node_modules")
            .join("@smelt-ai")
            .join("dsh-acp-rich");
        std::fs::create_dir_all(&installed).expect("create node_modules");
        std::fs::write(
            installed.join("package.json"),
            r#"{"version":"0.1.2","description":"Smelt host"}"#,
        )
        .expect("write installed manifest");

        let plugins = plugins_under(&root, "web").expect("list plugins");
        std::fs::remove_dir_all(&root).ok();

        let host = plugins
            .iter()
            .find(|plugin| plugin.name == "@smelt-ai/dsh-acp-rich")
            .expect("host listed");
        assert_eq!(host.installed.as_deref(), Some("0.1.2"));
        assert_eq!(host.requested.as_deref(), Some("^0.1.2"));
        assert!(host.bundled && !host.builtin && host.is_active());

        // 声明了却没装：这正是「add 报成功但其实没装上」的现场，不能显示成已启用。
        let missing = plugins
            .iter()
            .find(|plugin| plugin.name == "@vendor/declared-but-missing")
            .expect("declared dependency listed");
        assert!(missing.installed.is_none());
        assert!(!missing.bundled && !missing.is_active());

        let builtin = plugins
            .iter()
            .find(|plugin| plugin.name == "@deepseek-ai/dsh-base")
            .expect("builtin bundle listed");
        assert!(builtin.builtin && builtin.is_active());
        assert!(builtin.requested.is_none());

        // Smelt Host 是 ACP 的唯一通道：可以更新，但不能出现卸载入口。
        assert!(host.is_essential() && !host.is_removable());
        assert!(!builtin.is_removable());
        assert!(missing.is_removable());
    }

    /// 装了纯 Web UI 插件（如 dshmarket）不会报错，agent 侧照常生效，但它的界面
    /// 在 Smelt 的原生窗口里不会出现。不标出来用户就会以为是装坏了。
    #[test]
    fn web_ui_plugins_are_marked_so_their_missing_ui_is_not_mistaken_for_breakage() {
        let root = std::env::temp_dir().join(format!(
            "smelt-dsh-webui-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        let profile = root.join("profiles").join("web");
        std::fs::create_dir_all(&profile).expect("create profile dir");
        std::fs::write(
            profile.join("package.json"),
            r#"{
              "dependencies": { "dshmarket": "1.15.0", "@vendor/agent-only": "^1.0.0" },
              "dsh": { "profile": { "bundles": ["dshmarket", "@vendor/agent-only"] } }
            }"#,
        )
        .expect("write manifest");
        for (name, manifest) in [
            (
                "dshmarket",
                r#"{"version":"1.15.0","dsh":{"client":{"platform":"web"}}}"#,
            ),
            ("@vendor/agent-only", r#"{"version":"1.0.0","dsh":{}}"#),
        ] {
            let dir = profile.join("node_modules").join(name);
            std::fs::create_dir_all(&dir).expect("create node_modules");
            std::fs::write(dir.join("package.json"), manifest).expect("write installed manifest");
        }

        let plugins = plugins_under(&root, "web").expect("list plugins");
        std::fs::remove_dir_all(&root).ok();

        let market = plugins
            .iter()
            .find(|plugin| plugin.name == "dshmarket")
            .expect("web ui plugin listed");
        // 它照样是装好且生效的，只是界面那一半在 Smelt 里看不到。
        assert!(market.web_ui && market.is_active());
        assert!(market.status().contains("Smelt 中不显示"));

        let agent_only = plugins
            .iter()
            .find(|plugin| plugin.name == "@vendor/agent-only")
            .expect("agent-only plugin listed");
        assert!(!agent_only.web_ui);
        assert_eq!(agent_only.status(), "已启用");
    }

    /// 界面藏起按钮还不够——卸载只有这一个出口，它自己必须会拒绝。
    #[test]
    fn removing_smelt_host_is_refused_before_touching_the_profile() {
        for name in [
            SMELT_HOST_PACKAGE,
            &format!("{SMELT_HOST_PACKAGE}@0.1.2"),
            &format!("  {SMELT_HOST_PACKAGE}  "),
        ] {
            let error = dsh_plugin_remove("web", name).expect_err("must refuse");
            assert!(
                error.contains("只能更新，不能卸载"),
                "unexpected error for {name}: {error}"
            );
        }
    }
}

#[cfg(test)]
mod dsh_launch_tests {
    use crate::agent_kind::{AcpProfile, ConversationAgentKind, default_acp_dsh_cmd};

    #[test]
    fn dsh_default_cmd_does_not_guess_a_native_profile() {
        let cmd = default_acp_dsh_cmd();
        assert_eq!(cmd, "dsh-acp-rich");
    }

    #[test]
    fn native_dsh_profile_uses_profile_name_instead_of_a_workspace_env() {
        let profile = AcpProfile {
            id: "dsh-native-team".into(),
            kind_id: "dsh".into(),
            label: "DeepSeek Harness · team".into(),
            workspace_dir: "team".into(),
        };
        let launch = profile.launch_spec().expect("原生 DSH profile");
        assert!(launch.command.contains("--profile team"));
        assert!(launch.env.is_empty());
    }

    #[test]
    fn dsh_descriptor_uses_the_dsh_default_cmd() {
        assert_eq!(
            (ConversationAgentKind::Dsh.descriptor().default_cmd)(),
            default_acp_dsh_cmd(),
            "描述符没接到 dsh 的默认命令"
        );
    }
}

#[cfg(test)]
mod dsh_settings_tests {
    use super::{DshDefaultModel, dsh_settings_path_at, parse_dsh_default_model};

    #[test]
    fn reads_the_default_model_from_the_native_yaml_document() {
        let model = parse_dsh_default_model(
            "ui-onboarding:\n  welcomeNoticeVersion: 2026-08-13.1\nagent-default-model:\n  provider: deepseek-official\n  model: deepseek-v4-flash\n  reasoningEffort: max\n",
        )
        .expect("valid DSH settings");

        assert_eq!(
            model,
            Some(DshDefaultModel {
                provider: Some("deepseek-official".into()),
                model: Some("deepseek-v4-flash".into()),
                reasoning_effort: Some("max".into()),
            })
        );
    }

    #[test]
    fn does_not_mistake_invalid_yaml_for_an_empty_model_configuration() {
        assert!(parse_dsh_default_model("agent-default-model: [").is_err());
    }

    #[test]
    fn prefers_the_standard_yaml_document_over_a_stale_json_file() {
        let home =
            std::env::temp_dir().join(format!("smelt-dsh-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("create temporary DSH home");
        std::fs::write(home.join("settings.json"), "{}").expect("write legacy settings");
        std::fs::write(home.join("settings.yaml"), "agent-default-model: {}")
            .expect("write native settings");

        assert_eq!(dsh_settings_path_at(&home), home.join("settings.yaml"));

        std::fs::remove_dir_all(home).expect("remove temporary DSH home");
    }
}

#[cfg(test)]
mod dsh_profile_bundle_tests {
    use super::manifest_declares_bundle;

    #[test]
    fn requires_the_smelt_bundle_to_be_registered_in_the_profile_manifest() {
        let unregistered = serde_json::json!({
            "dependencies": { "@smelt-ai/dsh-acp-rich": "^0.1.0" },
            "dsh": { "profile": { "bundles": ["@deepseek-ai/dsh-base"] } }
        });
        let registered = serde_json::json!({
            "dsh": {
                "profile": {
                    "bundles": ["@deepseek-ai/dsh-base", "@smelt-ai/dsh-acp-rich"]
                }
            }
        });

        assert!(!manifest_declares_bundle(
            &unregistered,
            "@smelt-ai/dsh-acp-rich"
        ));
        assert!(manifest_declares_bundle(
            &registered,
            "@smelt-ai/dsh-acp-rich"
        ));
    }
}

#[cfg(test)]
mod dsh_cli_tests {
    use super::*;

    #[test]
    fn prefers_a_global_dsh_binary() {
        let cli = resolve_dsh_cli(
            Some(std::path::PathBuf::from("/opt/homebrew/bin/dsh")),
            Some(std::path::PathBuf::from("/usr/bin/npx")),
        )
        .expect("global dsh is enough");
        assert!(!cli.is_npx_fallback());
        assert_eq!(
            cli.program,
            std::path::PathBuf::from("/opt/homebrew/bin/dsh")
        );
        assert!(cli.prefix_args.is_empty());
    }

    #[test]
    fn falls_back_to_pinned_npx_when_dsh_is_missing() {
        let cli = resolve_dsh_cli(None, Some(std::path::PathBuf::from("/usr/bin/npx")))
            .expect("npx is enough to start dsh");
        assert!(cli.is_npx_fallback());
        assert_eq!(cli.program, std::path::PathBuf::from("/usr/bin/npx"));
        assert_eq!(
            cli.prefix_args,
            vec![
                "--yes".to_string(),
                format!("{DSH_CLI_PACKAGE}@{DSH_CLI_VERSION}")
            ]
        );
        assert_eq!(
            cli.npx_spec(),
            format!("npx {DSH_CLI_PACKAGE}@{DSH_CLI_VERSION}")
        );
    }

    #[test]
    fn errors_when_neither_dsh_nor_npx_exists() {
        let error = resolve_dsh_cli(None, None).expect_err("must fail closed");
        assert!(error.contains("which npx"), "{error}");
        assert!(error.contains("登录 shell"), "{error}");
    }

    #[test]
    fn npx_shim_execs_the_pinned_package_as_dsh() {
        let cli = resolve_dsh_cli(
            None,
            Some(std::path::PathBuf::from("/Users/me/.volta/bin/npx")),
        )
        .expect("npx fallback");
        let dir = std::env::temp_dir().join(format!(
            "smelt-dsh-npx-shim-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        write_dsh_npx_shim(&dir, &cli).expect("write shim");
        let script = std::fs::read_to_string(dir.join("dsh")).expect("read shim");
        std::fs::remove_dir_all(&dir).ok();

        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("/Users/me/.volta/bin/npx"));
        assert!(script.contains(&format!("{DSH_CLI_PACKAGE}@{DSH_CLI_VERSION}")));
        assert!(
            script.contains("\"$@\""),
            "must forward dsh 的参数：{script}"
        );
    }
}

/// 用户的 Node 从哪来（nvm/volta/fnm/asdf/mise/homebrew）决定了插件能不能装上，
/// 而失败信息决定了用户能不能自己修好。这一组锁死后者。
#[cfg(test)]
mod toolchain_tests {

    /// dsh 只会说一句 "pnpm not found on PATH"，看不出该去哪装、也看不出跟自己
    /// 用的版本管理器有什么关系——volta 的 pnpm 支持还是个要先开开关的实验特性。
    #[test]
    fn missing_pnpm_is_translated_into_an_actionable_hint() {
        let hint = super::toolchain_hint(
            "dsh: pnpm not found on PATH — install pnpm to manage profile plugins",
        )
        .expect("缺 pnpm 必须给出指引");
        assert!(
            hint.contains("npm install -g pnpm"),
            "要给出通用装法：{hint}"
        );
        assert!(
            hint.contains("VOLTA_FEATURE_PNPM"),
            "volta 用户装 pnpm 需要先开实验开关，不提会让他们卡在 `volta install pnpm` 报错上：{hint}"
        );
        assert!(hint.contains("pnpm not found"), "不能吞掉原始输出：{hint}");
    }

    /// volta 的 shim 在 GUI 进程里最典型的失败就是缺 VOLTA_HOME。它是环境问题，
    /// 不是"没装"，指引必须把用户引向 shell 配置文件而不是重装 node。
    #[test]
    fn missing_volta_home_is_translated_into_an_actionable_hint() {
        let hint = super::toolchain_hint("Volta error: Requires VOLTA_HOME to be set")
            .expect("缺 VOLTA_HOME 必须给出指引");
        assert!(hint.contains("VOLTA_HOME"), "{hint}");
        assert!(
            hint.contains(".zshrc"),
            "要指向 shell 配置文件而不是当前终端：{hint}"
        );
    }

    /// 设置页只展示错误的最后几行（`condense_plugin_error`）。指引若放在开头，
    /// 会被那次截断整段丢掉，用户看到的仍然是那句看不懂的英文——所以指引必须
    /// 落在末尾，且短到能活过截断。
    #[test]
    fn hint_survives_being_truncated_to_the_last_three_lines() {
        let noisy = (0..40)
            .map(|i| format!("progress line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\ndsh: pnpm not found on PATH";
        let hint = super::toolchain_hint(&noisy).expect("缺 pnpm 必须给出指引");
        let tail = hint
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .rev()
            .take(3)
            .collect::<Vec<_>>();
        let tail = tail.join("\n");
        assert!(
            tail.contains("npm install -g pnpm") && tail.contains("VOLTA_FEATURE_PNPM"),
            "指引要能活过截断，实际留下的是：\n{tail}"
        );
    }

    /// volta 用户走的是**另一条**报错口径，而且它一度没被匹配上。
    ///
    /// volta 在 `~/.volta/bin` 铺了 pnpm shim，文件真实存在 → dsh 的 spawn 不报
    /// ENOENT → dsh 那句 "pnpm not found on PATH" 根本不会打印，只留下这句 volta
    /// 原文。下面两条字符串是真机跑 volta 2.0.2 抓下来的，不是照文档猜的。
    #[test]
    fn volta_own_wording_for_missing_pnpm_is_also_recognized() {
        let real = "Volta error: Could not find executable \"pnpm\"\n\n\
Use `volta install` to add a package to your toolchain (see `volta help install` for more info).";
        let hint = super::toolchain_hint(real).expect("volta 的报错口径必须也能命中");
        assert!(
            hint.contains("VOLTA_FEATURE_PNPM"),
            "volta 用户最需要的就是这个开关：{hint}"
        );
    }

    /// volta 找不到 Node 时同样不提 VOLTA_HOME 三个字，只说 "Node is not
    /// available"——按变量名去匹配会漏掉真实场景。
    #[test]
    fn volta_wording_for_missing_node_is_recognized() {
        let real = "Volta error: Node is not available.\n\n\
To run any Node command, first set a default version using `volta install node`";
        let hint = super::toolchain_hint(real).expect("缺 Node 的 volta 报错必须命中");
        assert!(hint.contains("volta install node"), "{hint}");
        assert!(hint.contains(".zshrc"), "要指向 shell 配置文件：{hint}");
    }

    /// 普通失败（包不存在、404）不能被套上工具链的帽子，否则真正的原因被指引淹没。
    #[test]
    fn ordinary_failures_are_not_mistaken_for_toolchain_problems() {
        assert!(
            super::toolchain_hint(
                "ERR_PNPM_FETCH_404  GET https://registry.npmjs.org/nope: Not Found - 404"
            )
            .is_none()
        );
        // 这条也不该命中：找不到的是别的可执行文件，跟 pnpm 无关。
        assert!(super::toolchain_hint("Volta error: Could not find executable \"yarn\"").is_none());
        assert!(super::toolchain_hint("").is_none());
    }

    /// 安装必须有上限。没有上限时"网络连不上"和"正在下载"在界面上是同一个状态，
    /// 用户只能去杀进程——这正是本次修复要消灭的"卡住"。
    #[test]
    fn plugin_command_has_a_timeout_ceiling() {
        let timeout = super::PLUGIN_COMMAND_TIMEOUT;
        assert!(
            timeout.as_secs() >= 300,
            "太短会误杀慢网络下正常的安装：{timeout:?}"
        );
        assert!(timeout.as_secs() <= 1800, "太长等于没有上限：{timeout:?}");
    }

    /// 找不到工具链时不能报"某某目录不存在"——用户的 node 可能来自 nvm/volta/
    /// fnm/asdf/mise/homebrew，路径各不相同。要说清查找依据，让用户能自己复现。
    #[test]
    fn toolchain_missing_hint_points_at_login_shell_not_a_guessed_path() {
        let hint = super::TOOLCHAIN_MISSING_HINT;
        assert!(hint.contains("which npx"), "要给出可自查的命令：{hint}");
        assert!(hint.contains("登录 shell"), "要说清查找依据：{hint}");
        for guessed in ["~/.nvm", "~/.volta", "/usr/local/bin/node"] {
            assert!(
                !hint.contains(guessed),
                "不该猜具体安装路径（{guessed}），管理器各不相同：{hint}"
            );
        }
    }
}

#[cfg(test)]
mod model_capability_tests {
    use super::*;

    /// `dsh-model-capabilities --profile web` 在一台真实机器上的原样输出。
    ///
    /// 用原文而不是手写的最小 JSON：这是跨进程契约，两边分别演进，只有真实报文
    /// 才能在桥改了字段名时让这里红掉。这份报文恰好覆盖了全部两种情况——直连
    /// 广告四档，中转一档都不广告。
    const REAL_REPORT: &str = r#"{"providers":[{"id":"deepseek-official","name":"DeepSeek","models":[{"id":"deepseek-v4-flash","name":"DeepSeek-V4-Flash","efforts":[{"id":"off","name":"Off"},{"id":"low","name":"Low"},{"id":"high","name":"High"},{"id":"max","name":"Max"}],"defaultEffort":"high"},{"id":"deepseek-v4-pro","name":"DeepSeek-V4-Pro","efforts":[{"id":"off","name":"Off"},{"id":"low","name":"Low"},{"id":"high","name":"High"},{"id":"max","name":"Max"}],"defaultEffort":"high"}]},{"id":"sub2api","name":"Sub2Api","models":[{"id":"deepseek-v4-flash","name":"Deepseek-V4-Flash"},{"id":"deepseek-v4-pro","name":"Deepseek-V4-Pro"}]}]}"#;

    fn report() -> DshModelCapabilities {
        serde_json::from_str(REAL_REPORT).expect("real capability report must parse")
    }

    #[test]
    fn direct_route_reports_the_efforts_it_advertises() {
        let efforts = report()
            .efforts("deepseek-official", "deepseek-v4-pro")
            .expect("advertised efforts")
            .to_vec();
        assert_eq!(
            efforts.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["off", "low", "high", "max"]
        );
        assert_eq!(
            report()
                .model("deepseek-official", "deepseek-v4-pro")
                .and_then(|row| row.default_effort.clone())
                .as_deref(),
            Some("high")
        );
    }

    /// 这条断言是整件事的支点。中转路由解析得好好的、模型也在，就是没有 efforts
    /// 字段——界面必须据此不给选择。一旦这里退化成"缺省即四档"，用户又会拿到一个
    /// 发一次失败一次的 `max`。
    #[test]
    fn relay_route_advertising_no_reasoning_yields_no_efforts() {
        assert!(report().model("sub2api", "deepseek-v4-pro").is_some());
        assert!(report().efforts("sub2api", "deepseek-v4-pro").is_none());
    }

    #[test]
    fn a_model_outside_the_report_is_not_invented() {
        assert!(report().efforts("deepseek-official", "unlisted").is_none());
        assert!(
            report()
                .efforts("no-such-provider", "deepseek-v4-pro")
                .is_none()
        );
    }

    /// 空列表和字段缺席都表示"没有旋钮"；只认后者会漏掉这一种。
    #[test]
    fn an_empty_effort_list_is_treated_as_no_efforts() {
        let capabilities: DshModelCapabilities = serde_json::from_str(
            r#"{"providers":[{"id":"p","models":[{"id":"m","efforts":[]}]}]}"#,
        )
        .expect("parse");
        assert!(capabilities.model("p", "m").is_some());
        assert!(capabilities.efforts("p", "m").is_none());
    }

    /// 模型列表要能直接喂给选择器：ID 用来存盘，展示名用来显示。
    #[test]
    fn models_expose_ids_and_display_names_for_selection() {
        let report = report();
        let models = report.models("deepseek-official");
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["deepseek-v4-flash", "deepseek-v4-pro"]
        );
        assert_eq!(
            models.iter().map(|m| m.label()).collect::<Vec<_>>(),
            ["DeepSeek-V4-Flash", "DeepSeek-V4-Pro"]
        );
    }

    /// 报告里没有的 provider 返回空表，调用方据此不画选择器——而不是崩掉或凭空
    /// 造一个候选出来。
    #[test]
    fn an_unknown_provider_offers_no_models() {
        assert!(report().models("no-such-provider").is_empty());
    }

    /// 老桥（或未来精简过的报告）不带展示名时退回 ID，绝不显示成空白按钮。
    #[test]
    fn a_model_without_a_display_name_falls_back_to_its_id() {
        let capabilities: DshModelCapabilities = serde_json::from_str(
            r#"{"providers":[{"id":"p","models":[{"id":"m"},{"id":"n","name":"  "}]}]}"#,
        )
        .expect("parse");
        assert_eq!(
            capabilities
                .models("p")
                .iter()
                .map(|m| m.label())
                .collect::<Vec<_>>(),
            ["m", "n"]
        );
    }
}

#[cfg(test)]
mod node_stderr_tests {
    use super::condense_node_stderr;

    /// `dsh-model-settings --action save-custom` 收到空 models 时的**原样** stderr。
    ///
    /// 用真实报文而不是手写：这条链路的价值全在"Node 到底吐什么"，手写会把我
    /// 以为的格式固化下来，真格式变了也测不出。
    const REAL_STDERR: &str = r#"file:///Users/x/plugins/dsh-acp-rich/bin/dsh-model-settings.mjs:280
    throw new Error('dsh-model-settings: a custom provider requires at least one model')
          ^

Error: dsh-model-settings: a custom provider requires at least one model
    at requiredModels (file:///Users/x/plugins/dsh-acp-rich/bin/dsh-model-settings.mjs:280:11)
    at file:///Users/x/plugins/dsh-acp-rich/bin/dsh-model-settings.mjs:85:18
    at process.processTicksAndRejections (node:internal/process/task_queues:103:5)

Node.js v22.23.2
"#;

    /// 这条是重点：原先取末尾三行，恰好只剩内部栈帧和 Node 版本号，唯一有用的
    /// 那句话被整条调用栈挤掉了。
    #[test]
    fn the_real_reason_survives_instead_of_the_stack_frames() {
        let message = condense_node_stderr(REAL_STDERR);
        assert_eq!(
            message,
            "dsh-model-settings: a custom provider requires at least one model"
        );
        assert!(!message.contains("processTicksAndRejections"));
        assert!(!message.contains("Node.js v"));
    }

    /// 抛在源码里的那行 `throw new Error('…')` 长得也很像，但它不是 Node 的错误
    /// 行；认错了就会把源码片段当成原因显示出来。
    #[test]
    fn the_echoed_source_line_is_not_mistaken_for_the_reason() {
        assert_eq!(
            condense_node_stderr(REAL_STDERR),
            "dsh-model-settings: a custom provider requires at least one model"
        );
        assert_eq!(
            condense_node_stderr("    throw new Error('boom')\n\nTypeError: bad input\n"),
            "bad input"
        );
    }

    /// 认不出格式就原样给出去：宁可啰嗦，也不能把唯一的线索吞成"原因未知"。
    #[test]
    fn unrecognised_output_is_passed_through() {
        assert_eq!(
            condense_node_stderr("dsh: command not found"),
            "dsh: command not found"
        );
    }

    #[test]
    fn empty_output_says_so_rather_than_showing_nothing() {
        assert_eq!(condense_node_stderr("   \n\n"), "执行失败，原因未知");
    }
}

#[cfg(test)]
mod model_discovery_tests {
    use super::*;

    /// `dsh-discover-models` 问一台真实网关得到的**原样**输出（截取前几条）。
    const REAL_REPORT: &str = r#"{"models":[{"id":"codex-auto-review","name":"codex-auto-review"},{"id":"glm-5.2","contextWindow":128000,"maxTokens":8192},{"id":"gpt-5.3-codex","name":"gpt-5.3-codex"}]}"#;

    #[test]
    fn a_real_listing_parses_into_editable_rows() {
        let report: DshDiscoveryReport =
            serde_json::from_str(REAL_REPORT).expect("real listing must parse");
        assert_eq!(report.models.len(), 3);
        let glm = &report.models[1];
        assert_eq!(glm.id, "glm-5.2");
        // 端点没给展示名就退回 ID——表单那一格不能显示成空白。
        assert_eq!(glm.label(), "glm-5.2");
        assert_eq!(glm.context_window, Some(128_000));
        assert_eq!(glm.max_tokens, Some(8192));
        // 端点没披露容量时字段缺席，不能变成 0：0 会被表单当成"填了个非法值"。
        assert_eq!(report.models[0].context_window, None);
        assert_eq!(report.models[0].max_tokens, None);
    }

    /// 请求里没填的字段必须**整个不出现**。桥把空串也当成"没填"，但让空值走到
    /// 线上本身就是错的：一个空的 `baseURL` 会被助手当成一个要去问的端点。
    #[test]
    fn an_unset_field_is_omitted_from_the_request() {
        let request = DshDiscoveryRequest {
            provider: Some("sub2api".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&request).expect("serialize");
        assert_eq!(json, r#"{"provider":"sub2api"}"#);
    }

    /// 字段名要和助手的 `LlmModelDiscoveryRequest` 对上；写成 snake_case 就会被
    /// 静默忽略，表现为"发现总是问不到东西"。
    #[test]
    fn the_request_uses_the_field_names_the_harness_reads() {
        let request = DshDiscoveryRequest {
            provider: None,
            base_url: Some("https://g.example/v1".into()),
            api: Some("openai-responses".into()),
            api_key: Some("sk-x".into()),
        };
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(
            json.contains(r#""baseURL":"https://g.example/v1""#),
            "{json}"
        );
        assert!(json.contains(r#""apiKey":"sk-x""#), "{json}");
        assert!(!json.contains("base_url"), "{json}");
    }

    #[test]
    fn an_endpoint_that_advertises_nothing_parses_as_empty() {
        let report: DshDiscoveryReport = serde_json::from_str(r#"{"models":[]}"#).expect("parse");
        assert!(report.models.is_empty());
    }
}

#[cfg(test)]
mod smelt_host_version_tests {
    use super::smelt_host_version_is_supported;

    /// 更新拉的是 registry 的 latest，够不到的版本 Smelt 管不着——所以补丁版要放
    /// 行（那正是不必先升 Smelt 的那一类），破坏兼容的大版本要认出来。
    #[test]
    fn accepts_patch_releases_and_flags_the_breaking_ones() {
        assert!(smelt_host_version_is_supported("0.1.7"));
        assert!(smelt_host_version_is_supported("0.1.99"));
        assert!(!smelt_host_version_is_supported("0.2.0"));
        assert!(!smelt_host_version_is_supported("0.1.6"));
    }

    /// 看不懂的版本号不报警：宁可漏一次提示，也不要对着一个我们没解析出来的字符串
    /// 吓唬用户。
    #[test]
    fn stays_quiet_on_unparseable_versions() {
        assert!(smelt_host_version_is_supported("不是版本"));
        assert!(smelt_host_version_is_supported(""));
    }
}

#[cfg(test)]
mod plugin_update_live_tests {
    /// 真机联调：确认 `pnpm view` 那一行确实能被解析成版本号。
    ///
    /// 默认 `#[ignore]`——要联网、要本机装好 profile。但它守的东西没法用单测覆盖：
    /// 更新流程的第一步就是把 pnpm 的输出解析成一个能点名的版本，解析歪了整条更新
    /// 就是空转。
    #[test]
    #[ignore = "requires a real dsh profile and network access"]
    fn reads_the_latest_version_from_the_registry() {
        let profile = std::env::var("SMELT_TEST_DSH_PROFILE").unwrap_or_else(|_| "web".into());
        let version =
            super::dsh_plugin_latest_version(&profile, super::SMELT_HOST_PACKAGE).expect("view");
        assert!(
            semver::Version::parse(&version).is_ok(),
            "解析出来的不是版本号：{version}"
        );
    }
}

#[cfg(test)]
mod model_capability_live_tests {
    /// 真机联调：起一次真实的 dsh 运行时，确认整条链在**贫瘠环境**下也能走通。
    ///
    /// 默认 `#[ignore]`，因为它要求本机装好 profile 和 0.1.7 的桥，还要花几秒。
    /// 但它是唯一能覆盖 login shell 环境注入的测试：从终端跑 cargo 时环境本来就
    /// 是全的，只有把 PATH 清成 GUI 那样才验得到——
    ///
    /// ```sh
    /// env -i HOME="$HOME" PATH=/usr/bin:/bin \
    ///   cargo test -p smelt-core model_capability_live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires a real dsh profile with the 0.1.7 bridge installed"]
    fn queries_a_real_profile() {
        let profile = std::env::var("SMELT_TEST_DSH_PROFILE").unwrap_or_else(|_| "web".into());
        let report = super::dsh_model_capabilities(&profile).expect("capability query");
        assert!(
            !report.providers.is_empty(),
            "a configured profile must advertise at least one route"
        );
        for provider in &report.providers {
            for model in &provider.models {
                let efforts = report
                    .efforts(&provider.id, &model.id)
                    .map(|efforts| efforts.iter().map(|e| e.id.as_str()).collect::<Vec<_>>());
                println!("{}/{} -> {efforts:?}", provider.id, model.id);
            }
        }
    }

    /// 真机联调：问一个真实端点它提供哪些模型。
    ///
    /// 这条链比能力查询多一次网络往返，所以除了 profile 还要求那个端点此刻可达、
    /// 凭据有效。用环境变量指定要问谁，默认问已配置的路由——那条路由的凭据 dsh
    /// 自己会取，不需要测试持有密钥。
    ///
    /// ```sh
    /// env -i HOME="$HOME" PATH=/usr/bin:/bin SMELT_TEST_DSH_DISCOVER_PROVIDER=sub2api \
    ///   cargo test -p smelt-core discovers_a_real_endpoint -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires a reachable endpoint and a configured credential"]
    fn discovers_a_real_endpoint() {
        let profile = std::env::var("SMELT_TEST_DSH_PROFILE").unwrap_or_else(|_| "web".into());
        let Ok(provider) = std::env::var("SMELT_TEST_DSH_DISCOVER_PROVIDER") else {
            eprintln!("set SMELT_TEST_DSH_DISCOVER_PROVIDER to a configured custom provider");
            return;
        };
        let request = super::DshDiscoveryRequest {
            provider: Some(provider.clone()),
            base_url: std::env::var("SMELT_TEST_DSH_DISCOVER_BASE_URL").ok(),
            api: std::env::var("SMELT_TEST_DSH_DISCOVER_API").ok(),
            api_key: None,
        };
        let report = super::dsh_discover_models(&profile, &request).expect("discovery");
        println!("{provider} -> {} models", report.models.len());
        for model in &report.models {
            println!("  {} ({})", model.id, model.label());
        }
        assert!(
            !report.models.is_empty(),
            "a reachable listing endpoint must advertise at least one model"
        );
    }
}
