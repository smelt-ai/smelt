//! Skills 面板数据源与 CRUD：真身统一存 `.smelt/skills/<name>/SKILL.md`
//! （用户级 `~/.smelt/skills`，项目级 `<项目>/.smelt/skills`），创建/改名/删除时
//! 自动在已知 agent 的 skills 目录（`~/.claude/skills`、`~/.codex/skills`、
//! `~/.copilot/skills`（项目级是 `.github/skills`）——都遵循同一套
//! `<name>/SKILL.md` 约定）里维护一份
//! **symlink** 指回真身，这样同一个 skill 改一处、各 agent 都同步生效，不用
//! 逐个目录复制。
//!
//! 面板同时也认「不是 symlink、直接躺在某个 agent 目录里」的旧 skill（不是
//! 我们建的，管不了同步，标 `managed = false`，编辑/删除仍可对它直接操作，
//! 但只影响那一个 agent）。
//!
//! **只读开关**：Claude Code 没有「启用/停用某个 skill」的开关机制
//! （settings.json 里的 `enabledPlugins` 管的是插件，不是 skill），所以这里
//! 不做开关——放一个拨了不生效的开关比不放更糟。
//!
//! 跟 claude_memory.rs 同一个套路：纯数据函数，后台线程扫盘，render 只读缓存。

use gpui::AppContext;
use gpui_component::input::Input;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::ui_theme;

/// 一个已知会扫描 `<agent 目录>/skills/<name>/SKILL.md` 的 agent：用户级和
/// 项目级目录名可能不一样（比如 Copilot CLI 用户级是 `~/.copilot/skills`，
/// 项目级却是 `.github/skills`），所以分开存。新增支持别的 agent 只需要在
/// `AGENT_TARGETS` 里加一项。
pub struct AgentTarget {
    /// 短标识，UI 里当 chip 文案用。
    pub label: &'static str,
    user_rel: &'static str,
    project_rel: &'static str,
}

impl AgentTarget {
    fn rel_dir(&self, project_scope: bool) -> &'static str {
        if project_scope {
            self.project_rel
        } else {
            self.user_rel
        }
    }
}

pub const AGENT_TARGETS: &[AgentTarget] = &[
    AgentTarget {
        label: "Claude",
        user_rel: ".claude/skills",
        project_rel: ".claude/skills",
    },
    AgentTarget {
        label: "Codex",
        user_rel: ".codex/skills",
        project_rel: ".codex/skills",
    },
    AgentTarget {
        label: "Copilot",
        user_rel: ".copilot/skills",
        project_rel: ".github/skills",
    },
    AgentTarget {
        label: "Grok",
        user_rel: ".grok/skills",
        project_rel: ".grok/skills",
    },
];

/// 一条 skill。
#[derive(Clone)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    /// true = 项目级，false = 用户级。
    pub project_scope: bool,
    /// skill 的真实数据所在目录（含 `SKILL.md` 的那一层）：托管 skill 指向
    /// `.smelt/skills/<name>`，非托管（legacy）指向它在某个 agent 目录里的
    /// 实际路径。编辑/删除都对着它操作。
    pub dir: PathBuf,
    /// 这个 scope 的根：用户级 = 主目录，项目级 = 项目 cwd。创建 symlink /
    /// 改名重新链接时用它拼出各 agent 目录。
    pub base: PathBuf,
    /// true = 真身在 `.smelt/skills` 下、已同步链接到各 agent 目录；
    /// false = 旧 skill，直接躺在某个 agent 自己的 skills 目录里，不受
    /// 「一处改、处处生效」管理，改名/删除只影响它所在的那一个 agent。
    pub managed: bool,
    /// 托管 skill 当前实际链接到了哪些 agent（`AgentTarget::label`）——
    /// 非托管 skill 这里始终是空，UI 不必也不该为它画链接矩阵，它本来就只
    /// 活在自己所在的那一个 agent 目录里，原样显示即可。
    pub linked_agents: Vec<&'static str>,
    /// 非托管 skill 实际躺在哪个 agent 自己的目录里（`AgentTarget::label`）
    /// ——托管 skill 真身在 `.smelt`，这里始终是 `None`。UI 靠它告诉用户
    /// 「这条到底是谁的」，而不是一个笼统看不出来源的「旧」。
    pub source_agent: Option<&'static str>,
    /// 同一 scope 下其它 agent 目录里发现的同名实体副本。过去这些副本会被
    /// 静默去重；现在保留下来交给 UI 明示冲突并让用户选择真身。
    pub duplicates: Vec<SkillDuplicate>,
}

#[derive(Clone)]
pub struct SkillDuplicate {
    pub dir: PathBuf,
    pub description: String,
    pub source_agent: Option<&'static str>,
    pub managed: bool,
}

/// 扫描用户级 + 项目级 skills（阻塞读盘，调用方放后台线程）。
pub fn scan_skills(project_cwd: Option<&str>) -> Vec<SkillEntry> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        collect_scope(&home, false, &mut out);
    }
    if let Some(cwd) = project_cwd {
        collect_scope(&PathBuf::from(cwd), true, &mut out);
    }
    // 项目级在前（更贴近手头的活），组内按名字排。
    out.sort_by(|a, b| {
        b.project_scope
            .cmp(&a.project_scope)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// 扫一个 scope（用户级或项目级）：先收 `.smelt/skills` 下的托管 skill，
/// 再扫各 agent 自己的 skills 目录——跳过已经解析回 `.smelt/skills` 的
/// symlink（那就是托管 skill 在这个 agent 下的落地，不重复列出），剩下的
/// 真实目录当 legacy 收进来。
fn collect_scope(base: &Path, project_scope: bool, out: &mut Vec<SkillEntry>) {
    let canonical_root = base.join(".smelt/skills");
    let mut found = Vec::new();
    collect_dir(&canonical_root, project_scope, base, true, None, &mut found);
    for t in AGENT_TARGETS {
        collect_dir(
            &base.join(t.rel_dir(project_scope)),
            project_scope,
            base,
            false,
            Some(t.label),
            &mut found,
        );
    }

    // `.smelt` 真身先扫描，因此同名存在托管版本时它自然成为主记录；其余
    // 实体目录作为冲突副本挂在同一张卡上，不再静默消失。
    let mut merged: Vec<SkillEntry> = Vec::new();
    for entry in found {
        if let Some(primary) = merged.iter_mut().find(|item| item.name == entry.name) {
            primary.duplicates.push(SkillDuplicate {
                dir: entry.dir,
                description: entry.description,
                source_agent: entry.source_agent,
                managed: entry.managed,
            });
        } else {
            merged.push(entry);
        }
    }
    out.extend(merged);
}

fn collect_dir(
    dir: &Path,
    project_scope: bool,
    base: &Path,
    managed: bool,
    source_label: Option<&'static str>,
    out: &mut Vec<SkillEntry>,
) {
    let canonical_root = base.join(".smelt/skills");
    let canonical_root_resolved =
        std::fs::canonicalize(&canonical_root).unwrap_or_else(|_| canonical_root.clone());
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !managed {
            // legacy 扫描时，跳过指回 `.smelt/skills` 的 symlink——那是托管
            // skill 链接到这个 agent 的落地，已经在 collect_dir(managed=true)
            // 那一趟收过了，这里再收就重复。
            let is_symlink = std::fs::symlink_metadata(&path)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            if is_symlink {
                if let Ok(target) = std::fs::canonicalize(&path) {
                    if target.starts_with(&canonical_root_resolved) {
                        continue;
                    }
                }
            }
        }
        let md = path.join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&md) else {
            continue;
        };
        let (name, description) = parse_frontmatter(&text);
        // frontmatter 缺 name 就退回目录名——目录名本来就是 skill 的调用名。
        let name = name.unwrap_or_else(|| {
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        if name.is_empty() {
            continue;
        }
        let linked_agents = if managed {
            linked_targets(base, project_scope, &name, &path)
        } else {
            Vec::new()
        };
        out.push(SkillEntry {
            name,
            description: description.unwrap_or_default(),
            project_scope,
            dir: path,
            base: base.to_path_buf(),
            managed,
            linked_agents,
            source_agent: source_label,
            duplicates: Vec::new(),
        });
    }
}

/// 某个托管 skill（真身在 `canonical_dir`）当前实际链接到了哪些 agent：
/// 逐个 agent 目录检查 `<agent 目录>/<name>` 是否是指回真身的 symlink。
fn linked_targets(
    base: &Path,
    project_scope: bool,
    name: &str,
    canonical_dir: &Path,
) -> Vec<&'static str> {
    let canon =
        std::fs::canonicalize(canonical_dir).unwrap_or_else(|_| canonical_dir.to_path_buf());
    AGENT_TARGETS
        .iter()
        .filter(|t| {
            let link = base.join(t.rel_dir(project_scope)).join(name);
            std::fs::symlink_metadata(&link)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
                && std::fs::canonicalize(&link)
                    .map(|p| p == canon)
                    .unwrap_or(false)
        })
        .map(|t| t.label)
        .collect()
}

/// 校验 skill 名（同时是目录名和 frontmatter `name`，两者本该一致）：非空、
/// 不超长、只允许字母/数字/连字符/下划线——避免路径穿越（`..`、`/`）或写出
/// Claude / Codex 不认的名字。
pub fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("名称不能为空".into());
    }
    if name.len() > 64 {
        return Err("名称过长（最多 64 个字符）".into());
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err("只能包含字母、数字、连字符(-)和下划线(_)".into());
    }
    if name.starts_with('-') || name.starts_with('_') {
        return Err("不能以 - 或 _ 开头".into());
    }
    Ok(())
}

