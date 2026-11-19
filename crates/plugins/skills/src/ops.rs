//! 写操作。都先算出要做的几步（[`Action`]），预览和执行走同一份：面板上看到的就是会落盘的。
//!
//! 删掉、换下的一律移进回收目录，能手动找回。软链只是引用：删链接不碰它指着的东西，搬本体
//! 时把指着它的链接一起清掉，不留悬空链接。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rust_i18n::t;
use smelt_plugin_api::model::chrono;

use crate::agents::{self, AGENTS, Agent};
use crate::fsx::{self, Kind};
use crate::scan::{
    self, BrokenLink, Frontmatter, LEGACY, Placement, Places, Skill, UNIVERSAL, bridge_agents,
    installed_agents, skill_roots,
};

/// 一步文件操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// 把选中的目录装成 `to`（导入）。已有同名目录时换下来进回收目录。
    Install {
        from: PathBuf,
        to: PathBuf,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
    },
    /// 在 `at` 放一条指向 `to` 的兼容链接。
    Link {
        at: PathBuf,
        to: PathBuf,
    },
    /// 删掉 `at` 这条链接（是软链才删）。
    Unlink {
        at: PathBuf,
    },
    Trash {
        from: PathBuf,
    },
}

/// 技能给谁用、放在哪一层级。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRequest {
    /// 给所有 agent（放在 `.agents/skills`）。否则只给 `labels` 里的几家，第一家放本体。
    pub universal: bool,
    pub labels: Vec<&'static str>,
    pub project_scope: bool,
}

impl ScopeRequest {
    /// 技能现在的作用范围。
    pub fn of(skill: &Skill) -> Self {
        match skill.placement {
            Placement::Agent(label) => Self {
                universal: false,
                labels: vec![label],
                project_scope: skill.project_scope,
            },
            _ => Self {
                universal: true,
                labels: Vec::new(),
                project_scope: skill.project_scope,
            },
        }
    }

    /// 勾选按能力表的先后排；指定 agent 却一个没勾是错的。
    fn normalized(&self) -> Result<Self, String> {
        let labels: Vec<&'static str> = AGENTS
            .iter()
            .filter(|agent| self.labels.contains(&agent.label))
            .map(|agent| agent.label)
            .collect();
        if !self.universal && labels.is_empty() {
            return Err(t!("error.no_agent").into_owned());
        }
        Ok(Self {
            universal: self.universal,
            labels,
            project_scope: self.project_scope,
        })
    }
}

/// 这个层级里能单独放一份技能的 agent，和它在不在用。在用的排前面。
pub fn scope_choices(
    installed: &BTreeSet<&'static str>,
    project_scope: bool,
) -> Vec<(&'static str, bool)> {
    let mut choices: Vec<(&'static str, bool)> = AGENTS
        .iter()
        .filter(|agent| agent.dir(project_scope).is_some())
        .map(|agent| (agent.label, installed.contains(agent.label)))
        .collect();
    choices.sort_by_key(|(_, installed)| !installed);
    choices
}

fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

fn agent_root(base: &Path, agent: &Agent, project_scope: bool) -> Result<PathBuf, String> {
    agent
        .dir(project_scope)
        .map(|dir| base.join(dir))
        .ok_or_else(|| {
            t!(
                "error.agent_reads_universal",
                agent = agent.label,
                dir = UNIVERSAL
            )
            .into_owned()
        })
}

fn destination_for(base: &Path, name: &str, request: &ScopeRequest) -> Result<PathBuf, String> {
    if request.universal {
        return Ok(base.join(UNIVERSAL).join(name));
    }
    let primary = request
        .labels
        .first()
        .and_then(|label| agents::find(label))
        .ok_or_else(|| t!("error.unknown_agent").into_owned())?;
    Ok(agent_root(base, primary, request.project_scope)?.join(name))
}

/// 本体落定之后还要被哪些路径引用。通用只给「在用、且自己不读 `.agents`」的 agent 建链接；
/// 指定 agent 时，第一家是本体，其余是用户勾上的链接。整个技能目录就软链到本体所在目录的
/// agent 已经读得到，不再建：那条链接会落进本体自己的目录。
fn references_for(
    installed: &BTreeSet<&'static str>,
    base: &Path,
    name: &str,
    request: &ScopeRequest,
) -> Result<Vec<PathBuf>, String> {
    let wanted: Vec<&Agent> = if request.universal {
        bridge_agents(installed, request.project_scope)
    } else {
        AGENTS
            .iter()
            .filter(|agent| {
                request.labels.contains(&agent.label)
                    && request.labels.first() != Some(&agent.label)
            })
            .collect()
    };
    let destination = destination_for(base, name, request)?;
    let destination_root = fsx::canonical(destination.parent().unwrap_or(base));
    let mut out = Vec::new();
    for agent in wanted {
        let at = agent_root(base, agent, request.project_scope)?.join(name);
        if fsx::canonical(at.parent().unwrap_or(base)) != destination_root {
            out.push(at);
        }
    }
    Ok(out)
}

/// 这个层级里指向 `target` 的软链。删本体或搬走时要一并清掉，否则留下悬空链接。
fn references_to(base: &Path, project_scope: bool, target: &Path) -> Vec<PathBuf> {
    let wanted = fsx::canonical(target);
    let mut found = Vec::new();
    for root in skill_roots(base, project_scope) {
        let Ok(entries) = fsx::entries(&root.path) else {
            continue;
        };
        for (name, kind) in entries {
            let path = root.path.join(name);
            if kind == Kind::Symlink && path != target && fsx::canonical(&path) == wanted {
                found.push(path);
            }
        }
    }
    found
}

/// 在 `at` 放一条指向 `target` 的兼容链接。实体目录绝不覆盖。
fn write_link(at: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent).map_err(io_error)?;
    }
    match fsx::kind(at) {
        None => {}
        Some(Kind::Symlink) => {
            if fsx::canonical(at) == fsx::canonical(target) {
                return Ok(());
            }
            fsx::remove_link(at).map_err(io_error)?;
        }
        Some(_) => {
            return Err(t!("error.real_dir", path = at.display()).into_owned());
        }
    }
    fsx::symlink(target, at).map_err(io_error)
}

