//! 智能体插件目录：枚举 Pi 会话可加载的 skill 与 extension。
//!
//! Pi 没有内置 MCP，可插拔的是 skill（Agent Skills 能力包）和 extension
//! （注册工具/命令的 TS 模块）。裸 Pi 会自动发现全局资源及受信任项目中的原生资源；
//! Smelt 的产品智能体则用勾选式加载：先关掉自动发现，再显式传入勾选插件和工作区技能。
//!
//! `discover_plugins` 只扫供产品智能体勾选的全局插件目录。智能体 space 和绑定目录里的
//! 技能由 `workspace_skill_args` 单独显式加载，避免依赖 Pi 的 cwd / 项目信任推断。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agent_kind::{ConversationLaunchSpec, SMELT_AGENT_PLUGIN_ARGS_ENV};

/// 插件类别。决定注入时用哪个 Pi 启动参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// 把工作区自带的技能目录换成 Pi 的显式加载参数。
///
/// 为什么必须显式加载：会话 cwd 是智能体自己的 space，绑定目录压根不是 cwd，
/// Pi 的项目级发现扫不到；而且勾选式加载已经下发了 `--no-skills` 关掉全部自动
/// 发现。`--skill` 在 `--no-skills` 之后仍然生效，且接受目录并递归找 `SKILL.md`，
/// 所以整包传目录就够了，不用逐个枚举。
pub fn workspace_skill_args(roots: &[PathBuf]) -> Vec<String> {
    let mut args = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for root in roots {
        for relative in WORKSPACE_SKILL_DIRS {
            let dir = root.join(relative);
            if !dir.is_dir() {
                continue;
            }
            // 绑定目录可能与 space 重合，或经软链指向同一处；按真实路径去重，
            // 否则同一个技能会被加载两遍。
            let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
            if !seen.insert(key) {
                continue;
            }
            args.push(PiPluginKind::Skill.load_flag().to_string());
            args.push(dir.to_string_lossy().into_owned());
        }
    }
    args
}

pub fn global_extension_root() -> PathBuf {
    crate::pi_model_settings::pi_agent_dir().join("extensions")
}

/// 扫描全部全局位置。同一份插件被多处引用（例如 `~/.pi/agent/skills` 下的软链
/// 指向 `~/.agents/skills`）时按真实路径去重，只保留先发现的那个。
pub fn discover_plugins() -> Vec<PiPlugin> {
    let mut plugins = Vec::new();
    for root in global_skill_roots() {
        collect_skills(&root, &mut plugins);
    }
    collect_extensions(&global_extension_root(), &mut plugins);
    dedupe(plugins)
}

/// 按真实路径去重；坏条目没有真实路径，用 id 兜底避免互相顶掉。
fn dedupe(plugins: Vec<PiPlugin>) -> Vec<PiPlugin> {
    let mut seen = BTreeMap::new();
    let mut out = Vec::new();
    for plugin in plugins {
        let key = std::fs::canonicalize(&plugin.path)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| format!("<unresolved>{}", plugin.id));
        if seen.insert(key, ()).is_none() {
            out.push(plugin);
        }
    }
    out.sort_by_key(|a| (a.kind, a.name.to_lowercase()));
    out
}

fn collect_skills(root: &Path, out: &mut Vec<PiPlugin>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let origin = display_path(root);
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(dir_name) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if dir_name.starts_with('.') {
            continue;
        }
        // 软链断了时 `path.is_dir()` 是 false，但条目确实存在，必须报出来。
        if !path.exists() {
            out.push(PiPlugin {
                id: plugin_id(PiPluginKind::Skill, &dir_name),
                kind: PiPluginKind::Skill,
                name: dir_name,
                description: String::new(),
                path,
                origin: origin.clone(),
                broken: Some("链接目标不存在".to_string()),
            });
            continue;
        }
        if path.is_dir() {
            let manifest = path.join("SKILL.md");
            if !manifest.is_file() {
                continue;
            }
            let (name, description) = read_skill_manifest(&manifest, &dir_name);
            out.push(PiPlugin {
                id: plugin_id(PiPluginKind::Skill, &dir_name),
                kind: PiPluginKind::Skill,
                name,
                description,
                path,
                origin: origin.clone(),
                broken: None,
            });
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            // Pi 只把带合法 frontmatter 的根 md 当技能，没有 description 的忽略。
            let stem = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir_name.clone());
            let (name, description) = read_skill_manifest(&path, &stem);
            if description.is_empty() {
                continue;
            }
            out.push(PiPlugin {
                id: plugin_id(PiPluginKind::Skill, &stem),
                kind: PiPluginKind::Skill,
                name,
                description,
                path,
                origin: origin.clone(),
                broken: None,
            });
        }
    }
}