/// 某个 scope 的根：项目级要求 `project_cwd` 已知，否则报错（调用方应该在
/// 项目未打开时禁掉「项目级」这个选项）。
fn scope_base(project_cwd: Option<&str>, project_scope: bool) -> Result<PathBuf, String> {
    if project_scope {
        project_cwd
            .map(PathBuf::from)
            .ok_or_else(|| "项目未打开，无法创建项目级 skill".to_string())
    } else {
        dirs::home_dir().ok_or_else(|| "无法定位用户主目录".to_string())
    }
}

/// 把字符串写成 YAML 双引号标量：能安全塞进 frontmatter 单行字段，不管原文
/// 有没有冒号、引号——省得再判断要不要加引号。
fn quote_scalar(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

/// 把 `<base>/.smelt/skills/<name>` 链接到某一个 agent 目录下。尽力而为：
/// 那个 agent 目录下已经有非 symlink 的同名 legacy skill，就跳过（打印到
/// stderr），不让单个 agent 的历史冲突挡住其它 agent。
fn link_one_agent(
    base: &Path,
    project_scope: bool,
    name: &str,
    target: &Path,
    agent: &AgentTarget,
) {
    let root = base.join(agent.rel_dir(project_scope));
    if std::fs::create_dir_all(&root).is_err() {
        return;
    }
    let link = root.join(name);
    match std::fs::symlink_metadata(&link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let _ = std::fs::remove_file(&link);
        }
        Ok(_) => {
            eprintln!(
                "[skills] {} 已存在非托管的同名 skill，跳过链接",
                link.display()
            );
            return;
        }
        Err(_) => {}
    }
    if let Err(e) = std::os::unix::fs::symlink(target, &link) {
        eprintln!(
            "[skills] symlink {} -> {} 失败：{e}",
            link.display(),
            target.display()
        );
    }
}

/// 从某一个 agent 目录里移除 `name` 对应的 symlink。只删 symlink，遇到非
/// symlink（legacy 撞了同名）绝不动。
fn unlink_one_agent(base: &Path, project_scope: bool, name: &str, agent: &AgentTarget) {
    let link = base.join(agent.rel_dir(project_scope)).join(name);
    if let Ok(meta) = std::fs::symlink_metadata(&link) {
        if meta.file_type().is_symlink() {
            let _ = std::fs::remove_file(&link);
        }
    }
}

/// 把 `<base>/.smelt/skills/<name>` 链接到全部已知 agent 的 skills 目录下。
fn link_to_agents(base: &Path, project_scope: bool, name: &str, target: &Path) {
    for t in AGENT_TARGETS {
        link_one_agent(base, project_scope, name, target, t);
    }
}

/// 从全部已知 agent 的 skills 目录里移除 `name` 对应的 symlink（改名/删除
/// 托管 skill 前调用）。
fn unlink_from_agents(base: &Path, project_scope: bool, name: &str) {
    for t in AGENT_TARGETS {
        unlink_one_agent(base, project_scope, name, t);
    }
}

