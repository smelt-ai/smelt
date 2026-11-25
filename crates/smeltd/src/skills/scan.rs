//! 扫描：用户级和项目级各有哪些技能、各家 agent 读不读得到。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::agents::{self, AGENTS};
use super::fsx::{self, Kind};

pub const UNIVERSAL: &str = ".agents/skills";
/// smelt 早期的私有目录，没有 agent 读它。
pub const LEGACY: &str = ".smelt/skills";

/// 读写要用到的几个位置。`home` 和 `project` 都是解开软链后的真实路径。
#[derive(Clone, Debug)]
pub struct Places {
    pub home: PathBuf,
    /// 换掉、删掉的去这里，能手动找回。
    pub trash: PathBuf,
    /// 当前项目在这台机器上的根目录。
    pub project: Option<PathBuf>,
}

impl Places {
    pub fn resolve(project: Option<&str>) -> Result<Self, String> {
        let home = dirs::home_dir()
            .map(|h| fsx::canonical(&h))
            .ok_or_else(|| "Couldn't find the home folder".to_owned())?;
        let trash = smelt_paths::smelt_home()
            .unwrap_or_else(|| home.join(".smelt"))
            .join("trash")
            .join("skills");
        let project = project
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| fsx::canonical(Path::new(p)));
        Ok(Self {
            home,
            trash,
            project,
        })
    }

    pub fn base(&self, project_scope: bool) -> Result<&Path, String> {
        if project_scope {
            self.project.as_deref().ok_or_else(|| {
                "No project is open, so project-level skills aren't available".into()
            })
        } else {
            Ok(&self.home)
        }
    }

    pub fn pretty(&self, path: &Path) -> String {
        pretty(&self.home, path)
    }
}

pub fn pretty(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

pub fn modified_ms(time: Option<SystemTime>) -> Option<i64> {
    let time = time?;
    let dur = time.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(dur.as_millis()).ok()
}

/// 技能放在哪类目录里。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Universal,
    Legacy,
    Agent(&'static str),
}

impl Placement {
    fn rank(self) -> usize {
        match self {
            Placement::Universal => 0,
            Placement::Legacy => 1,
            Placement::Agent(label) => 2 + agents::rank(label),
        }
    }
}

/// 一份技能目录。
#[derive(Clone, Debug)]
pub struct SkillCopy {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    pub placement: Placement,
    pub modified: Option<SystemTime>,
}

/// 一家在用的 agent 怎么读到这个技能。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Reads,
    Linked,
    Owner,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    pub agent: &'static str,
    pub access: Access,
}

/// 同名的一组里排第一的那份，带上它的覆盖情况和其余副本。
#[derive(Clone, Debug)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub project_scope: bool,
    pub base: PathBuf,
    pub dir: PathBuf,
    pub placement: Placement,
    pub modified: Option<SystemTime>,
    pub bridges: Vec<&'static str>,
    pub missing_bridges: Vec<&'static str>,
    pub coverage: Vec<Coverage>,
    pub conflicts: Vec<SkillCopy>,
}

/// 指向的东西已经不在的软链。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokenLink {
    pub path: PathBuf,
    pub project_scope: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub skills: Vec<Skill>,
    pub broken: Vec<BrokenLink>,
    pub installed: BTreeSet<&'static str>,
}

/// 整份状态：用户级加项目级。项目根就是主目录时只扫一次。
pub fn scan(places: &Places) -> Scan {
    let installed = installed_agents(&places.home);
    let mut skills = Vec::new();
    let mut broken = Vec::new();
    scan_scope(&installed, &places.home, false, &mut skills, &mut broken);
    if let Some(project) = &places.project
        && fsx::canonical(project) != fsx::canonical(&places.home)
    {
        scan_scope(&installed, project, true, &mut skills, &mut broken);
    }
    skills.sort_by(|left, right| {
        right
            .project_scope
            .cmp(&left.project_scope)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    Scan {
        skills,
        broken,
        installed,
    }
}

pub fn installed_agents(home: &Path) -> BTreeSet<&'static str> {
    AGENTS
        .iter()
        .filter(|agent| home.join(agent.home).is_dir())
        .map(|agent| agent.label)
        .collect()
}

pub fn bridge_agents(
    installed: &BTreeSet<&'static str>,
    project_scope: bool,
) -> Vec<&'static agents::Agent> {
    AGENTS
        .iter()
        .filter(|agent| {
            agent.dir(project_scope).is_some()
                && !agent.reads_agents_dir
                && installed.contains(agent.label)
        })
        .collect()
}