/// 同一次操作移走的东西放进同一个时间戳目录。
fn trash_root(places: &Places) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    places.trash.join(stamp)
}

pub fn execute(places: &Places, actions: &[Action]) -> Result<(), String> {
    let mut trash: Option<PathBuf> = None;
    for action in actions {
        match action {
            Action::Install { from, to } => {
                let root = trash.get_or_insert_with(|| trash_root(places)).clone();
                install_tree(places, &root, from, to)?;
            }
            Action::Unlink { at } => {
                if fsx::kind(at) == Some(Kind::Symlink) {
                    fsx::remove_link(at).map_err(io_error)?;
                }
            }
            Action::Move { from, to } => {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).map_err(io_error)?;
                }
                std::fs::rename(from, to).map_err(io_error)?;
            }
            Action::Link { at, to } => write_link(at, to)?,
            Action::Trash { from } => {
                let root = trash.get_or_insert_with(|| trash_root(places));
                fsx::move_into(root, from, &fsx::file_name(from)).map_err(io_error)?;
            }
        }
    }
    Ok(())
}

/// 给缺兼容链接的 agent 补上。
pub fn repair_bridges(places: &Places, skill: &Skill) -> Result<(), String> {
    if matches!(skill.placement, Placement::Agent(_)) {
        return Err(t!("error.agent_only_no_links").into_owned());
    }
    let installed = installed_agents(&places.home);
    for agent in bridge_agents(&installed, skill.project_scope) {
        if skill.bridges.contains(&agent.label) {
            continue;
        }
        let root = agent_root(&skill.base, agent, skill.project_scope)?;
        write_link(&root.join(&skill.name), &skill.dir)?;
    }
    Ok(())
}

/// 删除：指着它的链接清掉，本体移进回收目录。本体本身是软链（指向层级之外的那种）时，删的
/// 只是这条引用。
pub fn trash_skill(places: &Places, skill: &Skill) -> Result<(), String> {
    for at in references_to(&skill.base, skill.project_scope, &skill.dir) {
        fsx::remove_link(&at).map_err(io_error)?;
    }
    if fsx::kind(&skill.dir) == Some(Kind::Symlink) {
        return fsx::remove_link(&skill.dir).map_err(io_error);
    }
    fsx::move_into(&trash_root(places), &skill.dir, &fsx::file_name(&skill.dir))
        .map(|_| ())
        .map_err(io_error)
}

/// 只删这些链接本身，不动任何真实目录。
pub fn prune_links(broken: &[BrokenLink]) -> Result<(), String> {
    for link in broken {
        if fsx::kind(&link.path) == Some(Kind::Symlink) {
            fsx::remove_link(&link.path).map_err(io_error)?;
        }
    }
    Ok(())
}