/// 按用户勾选的 agent 列表调整一个**托管** skill 的链接：勾了就确保有
/// symlink，没勾就摘掉。用于「管理链接」弹窗的确认动作——用户想让一个已经
/// 只同步到 Claude 的 skill 也覆盖到 Codex/Copilot/Grok，或者反过来收窄。
pub fn set_agent_links(entry: &SkillEntry, selected_labels: &[&'static str]) -> Result<(), String> {
    if !entry.managed {
        return Err("只有 .smelt 托管的 skill 才能这样调整链接".into());
    }
    for t in AGENT_TARGETS {
        if selected_labels.contains(&t.label) {
            link_one_agent(&entry.base, entry.project_scope, &entry.name, &entry.dir, t);
        } else {
            unlink_one_agent(&entry.base, entry.project_scope, &entry.name, t);
        }
    }
    Ok(())
}

/// 创建一个新 skill：真身写进 `.smelt/skills/<name>`，再同步链接到各已知
/// agent 目录。校验名字、确认目标目录不存在（避免覆盖同名 skill）、写最小
/// 可用的 `SKILL.md`。返回真身目录路径。
pub fn create_skill(
    project_cwd: Option<&str>,
    project_scope: bool,
    name: &str,
    description: &str,
) -> Result<PathBuf, String> {
    let name = name.trim();
    validate_skill_name(name)?;
    let base = scope_base(project_cwd, project_scope)?;
    let dir = base.join(".smelt/skills").join(name);
    if dir.exists() {
        return Err(format!("已存在同名 skill：{}", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败：{e}"))?;
    let description = description.trim();
    let content = format!(
        "---\nname: {}\ndescription: {}\n---\n\n# {}\n\n在这里写这个 skill 的具体使用说明和步骤。\n",
        quote_scalar(name),
        quote_scalar(description),
        name
    );
    std::fs::write(dir.join("SKILL.md"), content)
        .map_err(|e| format!("写入 SKILL.md 失败：{e}"))?;
    link_to_agents(&base, project_scope, name, &dir);
    Ok(dir)
}

/// 从本地已有目录导入一个 skill：目录里必须已有 `SKILL.md`。整个目录（含
/// 附属文件，比如 references/ 脚本等）复制进 `.smelt/skills/<name>`——`name`
/// 优先取 frontmatter 里的 `name`，没有就退回源目录名；复制完同样同步链接到
/// 各已知 agent 目录。
pub fn import_skill(
    project_cwd: Option<&str>,
    project_scope: bool,
    source_dir: &Path,
) -> Result<PathBuf, String> {
    let md_path = source_dir.join("SKILL.md");
    let text =
        std::fs::read_to_string(&md_path).map_err(|_| "所选目录中没有 SKILL.md".to_string())?;
    let (fm_name, _) = parse_frontmatter(&text);
    let name = fm_name.unwrap_or_else(|| {
        source_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let name = name.trim().to_string();
    validate_skill_name(&name)?;
    let base = scope_base(project_cwd, project_scope)?;
    let dir = base.join(".smelt/skills").join(&name);
    if dir.exists() {
        return Err(format!("已存在同名 skill：{}", dir.display()));
    }
    copy_dir_recursive(source_dir, &dir).map_err(|e| format!("复制目录失败：{e}"))?;
    link_to_agents(&base, project_scope, &name, &dir);
    Ok(dir)
}

/// 把一个非托管（legacy）skill「收编」进 `.smelt` 统一管理：把它的内容原样
/// 复制进 `.smelt/skills/<name>`，删掉它原来那份实体目录，再按 `selected_labels`
/// 同步链接到勾选的 agent。这样用户可以把一个只有 Claude 能用的 skill 挑着
/// 覆盖到 Codex/Copilot/Grok，不用手动复制。
///
/// 只对非托管 skill 调用；`entry.dir` 此时是真实目录（不是 symlink），复制
/// 完直接删掉即可，不用像 `unlink_from_agents` 那样只敢删 symlink。调用方
/// （UI 弹窗）应该强制勾上 `entry.source_agent` 对应的那个，不然这个 skill
/// 会从它原本所在的位置消失。
pub fn adopt_skill_selected(
    entry: &SkillEntry,
    selected_labels: &[&'static str],
) -> Result<PathBuf, String> {
    if entry.managed {
        return Err("这个 skill 已经在 .smelt 统一管理下了".into());
    }
    let canonical_dir = entry.base.join(".smelt/skills").join(&entry.name);
    if canonical_dir.exists() {
        return Err(format!(
            "已存在同名的托管 skill：{}",
            canonical_dir.display()
        ));
    }
    copy_dir_recursive(&entry.dir, &canonical_dir).map_err(|e| format!("复制目录失败：{e}"))?;
    std::fs::remove_dir_all(&entry.dir).map_err(|e| format!("删除原目录失败：{e}"))?;
    for t in AGENT_TARGETS {
        if selected_labels.contains(&t.label) {
            link_one_agent(
                &entry.base,
                entry.project_scope,
                &entry.name,
                &canonical_dir,
                t,
            );
        }
    }
    Ok(canonical_dir)
}

/// 把同名实体副本统一成 `.smelt` 真身。`source_index=0` 代表卡片主记录，后续
/// 下标依次对应 `duplicates`。先完整复制到同文件系统临时目录，再替换真身；
/// 只有新真身就位后才移除其余副本并重建 agent 链接。
pub fn consolidate_skill_copies(
    entry: &SkillEntry,
    source_index: usize,
    selected_labels: &[&'static str],
) -> Result<PathBuf, String> {
    if entry.duplicates.is_empty() {
        return Err("没有需要处理的同名副本".into());
    }
    let source = if source_index == 0 {
        &entry.dir
    } else {
        &entry
            .duplicates
            .get(source_index - 1)
            .ok_or_else(|| "选择的副本不存在".to_string())?
            .dir
    };
    if !source.join("SKILL.md").is_file() {
        return Err(format!("所选目录缺少 SKILL.md：{}", source.display()));
    }

    let canonical_root = entry.base.join(".smelt/skills");
    std::fs::create_dir_all(&canonical_root).map_err(|e| format!("创建托管目录失败：{e}"))?;
    let canonical_dir = canonical_root.join(&entry.name);
    let temp_dir = canonical_root.join(format!(
        ".{}-consolidating-{}",
        entry.name,
        std::process::id()
    ));
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir).map_err(|e| format!("清理临时目录失败：{e}"))?;
    }
    if let Err(e) = copy_dir_recursive(source, &temp_dir) {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(format!("复制所选副本失败：{e}"));
    }

    let backup_dir = canonical_root.join(format!(".{}-backup-{}", entry.name, std::process::id()));
    if backup_dir.exists() {
        std::fs::remove_dir_all(&backup_dir).map_err(|e| format!("清理备份目录失败：{e}"))?;
    }
    if canonical_dir.exists() {
        std::fs::rename(&canonical_dir, &backup_dir).map_err(|e| format!("备份原真身失败：{e}"))?;
    }
    if let Err(e) = std::fs::rename(&temp_dir, &canonical_dir) {
        if backup_dir.exists() {
            let _ = std::fs::rename(&backup_dir, &canonical_dir);
        }
        return Err(format!("启用新真身失败：{e}"));
    }

    unlink_from_agents(&entry.base, entry.project_scope, &entry.name);
    let mut old_dirs = vec![entry.dir.clone()];
    old_dirs.extend(entry.duplicates.iter().map(|copy| copy.dir.clone()));
    for dir in old_dirs {
        if dir != canonical_dir && dir != backup_dir && dir.join("SKILL.md").is_file() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("移除旧副本 {} 失败：{e}", dir.display()))?;
        }
    }
    if backup_dir.exists() {
        std::fs::remove_dir_all(&backup_dir).map_err(|e| format!("移除旧真身失败：{e}"))?;
    }
    for target in AGENT_TARGETS {
        if selected_labels.contains(&target.label) {
            link_one_agent(
                &entry.base,
                entry.project_scope,
                &entry.name,
                &canonical_dir,
                target,
            );
        }
    }
    Ok(canonical_dir)
}

/// 收编到全部已知 agent——`adopt_skill_selected` 勾全部 label 的简写，测试
/// 和「不需要挑」的调用点用它更省事。
#[cfg(test)]
pub fn adopt_skill(entry: &SkillEntry) -> Result<PathBuf, String> {
    let labels: Vec<&'static str> = AGENT_TARGETS.iter().map(|t| t.label).collect();
    adopt_skill_selected(entry, &labels)
}

/// 递归复制目录（`std::fs` 没有内建的目录复制）。跳过 symlink 本身（避免把
/// 源目录里意外存在的 symlink 原样带进真身，日后改名/删除时行为会出乎意料），
/// 只复制常规文件和子目录。
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to)?;
        }
        // 跳过 symlink / 其它特殊文件类型。
    }
    Ok(())
}