pub struct Root {
    pub path: PathBuf,
    pub placement: Placement,
    real: PathBuf,
    aliases: Vec<&'static str>,
}

pub fn skill_roots(base: &Path, project_scope: bool) -> Vec<Root> {
    let candidates = [
        (base.join(UNIVERSAL), Placement::Universal),
        (base.join(LEGACY), Placement::Legacy),
    ]
    .into_iter()
    .chain(AGENTS.iter().filter_map(|agent| {
        agent
            .dir(project_scope)
            .map(|dir| (base.join(dir), Placement::Agent(agent.label)))
    }));
    let mut roots: Vec<Root> = Vec::new();
    for (path, placement) in candidates {
        let real = fsx::canonical(&path);
        match roots.iter_mut().find(|root| root.real == real) {
            Some(target) => {
                if let Placement::Agent(label) = placement {
                    target.aliases.push(label);
                }
            }
            None => roots.push(Root {
                path,
                placement,
                real,
                aliases: Vec::new(),
            }),
        }
    }
    roots
}

fn read_copy(path: &Path, placement: Placement) -> Option<SkillCopy> {
    let skill_md = path.join("SKILL.md");
    let text = std::fs::read_to_string(&skill_md).ok()?;
    let frontmatter = Frontmatter::parse(&text);
    let name = frontmatter
        .name
        .unwrap_or_else(|| fsx::file_name(path))
        .trim()
        .to_owned();
    if name.is_empty() {
        return None;
    }
    Some(SkillCopy {
        name,
        description: frontmatter.description.unwrap_or_default(),
        dir: path.to_path_buf(),
        placement,
        modified: std::fs::metadata(&skill_md)
            .and_then(|meta| meta.modified())
            .ok(),
    })
}

struct Link {
    agent: Option<&'static str>,
    placement: Placement,
    at: PathBuf,
    target: PathBuf,
}

fn scan_root(
    root: &Root,
    project_scope: bool,
    copies: &mut Vec<SkillCopy>,
    links: &mut Vec<Link>,
    broken: &mut Vec<BrokenLink>,
) {
    let Ok(entries) = fsx::entries(&root.path) else {
        return;
    };
    let agent = match root.placement {
        Placement::Agent(label) => Some(label),
        _ => None,
    };
    for (name, kind) in entries {
        if name.starts_with('.') {
            continue;
        }
        let path = root.path.join(&name);
        match kind {
            Kind::Symlink => {
                let target = match std::fs::canonicalize(&path) {
                    Ok(target) => target,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        broken.push(BrokenLink {
                            path,
                            project_scope,
                        });
                        continue;
                    }
                    Err(_) => continue,
                };
                if !target.join("SKILL.md").is_file() {
                    broken.push(BrokenLink {
                        path,
                        project_scope,
                    });
                    continue;
                }
                links.push(Link {
                    agent,
                    placement: root.placement,
                    at: path,
                    target,
                });
            }
            Kind::Dir => {
                if let Some(copy) = read_copy(&path, root.placement) {
                    copies.push(copy);
                }
            }
            Kind::File | Kind::Other => {}
        }
    }
}

