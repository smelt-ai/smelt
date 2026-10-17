//! Pi 插件（技能、扩展）的数据和目录位置。扫描、导入、删除在运行时。

use std::path::PathBuf;

/// 插件类别。决定注入时用哪个 Pi 启动参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PiPluginKind {
    Skill,
    Extension,
}

impl PiPluginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Extension => "extension",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Skill => "技能",
            Self::Extension => "扩展",
        }
    }

    /// Pi 用来显式加载单个资源的参数名。`--no-skills` 之后 `--skill` 仍然生效，
    /// extension 同理走 `-e`。
    pub fn load_flag(self) -> &'static str {
        match self {
            Self::Skill => "--skill",
            Self::Extension => "-e",
        }
    }

    /// 关闭自动发现的参数。勾选式加载必须先关掉它，否则全局的还是会进来。
    pub fn disable_discovery_flag(self) -> &'static str {
        match self {
            Self::Skill => "--no-skills",
            Self::Extension => "--no-extensions",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "skill" => Some(Self::Skill),
            "extension" => Some(Self::Extension),
            _ => None,
        }
    }
}

/// 一个可勾选的插件。`id` 是写进智能体配置的稳定标识。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PiPlugin {
    pub id: String,
    pub kind: PiPluginKind,
    /// 展示名。skill 取 frontmatter 的 `name`，缺失时回退目录名。
    pub name: String,
    pub description: String,
    /// 传给 Pi 的路径，也是删除时要动的路径。
    pub path: PathBuf,
    /// 发现来源，用于在界面上说明这个插件是哪来的。
    pub origin: String,
    /// 失效原因。软链断了之类的坏条目要列出来让用户能删，而不是静默藏起来。
    pub broken: Option<String>,
}

impl PiPlugin {
    pub fn is_usable(&self) -> bool {
        self.broken.is_none()
    }
}

/// 插件 id 的稳定编码：`skill:<名字>` / `extension:<名字>`。
pub fn plugin_id(kind: PiPluginKind, name: &str) -> String {
    format!("{}:{}", kind.as_str(), name)
}

pub fn split_plugin_id(id: &str) -> Option<(PiPluginKind, &str)> {
    let (kind, name) = id.split_once(':')?;
    let kind = PiPluginKind::parse(kind)?;
    if name.is_empty() {
        return None;
    }
    Some((kind, name))
}

/// Pi 的全局 skill 目录。两个位置都是官方发现路径，`--no-skills` 会一起关掉。
pub fn global_skill_roots() -> Vec<PathBuf> {
    let mut roots = vec![crate::pi_model_settings::pi_agent_dir().join("skills")];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".agents").join("skills"));
    }
    roots
}

/// 工作区自带技能包的约定目录名，相对于绑定目录或智能体 space 根。
///
/// `.agents/skills` 是通用 Agent Skills 位置，也是 Pi 原生支持的位置；`.pi/skills` 是
/// Pi 原生项目级位置。`.claude/skills` 是 Smelt 为绑定目录提供的兼容入口，会通过显式
/// `--skill` 加载；Pi 本身不会从该位置自动发现。
pub const WORKSPACE_SKILL_DIRS: &[&str] = &[".agents/skills", ".claude/skills", ".pi/skills"];

pub fn global_extension_root() -> PathBuf {
    crate::pi_model_settings::pi_agent_dir().join("extensions")
}
