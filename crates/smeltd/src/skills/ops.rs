//! 技能写操作与计划生成：预览和执行走同一套动作序列。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::agents::{self, AGENTS, Agent};
use super::fsx::{self, Kind};
use super::scan::{
    self, BrokenLink, Frontmatter, LEGACY, Placement, Places, Skill, UNIVERSAL, bridge_agents,
    installed_agents, skill_roots,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Install { from: PathBuf, to: PathBuf },
    Move { from: PathBuf, to: PathBuf },
    Link { at: PathBuf, to: PathBuf },
    Unlink { at: PathBuf },
    Trash { from: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRequest {
    pub universal: bool,
    pub labels: Vec<String>,
    pub project_scope: bool,
}

impl ScopeRequest {
    fn normalized(&self) -> Result<NormalizedScope, String> {
        let labels: Vec<&'static str> = AGENTS
            .iter()
            .filter(|agent| self.labels.iter().any(|l| l == agent.label))
            .map(|agent| agent.label)
            .collect();
        if !self.universal && labels.is_empty() {
            return Err("Choose at least one agent, or use Universal".into());
        }
        Ok(NormalizedScope {
            universal: self.universal,
            labels,
            project_scope: self.project_scope,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedScope {
    universal: bool,
    labels: Vec<&'static str>,
    project_scope: bool,
}

fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

fn agent_root(base: &Path, agent: &Agent, project_scope: bool) -> Result<PathBuf, String> {
    agent
        .dir(project_scope)
        .map(|dir| base.join(dir))
        .ok_or_else(|| {
            format!(
                "{} only reads {UNIVERSAL} and has no folder of its own. Use Universal instead",
                agent.label
            )
        })
}

fn destination_for(base: &Path, name: &str, request: &NormalizedScope) -> Result<PathBuf, String> {
    if request.universal {
        return Ok(base.join(UNIVERSAL).join(name));
    }
    let primary = request
        .labels
        .first()
        .and_then(|label| agents::find(label))
        .ok_or_else(|| "Unknown agent".to_owned())?;
    Ok(agent_root(base, primary, request.project_scope)?.join(name))
}

fn references_for(
    installed: &BTreeSet<&'static str>,
    base: &Path,
    name: &str,
    request: &NormalizedScope,
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
            return Err(format!(
                "{} is a real folder and can't be replaced by a compatibility link",
                at.display()
            ));
        }
    }
    fsx::symlink(target, at).map_err(io_error)
}

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

pub fn repair_bridges(places: &Places, skill: &Skill) -> Result<(), String> {
    if matches!(skill.placement, Placement::Agent(_)) {
        return Err("Agent-specific skills have no compatibility links to repair".into());
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

pub fn prune_links(broken: &[BrokenLink]) -> Result<(), String> {
    for link in broken {
        if fsx::kind(&link.path) == Some(Kind::Symlink) {
            fsx::remove_link(&link.path).map_err(io_error)?;
        }
    }
    Ok(())
}

pub fn locate(places: &Places, dir: &Path) -> Result<Skill, String> {
    scan::scan(places)
        .skills
        .into_iter()
        .find(|skill| skill.dir == dir)
        .ok_or_else(|| "This skill is gone. Refresh the list".to_owned())
}

fn plan_scope_change(
    places: &Places,
    entry: &Skill,
    request: &ScopeRequest,
) -> Result<Vec<Action>, String> {
    if !entry.conflicts.is_empty() {
        return Err("This skill has copies with the same name. Resolve the conflict first".into());
    }
    let request = request.normalized()?;
    let destination_base = places.base(request.project_scope)?;
    let destination = destination_for(destination_base, &entry.name, &request)?;
    let occupant = fsx::kind(&destination);
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
                return Err(format!(
                    "A skill with the same name is already at {}",
                    places.pretty(&destination)
                ));
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

pub fn plan_scope(
    places: &Places,
    dir: &Path,
    request: &ScopeRequest,
) -> Result<Vec<Action>, String> {
    plan_scope_change(places, &locate(places, dir)?, request)
}

pub fn apply_scope(places: &Places, dir: &Path, request: &ScopeRequest) -> Result<PathBuf, String> {
    let entry = locate(places, dir)?;
    let actions = plan_scope_change(places, &entry, request)?;
    execute(places, &actions)?;
    let request = request.normalized()?;
    destination_for(places.base(request.project_scope)?, &entry.name, &request)
}

pub fn resolve_conflict(places: &Places, dir: &Path, keep: &Path) -> Result<(), String> {
    let entry = locate(places, dir)?;
    if entry.conflicts.is_empty() {
        return Err("No same-name copies to resolve".into());
    }
    let copies: Vec<PathBuf> = std::iter::once(entry.dir.clone())
        .chain(entry.conflicts.iter().map(|copy| copy.dir.clone()))
        .collect();
    if !copies.iter().any(|copy| copy == keep) {
        return Err("The chosen copy doesn't exist".into());
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

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Migration {
    pub migrated: Vec<String>,
    pub skipped: Vec<String>,
}

pub fn migrate_legacy(places: &Places) -> Result<Migration, String> {
    let mut outcome = Migration::default();
    let mut failed: BTreeSet<PathBuf> = BTreeSet::new();
    for guard in 0.. {
        if guard == 1000 {
            return Err("Migration ran too many rounds and was stopped".into());
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

pub fn validate_skill_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.is_empty() {
        return Err("The name can't be empty".into());
    }
    if name.chars().count() > 64 {
        return Err("The name is too long (64 characters at most)".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Use only letters, digits, hyphens (-) and underscores (_)".into());
    }
    if name.starts_with('-') || name.starts_with('_') {
        return Err("The name can't start with - or _".into());
    }
    Ok(name.to_owned())
}

fn read_source(source: &Path) -> Result<(String, String), String> {
    let text = std::fs::read_to_string(source.join("SKILL.md"))
        .map_err(|error| format!("The chosen folder has no readable SKILL.md: {error}"))?;
    let frontmatter = Frontmatter::parse(&text);
    let name = validate_skill_name(
        frontmatter
            .name
            .as_deref()
            .unwrap_or(&fsx::file_name(source)),
    )?;
    Ok((name, frontmatter.description.unwrap_or_default()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceInfo {
    pub name: String,
    pub description: String,
    pub has_global: bool,
}

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
        return Err(format!(
            "The existing item with this name isn't a skill folder that can be replaced: {}",
            places.pretty(&destination)
        ));
    }
    if occupant.is_some() && fsx::canonical(source) == fsx::canonical(&destination) {
        return Err(
            "The chosen folder is already where it would be installed; no need to import it".into(),
        );
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

pub fn import(places: &Places, request: &ScopeRequest, source: &Path) -> Result<PathBuf, String> {
    let plan = plan_import(places, request, source)?;
    execute(places, &plan.actions)?;
    plan.actions
        .iter()
        .find_map(|action| match action {
            Action::Install { to, .. } => Some(to.clone()),
            _ => None,
        })
        .ok_or_else(|| "The skill folder is missing".to_owned())
}

fn install_tree(
    places: &Places,
    trash: &Path,
    source: &Path,
    destination: &Path,
) -> Result<(), String> {
    let occupant = fsx::kind(destination);
    if occupant.is_some_and(|kind| kind != Kind::Dir) {
        return Err(format!(
            "The existing item with this name isn't a skill folder that can be replaced: {}",
            places.pretty(destination)
        ));
    }
    if occupant.is_some() && fsx::canonical(source) == fsx::canonical(destination) {
        return Err(
            "The chosen folder is already where it would be installed; no need to import it".into(),
        );
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "The skill folder is missing".to_owned())?;
    let staging = parent.join(".staging");
    std::fs::create_dir_all(&staging).map_err(io_error)?;
    let millis = chrono::Utc::now().timestamp_millis();
    let staged = staging.join(format!("import-{millis}"));
    let name = fsx::file_name(destination);
    let result = (|| {
        fsx::copy_tree(source, &staged).map_err(io_error)?;
        if !staged.join("SKILL.md").is_file() {
            fsx::move_into(trash, &staged, &name).map_err(io_error)?;
            return Err("SKILL.md is missing after copying".into());
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
        let _ = std::fs::remove_dir_all(&staged);
    }
    let _ = std::fs::remove_dir(&staging);
    result
}

/// 路径落在哪家 agent 的技能目录里（用于界面按 agent 说话）。
pub fn agent_of_path(path: &Path) -> Option<&'static str> {
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