/// 重扫一遍，按目录找到那个技能。界面上的那份可能已经过时。
pub fn locate(places: &Places, dir: &Path) -> Result<Skill, String> {
    scan::scan(places)
        .skills
        .into_iter()
        .find(|skill| skill.dir == dir)
        .ok_or_else(|| t!("error.skill_gone").into_owned())
}

fn plan_scope_change(
    places: &Places,
    entry: &Skill,
    request: &ScopeRequest,
) -> Result<Vec<Action>, String> {
    if !entry.conflicts.is_empty() {
        return Err(t!("error.has_conflicts").into_owned());
    }
    let request = request.normalized()?;
    let destination_base = places.base(request.project_scope)?;
    let destination = destination_for(destination_base, &entry.name, &request)?;
    let occupant = fsx::kind(&destination);
    // 目标位置常常正是一条指回本体的兼容链接：真实路径相等，但不代表本体已经在那。只有实体
    // 目录才算「已经在这」。
    let settled = destination == entry.dir
        || (occupant.is_some_and(|kind| kind != Kind::Symlink)
            && fsx::canonical(&destination) == fsx::canonical(&entry.dir));

    let mut stale = references_to(&entry.base, entry.project_scope, &entry.dir);
    for path in references_to(destination_base, request.project_scope, &entry.dir) {
        if !stale.contains(&path) {
            stale.push(path);
        }
    }
    let installed = installed_agents(&places.home);
    let wanted = references_for(&installed, destination_base, &entry.name, &request)?;

    let mut actions = Vec::new();
    for path in &stale {
        if *path != destination && !wanted.contains(path) {
            actions.push(Action::Unlink { at: path.clone() });
        }
    }
    if !settled {
        if let Some(kind) = occupant {
            if kind != Kind::Symlink {
                return Err(t!(
                    "error.destination_taken",
                    path = places.pretty(&destination)
                )
                .into_owned());
            }
            actions.push(Action::Unlink {
                at: destination.clone(),
            });
        }
        actions.push(Action::Move {
            from: entry.dir.clone(),
            to: destination.clone(),
        });
    }
    for path in wanted {
        // 已经指对了的链接不用重建，预览里也就不该出现这一步。
        if settled && stale.contains(&path) {
            continue;
        }
        actions.push(Action::Link {
            at: path,
            to: destination.clone(),
        });
    }
    Ok(actions)
}

/// 改作用范围要做的几步。空的表示已经是这样。
pub fn plan_scope(
    places: &Places,
    dir: &Path,
    request: &ScopeRequest,
) -> Result<Vec<Action>, String> {
    plan_scope_change(places, &locate(places, dir)?, request)
}

/// 改作用范围，返回技能落定后的目录，界面跟过去。
pub fn apply_scope(places: &Places, dir: &Path, request: &ScopeRequest) -> Result<PathBuf, String> {
    let entry = locate(places, dir)?;
    let actions = plan_scope_change(places, &entry, request)?;
    execute(places, &actions)?;
    let request = request.normalized()?;
    destination_for(places.base(request.project_scope)?, &entry.name, &request)
}

/// 留下 `keep` 那一份，其余连同指着它们的链接一起进回收目录。不合并内容。
pub fn resolve_conflict(places: &Places, dir: &Path, keep: &Path) -> Result<(), String> {
    let entry = locate(places, dir)?;
    if entry.conflicts.is_empty() {
        return Err(t!("error.no_conflicts").into_owned());
    }
    let copies: Vec<PathBuf> = std::iter::once(entry.dir.clone())
        .chain(entry.conflicts.iter().map(|copy| copy.dir.clone()))
        .collect();
    if !copies.iter().any(|copy| copy == keep) {
        return Err(t!("error.copy_missing").into_owned());
    }
    let mut actions = Vec::new();
    for copy in copies.iter().filter(|copy| *copy != keep) {
        for at in references_to(&entry.base, entry.project_scope, copy) {
            actions.push(Action::Unlink { at });
        }
        actions.push(Action::Trash { from: copy.clone() });
    }
    execute(places, &actions)
}

/// 迁移旧位置的结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Migration {
    pub migrated: Vec<String>,
    /// 同名冲突迁不动的，要用户先挑留哪份。
    pub skipped: Vec<String>,
}