/// 更新一个已有 skill：改名字/描述，保留 frontmatter 里的其它字段和正文不动。
/// 名字变了就顺手把目录也改名（目录名 = 调用名是约定），目标目录已存在则报错
/// 而不是静默覆盖；托管 skill 改名时同步重新链接各 agent 目录。
pub fn update_skill(entry: &SkillEntry, name: &str, description: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    validate_skill_name(name)?;
    let dir = &entry.dir;
    let md_path = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&md_path).map_err(|e| format!("读取 SKILL.md 失败：{e}"))?;
    let new_text = rewrite_frontmatter(&text, name, description.trim());
    std::fs::write(&md_path, new_text).map_err(|e| format!("写入 SKILL.md 失败：{e}"))?;

    let cur_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or_default();
    if cur_name != name {
        let new_dir = dir.with_file_name(name);
        if new_dir.exists() {
            return Err(format!("已存在同名 skill：{}", new_dir.display()));
        }
        std::fs::rename(dir, &new_dir).map_err(|e| format!("重命名目录失败：{e}"))?;
        if entry.managed {
            unlink_from_agents(&entry.base, entry.project_scope, cur_name);
            link_to_agents(&entry.base, entry.project_scope, name, &new_dir);
        }
        return Ok(new_dir);
    }
    Ok(dir.to_path_buf())
}

/// 删除一个 skill。删前确认目录里确有 `SKILL.md`——避免误删传进来的任意目录
/// （比如上层状态没同步好，指向了别的路径）；托管 skill 删除前先摘掉各 agent
/// 目录里的 symlink，再删真身。
pub fn delete_skill(entry: &SkillEntry) -> Result<(), String> {
    if !entry.dir.join("SKILL.md").exists() {
        return Err("目录中没有 SKILL.md，拒绝删除".into());
    }
    if entry.managed {
        unlink_from_agents(&entry.base, entry.project_scope, &entry.name);
    }
    std::fs::remove_dir_all(&entry.dir).map_err(|e| format!("删除失败：{e}"))
}