fn coverage_of(
    primary: &SkillCopy,
    bridges: &[&'static str],
    installed: &BTreeSet<&'static str>,
    project_scope: bool,
) -> Vec<Coverage> {
    if primary.placement == Placement::Legacy {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for agent in AGENTS {
        if !installed.contains(agent.label) {
            continue;
        }
        let access = match primary.placement {
            Placement::Agent(owner) if owner == agent.label => Access::Owner,
            Placement::Agent(_) if bridges.contains(&agent.label) => Access::Linked,
            Placement::Agent(_) => continue,
            _ if agent.reads_agents_dir => Access::Reads,
            _ if bridges.contains(&agent.label) => Access::Linked,
            _ if agent.dir(project_scope).is_some() => Access::Missing,
            _ => continue,
        };
        rows.push(Coverage {
            agent: agent.label,
            access,
        });
    }
    rows
}

pub fn scan_scope(
    installed: &BTreeSet<&'static str>,
    base: &Path,
    project_scope: bool,
    output: &mut Vec<Skill>,
    broken: &mut Vec<BrokenLink>,
) {
    let mut copies = Vec::new();
    let mut links = Vec::new();
    let roots = skill_roots(base, project_scope);
    for root in &roots {
        scan_root(root, project_scope, &mut copies, &mut links, broken);
    }

    let mut known: BTreeSet<PathBuf> = copies
        .iter()
        .map(|copy| fsx::canonical(&copy.dir))
        .collect();
    for link in &links {
        if known.contains(&link.target) {
            continue;
        }
        if let Some(copy) = read_copy(&link.at, link.placement) {
            copies.push(copy);
            known.insert(fsx::canonical(&link.at));
        }
    }

    let mut order: Vec<String> = Vec::new();
    let mut grouped: BTreeMap<String, Vec<SkillCopy>> = BTreeMap::new();
    for copy in copies {
        if !grouped.contains_key(&copy.name) {
            order.push(copy.name.clone());
        }
        grouped.entry(copy.name.clone()).or_default().push(copy);
    }

    let need_bridge = bridge_agents(installed, project_scope);
    for name in order {
        let Some(mut bucket) = grouped.remove(&name) else {
            continue;
        };
        bucket.sort_by_key(|copy| copy.placement.rank());
        let primary = bucket.remove(0);
        let canonical_primary = fsx::canonical(&primary.dir);
        let linked: Vec<&'static str> = links
            .iter()
            .filter(|link| link.target == canonical_primary && link.at != primary.dir)
            .filter_map(|link| link.agent)
            .collect();
        let aliases = roots
            .iter()
            .find(|root| Some(root.path.as_path()) == primary.dir.parent())
            .map(|root| root.aliases.as_slice())
            .unwrap_or_default();
        let bridges: Vec<&'static str> = AGENTS
            .iter()
            .filter(|agent| linked.contains(&agent.label) || aliases.contains(&agent.label))
            .map(|agent| agent.label)
            .collect();
        let missing_bridges = match primary.placement {
            Placement::Agent(_) => Vec::new(),
            _ => need_bridge
                .iter()
                .filter(|agent| !bridges.contains(&agent.label))
                .map(|agent| agent.label)
                .collect(),
        };
        let coverage = coverage_of(&primary, &bridges, installed, project_scope);
        output.push(Skill {
            name,
            description: primary.description,
            project_scope,
            base: base.to_path_buf(),
            dir: primary.dir,
            placement: primary.placement,
            modified: primary.modified,
            bridges,
            missing_bridges,
            coverage,
            conflicts: bucket,
        });
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
}

impl Frontmatter {
    pub fn parse(text: &str) -> Self {
        let mut lines = text.lines();
        let mut out = Self::default();
        if lines.next().map(str::trim) != Some("---") {
            return out;
        }
        let mut pending: Option<(&str, String, &str)> = None;
        let finish = |pending: &mut Option<(&str, String, &str)>, out: &mut Self| {
            if let Some((key, value, _)) = pending.take() {
                let value = unquote(value.trim()).to_owned();
                match key {
                    "name" => out.name = Some(value),
                    _ => out.description = Some(value),
                }
            }
        };
        for line in lines {
            if line.trim() == "---" {
                break;
            }
            if line.starts_with(' ') || line.starts_with('\t') {
                if let Some((_, value, joint)) = &mut pending {
                    if !value.is_empty() {
                        value.push_str(joint);
                    }
                    value.push_str(line.trim());
                }
                continue;
            }
            finish(&mut pending, &mut out);
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            if key != "name" && key != "description" {
                continue;
            }
            let value = value.trim();
            pending = Some(match value {
                "|" | "|-" | "|+" => (key, String::new(), "\n"),
                ">" | ">-" | ">+" => (key, String::new(), " "),
                _ => (key, value.to_owned(), " "),
            });
        }
        finish(&mut pending, &mut out);
        out
    }
}

fn unquote(value: &str) -> &str {
    let quoted = value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')));
    if quoted {
        &value[1..value.len() - 1]
    } else {
        value
    }
}