fn collect_extensions(root: &Path, out: &mut Vec<PiPlugin>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let origin = display_path(root);
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(raw_name) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if raw_name.starts_with('.') {
            continue;
        }
        if !path.exists() {
            out.push(PiPlugin {
                id: plugin_id(PiPluginKind::Extension, &raw_name),
                kind: PiPluginKind::Extension,
                name: raw_name,
                description: String::new(),
                path,
                origin: origin.clone(),
                broken: Some("链接目标不存在".to_string()),
            });
            continue;
        }
        // Pi 认 `extensions/*.ts` 和 `extensions/*/index.ts` 两种形态。
        let (entry_path, name) = if path.is_dir() {
            let index = path.join("index.ts");
            if !index.is_file() {
                continue;
            }
            (path, raw_name)
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("ts") {
            let stem = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| raw_name.clone());
            (path, stem)
        } else {
            continue;
        };
        let description = read_extension_summary(&entry_path);
        out.push(PiPlugin {
            id: plugin_id(PiPluginKind::Extension, &name),
            kind: PiPluginKind::Extension,
            name,
            description,
            path: entry_path,
            origin: origin.clone(),
            broken: None,
        });
    }
}

/// 取 extension 顶部的注释当简介。Pi 的 extension 没有 manifest，只能这样给个人话提示。
fn read_extension_summary(path: &Path) -> String {
    let file = if path.is_dir() {
        path.join("index.ts")
    } else {
        path.to_path_buf()
    };
    let Ok(text) = std::fs::read_to_string(&file) else {
        return String::new();
    };
    for line in text.lines().take(20) {
        let line = line.trim();
        let comment = line
            .strip_prefix("///")
            .or_else(|| line.strip_prefix("//"))
            .or_else(|| line.strip_prefix("*"));
        if let Some(comment) = comment {
            let comment = comment.trim();
            if !comment.is_empty() && !comment.starts_with("eslint") {
                return comment.to_string();
            }
        }
    }
    String::new()
}

fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.to_string_lossy());
    }
    text.into_owned()
}

/// 读 `SKILL.md` 的 YAML frontmatter。只需要 `name` 和 `description` 两个标量，
/// 不值得为此拉一个 YAML 依赖，但必须覆盖块标量，实际技能大量用 `description: |`。
fn read_skill_manifest(path: &Path, fallback_name: &str) -> (String, String) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (fallback_name.to_string(), String::new());
    };
    let fields = parse_frontmatter(&text);
    let name = fields
        .get("name")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_name)
        .to_string();
    let description = fields
        .get("description")
        .map(String::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    (name, description)
}

/// 极小的 frontmatter 解析：顶层标量 + `|`/`>` 块标量 + 引号剥离。
/// 嵌套结构直接跳过——技能清单里用不到，硬撑只会引入误判。
pub fn parse_frontmatter(text: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let mut lines = text.lines();
    // frontmatter 必须是文件第一行的 `---`，否则当作没有。
    if lines.next().map(str::trim) != Some("---") {
        return fields;
    }
    let body: Vec<&str> = lines.collect();
    let mut ix = 0;
    while ix < body.len() {
        let line = body[ix];
        if line.trim() == "---" {
            break;
        }
        let Some((key, rest)) = split_key(line) else {
            ix += 1;
            continue;
        };
        let rest = rest.trim();
        if rest == "|" || rest == ">" || rest == "|-" || rest == ">-" {
            let fold = rest.starts_with('>');
            let mut block = Vec::new();
            ix += 1;
            while ix < body.len() {
                let candidate = body[ix];
                if candidate.trim() == "---" {
                    break;
                }
                // 块标量靠缩进界定；回到零缩进的非空行就说明块结束了。
                let indented = candidate.starts_with(' ') || candidate.starts_with('\t');
                if !candidate.trim().is_empty() && !indented {
                    break;
                }
                block.push(candidate.trim());
                ix += 1;
            }
            let joined = if fold {
                block.join(" ")
            } else {
                block.join("\n")
            };
            fields.insert(key, joined.trim().to_string());
            continue;
        }
        fields.insert(key, strip_quotes(rest));
        ix += 1;
    }
    fields
}