/// 把还躺在 `.smelt/skills` 里的技能迁到同层级的 `.agents/skills`。一个迁不动不拖住其余的；
/// 迁空了才收掉旧目录。
pub fn migrate_legacy(places: &Places) -> Result<Migration, String> {
    let mut outcome = Migration::default();
    let mut failed: BTreeSet<PathBuf> = BTreeSet::new();
    for guard in 0.. {
        if guard == 1000 {
            return Err(t!("error.migrate_runaway").into_owned());
        }
        let Some(entry) = scan::scan(places)
            .skills
            .into_iter()
            .find(|skill| skill.placement == Placement::Legacy && !failed.contains(&skill.dir))
        else {
            break;
        };
        let request = ScopeRequest {
            universal: true,
            labels: Vec::new(),
            project_scope: entry.project_scope,
        };
        match plan_scope_change(places, &entry, &request)
            .and_then(|actions| execute(places, &actions))
        {
            Ok(()) => outcome.migrated.push(entry.name),
            Err(_) => {
                failed.insert(entry.dir);
                outcome.skipped.push(entry.name);
            }
        }
    }
    // 同名副本归在主条目的 conflicts 里，上面看不到。照实报出来，让用户先选留哪份。
    for entry in scan::scan(places).skills {
        let blocked = entry
            .conflicts
            .iter()
            .any(|copy| copy.placement == Placement::Legacy);
        if blocked && !outcome.skipped.contains(&entry.name) {
            outcome.skipped.push(entry.name);
        }
    }
    fsx::remove_empty_dir(&places.home.join(LEGACY));
    if let Some(project) = &places.project {
        fsx::remove_empty_dir(&project.join(LEGACY));
    }
    Ok(outcome)
}

/// 技能名：字母、数字、`-`、`_`，不以 `-` `_` 开头，最长 64。
pub fn validate_skill_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.is_empty() {
        return Err(t!("error.name_empty").into_owned());
    }
    if name.chars().count() > 64 {
        return Err(t!("error.name_long").into_owned());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(t!("error.name_chars").into_owned());
    }
    if name.starts_with('-') || name.starts_with('_') {
        return Err(t!("error.name_start").into_owned());
    }
    Ok(name.to_string())
}

/// 选中的目录里的 `SKILL.md`：名字（校验过）和描述。
fn read_source(source: &Path) -> Result<(String, String), String> {
    let text = std::fs::read_to_string(source.join("SKILL.md"))
        .map_err(|error| t!("error.no_skill_md", error = error).into_owned())?;
    let frontmatter = Frontmatter::parse(&text);
    let name = validate_skill_name(
        frontmatter
            .name
            .as_deref()
            .unwrap_or(&fsx::file_name(source)),
    )?;
    Ok((name, frontmatter.description.unwrap_or_default()))
}

/// 选中要导入的目录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceInfo {
    pub name: String,
    pub description: String,
    /// 用户级已经有另一份同名技能：装到项目级会在这个项目里盖过它。
    pub has_global: bool,
}

/// 源本身就是那份用户级技能时不算「已有」，否则把自己再导入一次也会提醒。
pub fn inspect_import(places: &Places, source: &Path) -> Result<SourceInfo, String> {
    let (name, description) = read_source(source)?;
    let mut user = Vec::new();
    scan::scan_scope(
        &installed_agents(&places.home),
        &places.home,
        false,
        &mut user,
        &mut Vec::new(),
    );
    let source_real = fsx::canonical(source);
    let existing = user.into_iter().find(|skill| skill.name == name);
    let same = existing.as_ref().is_some_and(|skill| {
        std::iter::once(&skill.dir)
            .chain(skill.conflicts.iter().map(|copy| &copy.dir))
            .any(|dir| fsx::canonical(dir) == source_real)
    });
    Ok(SourceInfo {
        name,
        description,
        has_global: existing.is_some() && !same,
    })
}

/// 导入会做的几步。`replaces`：目标已有同名目录，导入会换掉它（旧版本进回收目录）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportPlan {
    pub actions: Vec<Action>,
    pub replaces: bool,
}