/// 重写 frontmatter 的 `name` / `description` 两个字段，其余字段（含未知的
/// 自定义 key）原样保留在原位置之后；正文（`---` 之后的部分）完全不动。
/// 没有 frontmatter 的文件会被补上一段。
fn rewrite_frontmatter(text: &str, name: &str, description: &str) -> String {
    let header = format!(
        "---\nname: {}\ndescription: {}\n",
        quote_scalar(name),
        quote_scalar(description)
    );
    let raw_lines: Vec<&str> = text.lines().collect();
    if raw_lines.first().map(|l| l.trim()) != Some("---") {
        return format!("{header}---\n\n{text}");
    }
    let end_idx = raw_lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, l)| l.trim() == "---")
        .map(|(i, _)| i);
    let Some(end_idx) = end_idx else {
        return format!("{header}---\n\n{text}");
    };
    let body = raw_lines[end_idx + 1..].join("\n");

    // 保留原 frontmatter 里除 name/description（含其折叠续行）外的其它字段。
    let mut kept = Vec::new();
    let mut i = 1;
    while i < end_idx {
        let line = raw_lines[i];
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            if let Some((k, _)) = line.split_once(':') {
                if matches!(k.trim(), "name" | "description") {
                    i += 1;
                    while i < end_idx
                        && (raw_lines[i].starts_with(' ') || raw_lines[i].starts_with('\t'))
                    {
                        i += 1;
                    }
                    continue;
                }
            }
        }
        kept.push(line);
        i += 1;
    }

    let mut out = header;
    for l in kept {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str("---\n");
    let body_trimmed = body.trim_start_matches('\n');
    if !body_trimmed.is_empty() {
        out.push('\n');
        out.push_str(body_trimmed);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// 从 YAML frontmatter 里取 `name` / `description`，只认最简形态
/// （`key: value`，值可带引号、可跨行缩进续行）——skill 的 frontmatter 就这几个
/// 标量字段，引全套 YAML 解析器不值当。
fn parse_frontmatter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let (mut name, mut description) = (None, None);
    // 当前正在续行的字段（YAML 折叠行：下一行有缩进即为上一行的续写）。
    let mut pending: Option<&'static str> = None;
    let mut buf = String::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if indented {
            if pending.is_some() {
                buf.push(' ');
                buf.push_str(line.trim());
            }
            continue;
        }
        // 新字段开始：先把上一段收尾。
        if let Some(key) = pending.take() {
            let v = unquote(buf.trim());
            match key {
                "name" => name = Some(v),
                _ => description = Some(v),
            }
            buf.clear();
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        match k.trim() {
            "name" => {
                pending = Some("name");
                buf.push_str(v.trim());
            }
            "description" => {
                pending = Some("description");
                buf.push_str(v.trim());
            }
            _ => {}
        }
    }
    if let Some(key) = pending {
        let v = unquote(buf.trim());
        match key {
            "name" => name = Some(v),
            _ => description = Some(v),
        }
    }
    (name, description)
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

/// render 侧缓存类型别名（与 usage_cache 等同款：(取得时刻, 数据)）。
pub type SkillsCache = Option<(std::time::Instant, Rc<Vec<SkillEntry>>)>;

impl crate::Workspace {
    /// SKILLS 面板：确保缓存新鲜（>30s 或换了项目就后台重扫）。
    /// 跟 ensure_memory_list 同一套模板——读盘绝不放在 render 里同步做。
    pub(crate) fn ensure_skills(&mut self, cwd: Option<String>, cx: &mut gpui::Context<Self>) {
        use std::time::{Duration, Instant};
        let fresh = self
            .skills_cache
            .as_ref()
            .is_some_and(|(t, _)| t.elapsed() < Duration::from_secs(30))
            && self.skills_cache_cwd == cwd;
        if fresh || self.skills_inflight {
            return;
        }
        self.skills_inflight = true;
        let scan_cwd = cwd.clone();
        cx.spawn(async move |this, cx| {
            let c = scan_cwd.clone();
            let list = cx
                .background_executor()
                .spawn(async move { scan_skills(c.as_deref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.skills_inflight = false;
                this.skills_cache_cwd = scan_cwd;
                this.skills_cache = Some((Instant::now(), Rc::new(list)));
                cx.notify();
            });
        })
        .detach();
    }

    /// 把 `/skill-name` 送进当前会话：终端直接敲进去（不回车，留给人补参数）；
    /// ACP 会话填进输入框并聚焦。没有活动会话就什么都不做。
    pub(crate) fn send_skill_to_session(
        &mut self,
        cmd: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(sess) = self.sessions.get(self.active_session) else {
            return;
        };
        match &sess.kind {
            crate::SessionKind::Acp(view) => {
                let view = view.clone();
                let text = cmd.to_string();
                view.update(cx, |v, cx| v.insert_prompt_text(&text, window, cx));
            }
            crate::SessionKind::Term { active, .. } => {
                let pane = active.clone();
                let text = cmd.to_string();
                pane.update(cx, |tv, cx| tv.type_text(&text, cx));
                self.focus_active(window, cx);
            }
        }
        cx.notify();
    }

    /// 打开「编辑 skill」弹窗，预填现有的名字/描述。
    pub(crate) fn open_edit_skill_modal(
        &mut self,
        entry: &SkillEntry,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        use gpui_component::input::InputState;
        let name_input = cx.new(|cx| InputState::new(window, cx).default_value(entry.name.clone()));
        let desc_input =
            cx.new(|cx| InputState::new(window, cx).default_value(entry.description.clone()));
        name_input.update(cx, |s, cx| s.focus(window, cx));
        self.skill_modal = Some(SkillModalState {
            editing: Some(entry.clone()),
            project_scope: entry.project_scope,
            name_input,
            desc_input,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn close_skill_modal(&mut self, cx: &mut gpui::Context<Self>) {
        self.skill_modal = None;
        cx.notify();
    }

    /// 提交新建/编辑弹窗：校验失败就把错误文案摆在弹窗里，不关闭；成功则关闭弹窗
    /// 并让 SKILLS 面板重新扫盘（清缓存，下次 render 触发 ensure_skills）。
    pub(crate) fn submit_skill_modal(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(modal) = self.skill_modal.as_ref() else {
            return;
        };
        let name = modal.name_input.read(cx).value().trim().to_string();
        let description = modal.desc_input.read(cx).value().trim().to_string();
        let editing = modal.editing.clone();
        let project_scope = modal.project_scope;
        let cwd = self.cur().and_then(|s| s.cwd(cx));

        let result = match editing {
            Some(entry) => update_skill(&entry, &name, &description).map(|_| ()),
            None => create_skill(cwd.as_deref(), project_scope, &name, &description).map(|_| ()),
        };

        match result {
            Ok(()) => {
                self.skills_cache = None;
                self.skill_modal = None;
            }
            Err(msg) => {
                if let Some(modal) = self.skill_modal.as_mut() {
                    modal.error = Some(msg);
                }
            }
        }
        cx.notify();
    }

    /// 「导入 skill」：弹原生目录选择框，选中的目录必须已含 `SKILL.md`——整个
    /// 目录（含附属文件）复制进 `.smelt/skills/<name>`，并同步链接到各已知
    /// agent 目录。失败只打日志，不额外弹错误框（跟这批「安静失败」的克制处理
    /// 一致；面板缓存照常清空重扫，用户从列表里能直接看出有没有成功）。
    pub(crate) fn import_skill_from_folder(
        &mut self,
        project_scope: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        use gpui::PathPromptOptions;
        let cwd = self.cur().and_then(|s| s.cwd(cx));
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择要导入的 skill 目录（需含 SKILL.md）".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(source) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                match import_skill(cwd.as_deref(), project_scope, &source) {
                    Ok(_) => this.skills_cache = None,
                    Err(e) => eprintln!("[skills] 导入失败：{e}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 弹出「删除 skill」二次确认。
    pub(crate) fn request_delete_skill(
        &mut self,
        entry: &SkillEntry,
        cx: &mut gpui::Context<Self>,
    ) {
        self.skill_delete_target = Some(entry.clone());
        cx.notify();
    }

    pub(crate) fn cancel_delete_skill(&mut self, cx: &mut gpui::Context<Self>) {
        self.skill_delete_target = None;
        cx.notify();
    }

    /// 真正执行删除：成功/失败都关掉确认弹窗（失败暂不额外提示——跟
    /// perform_delete_file 同一套克制处理，避免再叠一层错误弹窗）。
    pub(crate) fn confirm_delete_skill(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(target) = self.skill_delete_target.take() else {
            return;
        };
        let _ = delete_skill(&target);
        self.skills_cache = None;
        cx.notify();
    }

    /// 打开「应用到其他工具」弹窗——只给非托管（legacy）skill 用：收编 +
    /// 删原目录是有点破坏性的一次性操作（且必须强制保留原 agent），值得让
    /// 用户勾完再点确定。默认全勾（含它原本所在的那个）。
    ///
    /// 托管 skill 不走这个弹窗——见 `toggle_managed_skill_link`：链接/取消
    /// 链接只是加减一个 symlink，随点随撤销，不需要弹窗二次确认。
    pub(crate) fn open_skill_link_modal(
        &mut self,
        entry: &SkillEntry,
        cx: &mut gpui::Context<Self>,
    ) {
        self.skill_link_modal = Some(SkillLinkModalState {
            entry: entry.clone(),
            selected: vec![true; AGENT_TARGETS.len()],
            source_index: 0,
        });
        cx.notify();
    }

    /// 托管 skill 出现同名实体副本时，以 `.smelt` 真身为准一键清理，并把
    /// 所有已知 Agent 位置恢复成链接。无需让用户在本应唯一的数据源之间选择。
    pub(crate) fn cleanup_managed_skill_duplicates(
        &mut self,
        entry: &SkillEntry,
        cx: &mut gpui::Context<Self>,
    ) {
        if !entry.managed || entry.duplicates.is_empty() {
            return;
        }
        let labels: Vec<&'static str> = AGENT_TARGETS.iter().map(|t| t.label).collect();
        if let Err(e) = consolidate_skill_copies(entry, 0, &labels) {
            eprintln!("[skills] {} 清理同名副本失败：{e}", entry.name);
        }
        self.skills_cache = None;
        cx.notify();
    }

    /// 点一下托管 skill 的某个 agent 链接小标签，直接就地加/减那一个 symlink，
    /// 不弹窗——这是纯粹可逆的操作（撤销只需再点一下）。
    pub(crate) fn toggle_managed_skill_link(
        &mut self,
        entry: &SkillEntry,
        label: &'static str,
        cx: &mut gpui::Context<Self>,
    ) {
        if !entry.managed {
            return;
        }
        let Some(t) = AGENT_TARGETS.iter().find(|t| t.label == label) else {
            return;
        };
        if entry.linked_agents.contains(&label) {
            unlink_one_agent(&entry.base, entry.project_scope, &entry.name, t);
        } else {
            link_one_agent(&entry.base, entry.project_scope, &entry.name, &entry.dir, t);
        }
        self.skills_cache = None;
        cx.notify();
    }

    pub(crate) fn cancel_skill_link_modal(&mut self, cx: &mut gpui::Context<Self>) {
        self.skill_link_modal = None;
        cx.notify();
    }

    /// 勾选框切换：非托管 skill 时，它原本所在的那个 agent 不让取消勾选——
    /// 不然收编完这个 skill 就从它原来待的地方消失了，用户多半不是故意的。
    pub(crate) fn toggle_skill_link_agent(&mut self, idx: usize, cx: &mut gpui::Context<Self>) {
        let Some(modal) = self.skill_link_modal.as_mut() else {
            return;
        };
        let Some(t) = AGENT_TARGETS.get(idx) else {
            return;
        };
        if !modal.entry.managed && modal.entry.source_agent == Some(t.label) {
            return;
        }
        if let Some(v) = modal.selected.get_mut(idx) {
            *v = !*v;
            cx.notify();
        }
    }

    pub(crate) fn select_skill_copy(&mut self, idx: usize, cx: &mut gpui::Context<Self>) {
        let Some(modal) = self.skill_link_modal.as_mut() else {
            return;
        };
        if idx <= modal.entry.duplicates.len() {
            modal.source_index = idx;
            cx.notify();
        }
    }

    /// 确认「管理链接」弹窗：托管 skill 按勾选结果增删 symlink；非托管
    /// skill 按勾选结果收编进 `.smelt` 并链接。失败只打日志，跟其它「尽力
    /// 而为」的操作一致。
    pub(crate) fn confirm_skill_link_modal(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(modal) = self.skill_link_modal.take() else {
            return;
        };
        let labels: Vec<&'static str> = AGENT_TARGETS
            .iter()
            .zip(modal.selected.iter())
            .filter(|(_, sel)| **sel)
            .map(|(t, _)| t.label)
            .collect();
        let result = if !modal.entry.duplicates.is_empty() {
            consolidate_skill_copies(&modal.entry, modal.source_index, &labels).map(|_| ())
        } else if modal.entry.managed {
            set_agent_links(&modal.entry, &labels)
        } else {
            adopt_skill_selected(&modal.entry, &labels).map(|_| ())
        };
        if let Err(e) = result {
            eprintln!("[skills] {} 调整链接失败：{e}", modal.entry.name);
        }
        self.skills_cache = None;
        cx.notify();
    }

    /// 「管理链接」弹窗：一个 skill + 每个已知 agent 一个勾选框。
    pub(crate) fn render_skill_link_modal(&self, cx: &mut gpui::Context<Self>) -> gpui::Div {
        use gpui::prelude::FluentBuilder;
        use gpui::*;
        use gpui_component::checkbox::Checkbox;
        use gpui_component::*;
        let Some(modal) = self.skill_link_modal.as_ref() else {
            return div();
        };
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            crate::Workspace::modal_accent_colors(false);
        let title = if !modal.entry.duplicates.is_empty() {
            "处理同名副本"
        } else if modal.entry.managed {
            "管理链接"
        } else {
            "应用到其他工具"
        };

        let mut content =
            v_flex()
                .gap_3()
                .child(div().font_bold().text_color(fg).text_lg().child(title))
                .child(div().text_sm().text_color(muted).child(
                    if modal.entry.duplicates.is_empty() {
                        format!("勾选要同步「{}」的 agent：", modal.entry.name)
                    } else {
                        format!(
                            "发现 {} 份同名内容。选择一份作为 .smelt 真身，其余副本将替换为链接。",
                            modal.entry.duplicates.len() + 1
                        )
                    },
                ));

        if !modal.entry.duplicates.is_empty() {
            let mut copies = vec![(
                modal.entry.dir.clone(),
                modal.entry.description.clone(),
                modal.entry.source_agent,
                modal.entry.managed,
            )];
            copies.extend(modal.entry.duplicates.iter().map(|copy| {
                (
                    copy.dir.clone(),
                    copy.description.clone(),
                    copy.source_agent,
                    copy.managed,
                )
            }));
            content = content.child(
                v_flex()
                    .gap_1()
                    .children(copies.into_iter().enumerate().map(
                        |(idx, (path, description, source_agent, managed))| {
                            let selected = modal.source_index == idx;
                            div()
                                .id(("skill-copy-source", idx))
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(if selected { tint } else { neutral_bg })
                                .cursor_pointer()
                                .hover(move |d| d.bg(neutral_hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_skill_copy(idx, cx)
                                }))
                                .child(
                                    h_flex()
                                        .justify_between()
                                        .child(div().text_sm().font_semibold().child(if managed {
                                            ".smelt 真身"
                                        } else {
                                            source_agent.unwrap_or("其它来源")
                                        }))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(if selected {
                                                    accent_text
                                                } else {
                                                    muted
                                                })
                                                .child(if selected {
                                                    "将保留"
                                                } else {
                                                    "选择"
                                                }),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_family("monospace")
                                        .text_color(muted)
                                        .truncate()
                                        .child(path.to_string_lossy().into_owned()),
                                )
                                .when(!description.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .truncate()
                                            .child(description),
                                    )
                                })
                        },
                    )),
            );
            content = content.child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("启用到以下 Agent："),
            );
        }

        for (i, t) in AGENT_TARGETS.iter().enumerate() {
            let checked = modal.selected.get(i).copied().unwrap_or(false);
            let locked = !modal.entry.managed && modal.entry.source_agent == Some(t.label);
            // 之前把 agent 名字文字放在 Checkbox 外面的兄弟 div 里，导致点文字没反应，
            // 只有点中那个小方框才算数——改用 `.label()` 让整行（方框+文字）都能点。
            let mut checkbox = Checkbox::new(("skill-link-agent", i))
                .checked(checked)
                .disabled(locked)
                .label(t.label)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_skill_link_agent(i, cx)));
            if locked {
                checkbox = checkbox.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("（当前所在位置，不能取消）"),
                );
            }
            content = content.child(checkbox);
        }

        content = content.child(
            h_flex()
                .justify_end()
                .gap_2()
                .child(crate::Workspace::modal_button(
                    "cancel-skill-link",
                    "取消",
                    neutral_bg,
                    neutral_hover,
                    fg,
                    |this, _, _, cx| this.cancel_skill_link_modal(cx),
                    cx,
                ))
                .child(crate::Workspace::modal_button(
                    "confirm-skill-link",
                    "确定",
                    tint,
                    hover,
                    accent_text,
                    |this, _, _, cx| this.confirm_skill_link_modal(cx),
                    cx,
                )),
        );
        crate::Workspace::modal_shell(360., false, content, cx)
    }

    /// 「新建/编辑 skill」弹窗：与 render_rename_session 同款视觉，正文换成
    /// 名字 + 描述两个输入框 + 作用域切换（仅新建时可选，编辑时作用域已固定）。
    pub(crate) fn render_skill_modal(&self, cx: &mut gpui::Context<Self>) -> gpui::Div {
        use gpui::prelude::FluentBuilder;
        use gpui::*;
        use gpui_component::*;
        let Some(modal) = self.skill_modal.as_ref() else {
            return div();
        };
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            crate::Workspace::modal_accent_colors(false);
        let is_edit = modal.editing.is_some();
        let has_project = self.cur().and_then(|s| s.cwd(cx)).is_some();
        let project_scope = modal.project_scope;

        let mut content = v_flex()
            .child(
                div()
                    .font_bold()
                    .text_color(fg)
                    .text_lg()
                    .child(if is_edit {
                        "编辑 skill"
                    } else {
                        "新建 skill"
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("名称（目录名与调用名，字母/数字/连字符/下划线）"),
            )
            .child(Input::new(&modal.name_input))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("描述（何时该用它）"),
            )
            .child(Input::new(&modal.desc_input));

        if let Some(entry) = modal.editing.as_ref().filter(|e| !e.managed) {
            let agent = entry.source_agent.unwrap_or("未知 agent");
            content = content.child(
                div().text_xs().text_color(rgb(ui_theme::text_faint())).child(format!(
                    "这是 {} 的历史遗留 skill（不在 .smelt 统一管理下），改动只影响它当前所在的位置：{}",
                    agent,
                    entry.dir.display()
                )),
            );
        }

        if !is_edit {
            let e_user = cx.entity();
            let e_project = cx.entity();
            content = content.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_xs().text_color(muted).child("作用域"))
                    .child(
                        div()
                            .id("skill-scope-user")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .when(!project_scope, |d| d.bg(tint).text_color(accent_text))
                            .when(project_scope, |d| d.text_color(muted))
                            .child("用户级")
                            .on_click(move |_ev, _window, cx: &mut App| {
                                e_user.update(cx, |ws: &mut crate::Workspace, cx| {
                                    if let Some(m) = ws.skill_modal.as_mut() {
                                        m.project_scope = false;
                                    }
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        div()
                            .id("skill-scope-project")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .when(has_project, |d| d.cursor_pointer())
                            .when(!has_project, |d| d.opacity(0.4))
                            .when(project_scope, |d| d.bg(tint).text_color(accent_text))
                            .when(!project_scope, |d| d.text_color(muted))
                            .child("项目级")
                            .on_click(move |_ev, _window, cx: &mut App| {
                                e_project.update(cx, |ws: &mut crate::Workspace, cx| {
                                    if ws.cur().and_then(|s| s.cwd(cx)).is_some() {
                                        if let Some(m) = ws.skill_modal.as_mut() {
                                            m.project_scope = true;
                                        }
                                        cx.notify();
                                    }
                                });
                            }),
                    ),
            );
        }

        if let Some(err) = modal.error.as_ref() {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(Hsla::from(rgb(ui_theme::red())))
                    .child(err.clone()),
            );
        }

        content = content.child(
            h_flex()
                .justify_end()
                .gap_2()
                .child(crate::Workspace::modal_button(
                    "cancel-skill-modal",
                    "取消",
                    neutral_bg,
                    neutral_hover,
                    fg,
                    |this, _, _, cx| this.close_skill_modal(cx),
                    cx,
                ))
                .child(crate::Workspace::modal_button(
                    "confirm-skill-modal",
                    if is_edit { "保存" } else { "创建" },
                    tint,
                    hover,
                    accent_text,
                    |this, _, _, cx| this.submit_skill_modal(cx),
                    cx,
                )),
        );

        crate::Workspace::modal_shell(380., false, content, cx)
    }

    /// 「删除 skill」二次确认弹窗：明确写出作用域 + 路径，危险操作走红色强调。
    pub(crate) fn render_delete_skill_confirm(&self, cx: &mut gpui::Context<Self>) -> gpui::Div {
        use gpui::*;
        use gpui_component::*;
        let Some(target) = self.skill_delete_target.as_ref() else {
            return div();
        };
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            crate::Workspace::modal_accent_colors(true);
        let scope = if target.project_scope {
            "项目级"
        } else {
            "用户级"
        };

        let content = v_flex()
            .child(
                div()
                    .font_bold()
                    .text_color(fg)
                    .text_lg()
                    .child("确定删除这个 skill 吗？"),
            )
            .child(div().text_sm().text_color(muted).child(format!(
                "将永久删除{}「{}」，位于 {}，此操作不可撤销。",
                scope,
                target.name,
                target.dir.display()
            )))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(crate::Workspace::modal_button(
                        "cancel-delete-skill",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_delete_skill(cx),
                        cx,
                    ))
                    .child(crate::Workspace::modal_button(
                        "confirm-delete-skill",
                        "确定删除",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_delete_skill(cx),
                        cx,
                    )),
            );
        crate::Workspace::modal_shell(360., true, content, cx)
    }
}

/// 「新建/编辑 skill」弹窗状态。`editing` 为 `None` 表示新建，`Some(entry)` 表示
/// 正在编辑该 skill（提交走 [`update_skill`] 而非 [`create_skill`]）。
pub(crate) struct SkillModalState {
    pub editing: Option<SkillEntry>,
    pub project_scope: bool,
    pub name_input: gpui::Entity<gpui_component::input::InputState>,
    pub desc_input: gpui::Entity<gpui_component::input::InputState>,
    pub error: Option<String>,
}

/// 「管理 / 应用到其他工具」弹窗状态：一个 skill + 每个 `AGENT_TARGETS` 对应
/// 一个勾选框（下标一一对应）。托管 skill 用它调整已有链接；非托管 skill
/// 用它选要收编进哪些 agent（原本所在的那个会被强制勾上，不然收编完这个
/// skill 就从它原来的位置消失了）。
pub(crate) struct SkillLinkModalState {
    pub entry: SkillEntry,
    pub selected: Vec<bool>,
    pub source_index: usize,
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_quoted_and_folded_frontmatter() {
        let text = "---\nname: commit-work\ndescription: \"Create commits:\n  split into logical commits.\"\n---\n\n# body\n";
        let (name, desc) = super::parse_frontmatter(text);
        assert_eq!(name.as_deref(), Some("commit-work"));
        assert_eq!(
            desc.as_deref(),
            Some("Create commits: split into logical commits.")
        );
    }

    #[test]
    fn ignores_files_without_frontmatter() {
        let (name, desc) = super::parse_frontmatter("# 没有 frontmatter\n");
        assert!(name.is_none() && desc.is_none());
    }

    #[test]
    fn validates_skill_name() {
        assert!(super::validate_skill_name("commit-work").is_ok());
        assert!(super::validate_skill_name("").is_err());
        assert!(super::validate_skill_name("../evil").is_err());
        assert!(super::validate_skill_name("has space").is_err());
        assert!(super::validate_skill_name("-leading-dash").is_err());
    }

    #[test]
    fn rewrite_frontmatter_preserves_body_and_unknown_fields() {
        let text = "---\nname: old-name\ndescription: old desc\nversion: 2\n---\n\n# Body\n\ncontent here\n";
        let out = super::rewrite_frontmatter(text, "new-name", "new desc");
        assert!(out.starts_with("---\nname: \"new-name\"\ndescription: \"new desc\"\n"));
        assert!(out.contains("version: 2\n"));
        assert!(out.contains("# Body\n\ncontent here\n"));
        assert!(!out.contains("old-name"));
        assert!(!out.contains("old desc"));
    }

    #[test]
    fn create_update_delete_skill_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("smelt-skill-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let cwd = tmp.to_string_lossy().into_owned();

        let dir = super::create_skill(Some(&cwd), true, "my-skill", "does a thing").unwrap();
        assert!(dir.join("SKILL.md").exists());
        assert!(dir.ends_with(".smelt/skills/my-skill"));

        // 同名再建应报错，不覆盖。
        assert!(super::create_skill(Some(&cwd), true, "my-skill", "x").is_err());

        // 应该同步链接到两个已知 agent 目录。
        for agent in [
            ".claude/skills",
            ".codex/skills",
            ".github/skills",
            ".grok/skills",
        ] {
            let link = tmp.join(agent).join("my-skill");
            let meta = std::fs::symlink_metadata(&link).unwrap();
            assert!(meta.file_type().is_symlink());
            assert!(link.join("SKILL.md").exists());
        }

        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        // legacy 扫描要跳过指回 .smelt 的 symlink，只应该出现这一条托管记录。
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].name, "my-skill");
        assert_eq!(scanned[0].description, "does a thing");
        assert!(scanned[0].managed);
        assert_eq!(scanned[0].linked_agents.len(), super::AGENT_TARGETS.len());

        let entry = scanned.into_iter().next().unwrap();
        let new_dir = super::update_skill(&entry, "renamed-skill", "new desc").unwrap();
        assert!(new_dir.join("SKILL.md").exists());
        assert!(!dir.exists());
        // 改名后旧 symlink 应该消失，新 symlink 应该指向新目录。
        for agent in [
            ".claude/skills",
            ".codex/skills",
            ".github/skills",
            ".grok/skills",
        ] {
            assert!(!tmp.join(agent).join("my-skill").exists());
            let link = tmp.join(agent).join("renamed-skill");
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].name, "renamed-skill");
        assert_eq!(scanned[0].description, "new desc");

        let entry = scanned.into_iter().next().unwrap();
        super::delete_skill(&entry).unwrap();
        assert!(!new_dir.exists());
        for agent in [
            ".claude/skills",
            ".codex/skills",
            ".github/skills",
            ".grok/skills",
        ] {
            assert!(!tmp.join(agent).join("renamed-skill").exists());
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn legacy_skill_is_listed_but_not_managed() {
        let tmp = std::env::temp_dir().join(format!("smelt-skill-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let legacy_dir = tmp.join(".claude/skills/old-skill");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(
            legacy_dir.join("SKILL.md"),
            "---\nname: old-skill\ndescription: 手工建的旧 skill\n---\n",
        )
        .unwrap();

        let cwd = tmp.to_string_lossy().into_owned();
        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].name, "old-skill");
        assert!(!scanned[0].managed);
        assert!(scanned[0].linked_agents.is_empty());
        assert_eq!(scanned[0].source_agent, Some("Claude"));
        assert_eq!(scanned[0].dir, legacy_dir);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn import_skill_copies_directory_and_links_to_agents() {
        let tmp = std::env::temp_dir().join(format!("smelt-skill-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let source = tmp.join("source-skill");
        std::fs::create_dir_all(source.join("references")).unwrap();
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: imported-skill\ndescription: 从本地目录导入\n---\n\n# body\n",
        )
        .unwrap();
        std::fs::write(source.join("references/notes.md"), "extra file").unwrap();

        let project = tmp.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let cwd = project.to_string_lossy().into_owned();

        let dir = super::import_skill(Some(&cwd), true, &source).unwrap();
        assert!(dir.ends_with(".smelt/skills/imported-skill"));
        assert!(dir.join("SKILL.md").exists());
        assert!(dir.join("references/notes.md").exists());

        for agent in [
            ".claude/skills",
            ".codex/skills",
            ".github/skills",
            ".grok/skills",
        ] {
            let link = project.join(agent).join("imported-skill");
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }

        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].name, "imported-skill");
        assert_eq!(scanned[0].linked_agents.len(), super::AGENT_TARGETS.len());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn adopt_skill_promotes_legacy_into_smelt_and_links_all_agents() {
        let tmp = std::env::temp_dir().join(format!("smelt-skill-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let legacy_dir = tmp.join(".claude/skills/old-skill");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(
            legacy_dir.join("SKILL.md"),
            "---\nname: old-skill\ndescription: 手工建的旧 skill\n---\n\n# body\n",
        )
        .unwrap();
        std::fs::write(legacy_dir.join("notes.txt"), "extra file").unwrap();

        let cwd = tmp.to_string_lossy().into_owned();
        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        assert_eq!(scanned.len(), 1);
        let entry = scanned.into_iter().next().unwrap();
        assert!(!entry.managed);

        let canonical_dir = super::adopt_skill(&entry).unwrap();
        assert!(canonical_dir.ends_with(".smelt/skills/old-skill"));
        assert!(canonical_dir.join("SKILL.md").exists());
        assert!(canonical_dir.join("notes.txt").exists());
        // 原来的实体目录应该被删掉了（下面会验证它变成了 symlink）。
        assert!(
            !legacy_dir.is_dir()
                || std::fs::symlink_metadata(&legacy_dir)
                    .unwrap()
                    .file_type()
                    .is_symlink()
        );

        for agent in [
            ".claude/skills",
            ".codex/skills",
            ".github/skills",
            ".grok/skills",
        ] {
            let link = tmp.join(agent).join("old-skill");
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }

        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        assert_eq!(scanned.len(), 1);
        assert!(scanned[0].managed);
        assert_eq!(scanned[0].linked_agents.len(), super::AGENT_TARGETS.len());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn managed_skill_reports_and_cleans_duplicate_agent_copies() {
        let tmp = std::env::temp_dir().join(format!(
            "smelt-skill-duplicate-cleanup-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let cwd = tmp.to_string_lossy().into_owned();
        let canonical = super::create_skill(Some(&cwd), true, "shared", "canonical").unwrap();
        std::fs::write(canonical.join("body.txt"), "keep me").unwrap();

        for rel in [".claude/skills/shared", ".codex/skills/shared"] {
            let dir = tmp.join(rel);
            std::fs::remove_file(&dir).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                "---\nname: shared\ndescription: stale copy\n---\n",
            )
            .unwrap();
        }

        let entry = super::scan_skills(Some(&cwd))
            .into_iter()
            .find(|skill| skill.project_scope && skill.name == "shared")
            .unwrap();
        assert!(entry.managed);
        assert_eq!(entry.duplicates.len(), 2);

        let labels: Vec<_> = super::AGENT_TARGETS.iter().map(|t| t.label).collect();
        super::consolidate_skill_copies(&entry, 0, &labels).unwrap();
        assert_eq!(
            std::fs::read_to_string(canonical.join("body.txt")).unwrap(),
            "keep me"
        );
        for rel in [
            ".claude/skills/shared",
            ".codex/skills/shared",
            ".github/skills/shared",
            ".grok/skills/shared",
        ] {
            assert!(
                std::fs::symlink_metadata(tmp.join(rel))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        let rescanned = super::scan_skills(Some(&cwd))
            .into_iter()
            .find(|skill| skill.project_scope && skill.name == "shared")
            .unwrap();
        assert!(rescanned.duplicates.is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn adopt_skill_selected_only_links_chosen_agents_but_keeps_origin() {
        let tmp =
            std::env::temp_dir().join(format!("smelt-skill-adopt-sel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let legacy_dir = tmp.join(".grok/skills/help");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(
            legacy_dir.join("SKILL.md"),
            "---\nname: help\ndescription: Grok 自带的帮助 skill\n---\n",
        )
        .unwrap();

        let cwd = tmp.to_string_lossy().into_owned();
        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        let entry = scanned.into_iter().next().unwrap();
        assert_eq!(entry.source_agent, Some("Grok"));

        // 只勾 Grok（原位置）和 Claude，不勾 Codex/Copilot。
        let canonical_dir = super::adopt_skill_selected(&entry, &["Grok", "Claude"]).unwrap();
        assert!(canonical_dir.join("SKILL.md").exists());

        let grok_link = tmp.join(".grok/skills/help");
        let claude_link = tmp.join(".claude/skills/help");
        let codex_link = tmp.join(".codex/skills/help");
        let copilot_link = tmp.join(".github/skills/help");
        assert!(
            std::fs::symlink_metadata(&grok_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::symlink_metadata(&claude_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!codex_link.exists());
        assert!(!copilot_link.exists());

        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        assert_eq!(scanned.len(), 1);
        assert!(scanned[0].managed);
        let mut linked = scanned[0].linked_agents.clone();
        linked.sort();
        assert_eq!(linked, vec!["Claude", "Grok"]);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn set_agent_links_adds_and_removes_symlinks_for_managed_skill() {
        let tmp = std::env::temp_dir().join(format!("smelt-skill-setlinks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let cwd = tmp.to_string_lossy().into_owned();

        let dir = super::create_skill(Some(&cwd), true, "commit-work", "desc").unwrap();
        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        let entry = scanned.into_iter().next().unwrap();
        assert!(entry.managed);
        assert_eq!(entry.linked_agents.len(), super::AGENT_TARGETS.len());

        // 收窄到只有 Claude。
        super::set_agent_links(&entry, &["Claude"]).unwrap();
        assert!(dir.exists());
        assert!(
            std::fs::symlink_metadata(tmp.join(".claude/skills/commit-work"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!tmp.join(".codex/skills/commit-work").exists());
        assert!(!tmp.join(".github/skills/commit-work").exists());
        assert!(!tmp.join(".grok/skills/commit-work").exists());

        // 再加回 Codex。
        super::set_agent_links(&entry, &["Claude", "Codex"]).unwrap();
        assert!(
            std::fs::symlink_metadata(tmp.join(".codex/skills/commit-work"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