/// 只认零缩进的 `key:`，避免把块标量正文里的冒号行误当成新字段。
fn split_key(line: &str) -> Option<(String, &str)> {
    if line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty() {
        return None;
    }
    let (key, rest) = line.split_once(':')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some((key.to_string(), rest))
}

fn strip_quotes(value: &str) -> String {
    let value = value.trim();
    for quote in ['\'', '"'] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// 从本地路径导入一个插件到 Pi 的全局目录。返回新插件的 id。
pub fn import_plugin(kind: PiPluginKind, source: &Path) -> Result<String, String> {
    if !source.exists() {
        return Err(format!("路径不存在：{}", source.display()));
    }
    let target_root = match kind {
        PiPluginKind::Skill => crate::pi_model_settings::pi_agent_dir().join("skills"),
        PiPluginKind::Extension => global_extension_root(),
    };
    std::fs::create_dir_all(&target_root).map_err(|error| format!("创建目录失败：{error}"))?;

    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| "无法确定插件名".to_string())?;
    match kind {
        PiPluginKind::Skill => {
            if !source.is_dir() {
                return Err("技能必须是包含 SKILL.md 的目录".to_string());
            }
            if !source.join("SKILL.md").is_file() {
                return Err("目录里没有 SKILL.md，不是合法技能".to_string());
            }
        }
        PiPluginKind::Extension => {
            let ok = (source.is_file()
                && source.extension().and_then(|ext| ext.to_str()) == Some("ts"))
                || (source.is_dir() && source.join("index.ts").is_file());
            if !ok {
                return Err("扩展必须是 .ts 文件或包含 index.ts 的目录".to_string());
            }
        }
    }

    let target = target_root.join(&name);
    if target.exists() {
        return Err(format!("已存在同名插件：{name}"));
    }
    if source.is_dir() {
        copy_dir(source, &target)?;
    } else {
        std::fs::copy(source, &target).map_err(|error| format!("拷贝失败：{error}"))?;
    }
    let name = if kind == PiPluginKind::Extension && target.is_file() {
        target
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or(name)
    } else {
        name
    };
    Ok(plugin_id(kind, &name))
}

fn copy_dir(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|error| format!("创建目录失败：{error}"))?;
    let entries = std::fs::read_dir(source).map_err(|error| format!("读取目录失败：{error}"))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = target.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|error| format!("拷贝失败：{error}"))?;
        }
    }
    Ok(())
}

/// 删除插件实体。删的是用户 `~/.pi` 下的真实文件，调用方必须先做二次确认。
pub fn remove_plugin(plugin: &PiPlugin) -> Result<(), String> {
    let path = &plugin.path;
    // 断链的 `is_dir()` 为 false，用 symlink_metadata 才能识别并删掉链接本身。
    let meta = std::fs::symlink_metadata(path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    if meta.file_type().is_symlink() || meta.is_file() {
        std::fs::remove_file(path).map_err(|error| format!("删除失败：{error}"))
    } else {
        std::fs::remove_dir_all(path).map_err(|error| format!("删除失败：{error}"))
    }
}

/// 这场 Pi 会话会加载哪些技能。推断自启动参数，不是 agent 运行时回执。
///
/// 产品智能体把勾选结果写进 `SMELT_AGENT_PLUGIN_ARGS`（先 `--no-skills` 再逐条
/// `--skill`）；没有这份参数时就是裸 Pi，走全局自动发现，并附带 cwd 下的项目级技能。
pub fn loaded_skills_for_launch(
    launch: &ConversationLaunchSpec,
    cwd: Option<&Path>,
) -> Vec<PiPlugin> {
    let mut plugins = match launch
        .env
        .get(SMELT_AGENT_PLUGIN_ARGS_ENV)
        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
    {
        Some(args) => skill_paths_from_plugin_args(&args)
            .into_iter()
            .flat_map(|path| skills_at_load_path(&path))
            .collect(),
        None => {
            let mut plugins = discover_plugins();
            if let Some(cwd) = cwd {
                collect_pi_project_skills(cwd, &mut plugins);
            }
            plugins
        }
    };
    plugins.retain(|plugin| plugin.kind == PiPluginKind::Skill && plugin.is_usable());
    dedupe(plugins)
}

/// 收集 Pi 裸会话按自身约定自动发现的项目技能。
///
/// Pi 从 cwd 到 Git 仓库根（没有仓库时到文件系统根）逐层找 `.agents/skills`，
/// `.pi/skills` 则只从当前工作目录发现。不要把 Smelt 的兼容目录混进来：它们只有在
/// 产品智能体显式传 `--skill` 时才会加载。
fn collect_pi_project_skills(cwd: &Path, out: &mut Vec<PiPlugin>) {
    collect_skills(&cwd.join(".pi/skills"), out);

    let git_root = cwd
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists());
    for ancestor in cwd.ancestors() {
        collect_skills(&ancestor.join(".agents/skills"), out);
        if git_root == Some(ancestor) {
            break;
        }
    }
}