pub fn plan_import(
    places: &Places,
    request: &ScopeRequest,
    source: &Path,
) -> Result<ImportPlan, String> {
    let (name, _) = read_source(source)?;
    let request = request.normalized()?;
    let base = places.base(request.project_scope)?;
    let destination = destination_for(base, &name, &request)?;
    let occupant = fsx::kind(&destination);
    if occupant.is_some_and(|kind| kind != Kind::Dir) {
        return Err(t!("error.not_replaceable", path = places.pretty(&destination)).into_owned());
    }
    if occupant.is_some() && fsx::canonical(source) == fsx::canonical(&destination) {
        return Err(t!("error.already_in_place").into_owned());
    }
    let installed = installed_agents(&places.home);
    let mut actions = vec![Action::Install {
        from: source.to_path_buf(),
        to: destination.clone(),
    }];
    actions.extend(
        references_for(&installed, base, &name, &request)?
            .into_iter()
            .map(|at| Action::Link {
                at,
                to: destination.clone(),
            }),
    );
    Ok(ImportPlan {
        actions,
        replaces: occupant.is_some(),
    })
}

/// 导入，返回装好后的目录，界面直接展开它。
pub fn import(places: &Places, request: &ScopeRequest, source: &Path) -> Result<PathBuf, String> {
    let plan = plan_import(places, request, source)?;
    execute(places, &plan.actions)?;
    plan.actions
        .iter()
        .find_map(|action| match action {
            Action::Install { to, .. } => Some(to.clone()),
            _ => None,
        })
        .ok_or_else(|| t!("error.no_skill_dir").into_owned())
}

/// 把选中的目录装成 `destination`。先复制到同目录下的暂存区：点开头，扫描会跳过；和目标同卷，
/// 交换才是原子的。目标已有实体目录时交换两边，已经指着目标路径的兼容链接全程不悬空，换下来
/// 的旧版本移进回收目录。
fn install_tree(
    places: &Places,
    trash: &Path,
    source: &Path,
    destination: &Path,
) -> Result<(), String> {
    let occupant = fsx::kind(destination);
    if occupant.is_some_and(|kind| kind != Kind::Dir) {
        return Err(t!("error.not_replaceable", path = places.pretty(destination)).into_owned());
    }
    if occupant.is_some() && fsx::canonical(source) == fsx::canonical(destination) {
        return Err(t!("error.already_in_place").into_owned());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| t!("error.no_skill_dir").into_owned())?;
    let staging = parent.join(".staging");
    std::fs::create_dir_all(&staging).map_err(io_error)?;
    let millis = chrono::Utc::now().timestamp_millis();
    let staged = staging.join(format!("import-{millis}"));
    let name = fsx::file_name(destination);
    let result = (|| {
        fsx::copy_tree(source, &staged).map_err(io_error)?;
        if !staged.join("SKILL.md").is_file() {
            fsx::move_into(trash, &staged, &name).map_err(io_error)?;
            return Err(t!("error.copy_lost_skill_md").into_owned());
        }
        if occupant.is_none() {
            std::fs::rename(&staged, destination).map_err(io_error)
        } else {
            fsx::exchange(&staged, destination).map_err(io_error)?;
            fsx::move_into(trash, &staged, &name)
                .map(|_| ())
                .map_err(io_error)
        }
    })();
    if result.is_err() {
        // 暂存的是自己刚复制出来的，留着只会挡下一次导入。
        let _ = std::fs::remove_dir_all(&staged);
    }
    let _ = std::fs::remove_dir(&staging);
    result
}

/// 路径落在哪家 agent 的技能目录里。预览按 agent 说话，用户不用懂「链接」。
fn agent_of_path(path: &Path) -> Option<&'static str> {
    let text = path.to_string_lossy();
    AGENTS
        .iter()
        .find(|agent| {
            [agent.user_dir, agent.project_dir]
                .into_iter()
                .flatten()
                .any(|dir| text.contains(&format!("/{dir}/")))
        })
        .map(|agent| agent.label)
}

/// 一步操作的一句话说明。
pub fn describe(places: &Places, action: &Action) -> String {
    match action {
        Action::Install { to, .. } => t!("step.copy", path = places.pretty(to)).into_owned(),
        Action::Move { from, to } => t!(
            "step.move",
            from = places.pretty(from),
            to = places.pretty(to)
        )
        .into_owned(),
        Action::Link { at, .. } => match agent_of_path(at) {
            Some(agent) => t!("step.link_for", agent = agent, path = places.pretty(at)),
            None => t!("step.link", path = places.pretty(at)),
        }
        .into_owned(),
        Action::Unlink { at } => match agent_of_path(at) {
            Some(agent) => t!("step.unlink_for", agent = agent, path = places.pretty(at)),
            None => t!("step.unlink", path = places.pretty(at)),
        }
        .into_owned(),
        Action::Trash { from } => t!("step.trash", path = places.pretty(from)).into_owned(),
    }
}