fn skill_paths_from_plugin_args(args: &[String]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == PiPluginKind::Skill.load_flag() {
            if let Some(path) = args.get(index + 1) {
                paths.push(PathBuf::from(path));
                index += 2;
                continue;
            }
        }
        index += 1;
    }
    paths
}

/// `--skill` 既可能指向单个技能目录 / `SKILL.md`，也可能指向整包技能根。
fn skills_at_load_path(path: &Path) -> Vec<PiPlugin> {
    let mut out = Vec::new();
    if path.is_file() {
        collect_skills(path.parent().unwrap_or_else(|| Path::new(".")), &mut out);
        out.retain(|plugin| plugin.path == path);
        return out;
    }
    if path.join("SKILL.md").is_file() {
        let dir_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "skill".to_string());
        let (name, description) = read_skill_manifest(&path.join("SKILL.md"), &dir_name);
        out.push(PiPlugin {
            id: plugin_id(PiPluginKind::Skill, &dir_name),
            kind: PiPluginKind::Skill,
            name,
            description,
            path: path.to_path_buf(),
            origin: display_path(path.parent().unwrap_or(path)),
            broken: None,
        });
        return out;
    }
    collect_skills(path, &mut out);
    out
}

/// 把勾选结果翻译成 Pi 启动参数。
///
/// 关键语义：只要智能体走勾选式加载，就必须先关掉两类自动发现，否则用户
/// `~/.pi` 下的东西还是会全量进来，勾选就成了摆设。Smelt 自己的权限扩展走
/// inline `extensionFactories`，不受 `--no-extensions` 影响。
pub fn launch_args_for_selection(selected: &[String], available: &[PiPlugin]) -> Vec<String> {
    let mut args = vec![
        PiPluginKind::Skill.disable_discovery_flag().to_string(),
        PiPluginKind::Extension.disable_discovery_flag().to_string(),
    ];
    for id in selected {
        let Some(plugin) = available.iter().find(|plugin| &plugin.id == id) else {
            continue;
        };
        if !plugin.is_usable() {
            continue;
        }
        args.push(plugin.kind.load_flag().to_string());
        args.push(plugin.path.to_string_lossy().into_owned());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    /// 绑定目录自带的技能包必须显式传给 Pi：会话 cwd 是智能体 space，绑定目录
    /// 不是 cwd，项目级发现扫不到；而且勾选式加载已经下发了 `--no-skills`。
    #[test]
    fn workspace_skill_directories_become_explicit_load_flags() {
        let root = std::env::temp_dir().join(format!("smelt-ws-skills-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".agents/skills/demo")).unwrap();
        std::fs::create_dir_all(root.join(".claude/skills/demo")).unwrap();
        std::fs::create_dir_all(root.join(".pi/skills/demo")).unwrap();
        std::fs::create_dir_all(root.join(".skills/demo")).unwrap();

        let args = workspace_skill_args(std::slice::from_ref(&root));

        // 顺序跟着约定表走。
        assert_eq!(
            args,
            vec![
                "--skill".to_string(),
                root.join(".agents/skills").to_string_lossy().into_owned(),
                "--skill".to_string(),
                root.join(".claude/skills").to_string_lossy().into_owned(),
                "--skill".to_string(),
                root.join(".pi/skills").to_string_lossy().into_owned(),
            ]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// 同一个目录既是 space 又被当成绑定目录时不能传两遍，否则技能会重复加载。
    #[test]
    fn the_same_skill_directory_is_never_loaded_twice() {
        let root = std::env::temp_dir().join(format!("smelt-ws-dedupe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".claude/skills")).unwrap();

        let args = workspace_skill_args(&[root.clone(), root.clone()]);

        assert_eq!(args.iter().filter(|arg| *arg == "--skill").count(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 没有任何约定目录时不能凭空多出参数。
    #[test]
    fn a_workspace_without_skill_directories_contributes_no_flags() {
        let root = std::env::temp_dir().join(format!("smelt-ws-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();

        assert!(workspace_skill_args(std::slice::from_ref(&root)).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn quoted_and_block_descriptions_both_parse() {
        let quoted =
            parse_frontmatter("---\nname: fund\ndescription: '查询基金: 档案'\n---\n# x\n");
        assert_eq!(quoted.get("name").unwrap(), "fund");
        assert_eq!(quoted.get("description").unwrap(), "查询基金: 档案");

        // 真实技能大量用块标量，正文里还带冒号，不能被当成新字段。
        let block = parse_frontmatter(
            "---\nname: meegle\ndescription: |\n  飞书项目操作工具。\n  关键词：工作项、排期。\n---\n# x\n",
        );
        assert_eq!(block.get("name").unwrap(), "meegle");
        assert_eq!(
            block.get("description").unwrap(),
            "飞书项目操作工具。\n关键词：工作项、排期。"
        );
    }

    #[test]
    fn frontmatter_must_start_the_file() {
        assert!(parse_frontmatter("# 标题\n---\nname: x\n---\n").is_empty());
    }

    #[test]
    fn skill_directories_need_a_manifest() {
        let temp = std::env::temp_dir().join(format!("smelt-skills-{}", uuid::Uuid::new_v4()));
        write(
            &temp.join("good/SKILL.md"),
            "---\nname: good\ndescription: 有效技能\n---\n",
        );
        std::fs::create_dir_all(temp.join("nomanifest")).unwrap();

        let mut found = Vec::new();
        collect_skills(&temp, &mut found);
        let ids: Vec<_> = found.iter().map(|plugin| plugin.id.as_str()).collect();
        assert_eq!(ids, vec!["skill:good"]);
        assert_eq!(found[0].description, "有效技能");

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn broken_symlinks_are_listed_so_they_can_be_removed() {
        let temp = std::env::temp_dir().join(format!("smelt-broken-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        std::os::unix::fs::symlink(temp.join("missing-target"), temp.join("dangling")).unwrap();

        let mut found = Vec::new();
        collect_skills(&temp, &mut found);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "skill:dangling");
        assert!(!found[0].is_usable());

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn selection_disables_discovery_and_loads_only_checked_plugins() {
        let available = vec![
            PiPlugin {
                id: "skill:a".into(),
                kind: PiPluginKind::Skill,
                name: "a".into(),
                description: String::new(),
                path: PathBuf::from("/skills/a"),
                origin: String::new(),
                broken: None,
            },
            PiPlugin {
                id: "skill:b".into(),
                kind: PiPluginKind::Skill,
                name: "b".into(),
                description: String::new(),
                path: PathBuf::from("/skills/b"),
                origin: String::new(),
                broken: None,
            },
            PiPlugin {
                id: "extension:e".into(),
                kind: PiPluginKind::Extension,
                name: "e".into(),
                description: String::new(),
                path: PathBuf::from("/ext/e.ts"),
                origin: String::new(),
                broken: None,
            },
        ];

        let args = launch_args_for_selection(
            &["skill:a".to_string(), "extension:e".to_string()],
            &available,
        );
        assert_eq!(
            args,
            vec![
                "--no-skills",
                "--no-extensions",
                "--skill",
                "/skills/a",
                "-e",
                "/ext/e.ts",
            ]
        );

        // 一个都不勾也必须关掉发现，否则「不加载」会变成「全量加载」。
        let none = launch_args_for_selection(&[], &available);
        assert_eq!(none, vec!["--no-skills", "--no-extensions"]);
    }

    #[test]
    fn unknown_and_broken_selections_are_skipped() {
        let available = vec![PiPlugin {
            id: "skill:gone".into(),
            kind: PiPluginKind::Skill,
            name: "gone".into(),
            description: String::new(),
            path: PathBuf::from("/skills/gone"),
            origin: String::new(),
            broken: Some("链接目标不存在".into()),
        }];
        let args = launch_args_for_selection(
            &["skill:gone".to_string(), "skill:never".to_string()],
            &available,
        );
        assert_eq!(args, vec!["--no-skills", "--no-extensions"]);
    }

    #[test]
    fn plugin_ids_round_trip() {
        let id = plugin_id(PiPluginKind::Skill, "hithink-finance-fund");
        assert_eq!(id, "skill:hithink-finance-fund");
        let (kind, name) = split_plugin_id(&id).unwrap();
        assert_eq!(kind, PiPluginKind::Skill);
        assert_eq!(name, "hithink-finance-fund");
        assert!(split_plugin_id("skill:").is_none());
        assert!(split_plugin_id("mcp:x").is_none());
        assert!(split_plugin_id("noscheme").is_none());
    }

    fn skill_manifest(name: &str, description: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n")
    }

    fn launch_with_plugin_args(args: &[&str]) -> ConversationLaunchSpec {
        let mut launch = ConversationLaunchSpec::from_command("pi");
        launch.env.insert(
            SMELT_AGENT_PLUGIN_ARGS_ENV.into(),
            serde_json::to_string(&args).unwrap(),
        );
        launch
    }

    #[test]
    fn explicit_plugin_args_list_only_selected_skills() {
        let temp =
            std::env::temp_dir().join(format!("smelt-loaded-skills-{}", uuid::Uuid::new_v4()));
        write(&temp.join("keep/SKILL.md"), &skill_manifest("keep", "留下"));
        write(
            &temp.join("skip/SKILL.md"),
            &skill_manifest("skip", "不该出现"),
        );

        let loaded = loaded_skills_for_launch(
            &launch_with_plugin_args(&[
                "--no-skills",
                "--no-extensions",
                "--skill",
                temp.join("keep").to_str().unwrap(),
            ]),
            None,
        );
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "keep");

        let empty = loaded_skills_for_launch(
            &launch_with_plugin_args(&["--no-skills", "--no-extensions"]),
            None,
        );
        assert!(empty.is_empty());

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn skills_root_argument_expands_each_child_skill() {
        let temp = std::env::temp_dir().join(format!("smelt-loaded-root-{}", uuid::Uuid::new_v4()));
        write(&temp.join("one/SKILL.md"), &skill_manifest("one", "第一个"));
        write(&temp.join("two/SKILL.md"), &skill_manifest("two", "第二个"));

        let loaded = loaded_skills_for_launch(
            &launch_with_plugin_args(&["--skill", temp.to_str().unwrap()]),
            None,
        );
        let mut names: Vec<_> = loaded.iter().map(|skill| skill.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["one", "two"]);

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn bare_pi_discovers_native_project_skill_locations() {
        let root = std::env::temp_dir().join(format!("smelt-loaded-cwd-{}", uuid::Uuid::new_v4()));
        let repo = root.join("repo");
        let cwd = repo.join("nested");
        std::fs::create_dir_all(root.join(".agents/skills/outside-repo")).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        write(
            &repo.join(".agents/skills/from-repo/SKILL.md"),
            &skill_manifest("from-repo", "仓库级"),
        );
        write(
            &cwd.join(".agents/skills/from-nested/SKILL.md"),
            &skill_manifest("from-nested", "子目录级"),
        );
        write(
            &cwd.join(".pi/skills/from-pi/SKILL.md"),
            &skill_manifest("from-pi", "Pi 原生"),
        );
        write(
            &cwd.join(".claude/skills/not-native/SKILL.md"),
            &skill_manifest("not-native", "需要显式加载"),
        );
        write(
            &cwd.join(".skills/not-native/SKILL.md"),
            &skill_manifest("not-native-shortcut", "需要显式加载"),
        );
        write(
            &root.join(".agents/skills/outside-repo/SKILL.md"),
            &skill_manifest("outside-repo", "仓库外"),
        );

        let loaded = loaded_skills_for_launch(
            &ConversationLaunchSpec::from_command("pi"),
            Some(cwd.as_path()),
        );
        let names: std::collections::BTreeSet<_> =
            loaded.iter().map(|skill| skill.name.as_str()).collect();
        assert!(names.contains("from-repo"), "{names:?}");
        assert!(names.contains("from-nested"), "{names:?}");
        assert!(names.contains("from-pi"), "{names:?}");
        assert!(!names.contains("outside-repo"), "{names:?}");
        assert!(!names.contains("not-native"), "{names:?}");
        assert!(!names.contains("not-native-shortcut"), "{names:?}");

        std::fs::remove_dir_all(&root).ok();
    }
}
