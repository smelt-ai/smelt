//! 扫描：用户级和项目级各有哪些技能、各家 agent 读不读得到。
//!
//! `.agents/skills` 是本体所在的通用位置；agent 目录里指回它的软链是派生的兼容链接。软链
//! 一律是「引用」，不管它躺在哪个目录：把软链当成实体副本，同名处理时用户选软链保留、
//! 真身被移进回收目录，技能当场消失。
//!
//! 作用范围由「躺在哪个目录」表达，磁盘是唯一真相，不另存元数据。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::agents::{self, AGENTS};
use crate::fsx::{self, Kind};

pub const UNIVERSAL: &str = ".agents/skills";
/// smelt 早期的私有目录，没有 agent 读它。
pub const LEGACY: &str = ".smelt/skills";

/// 读写要用到的几个位置。`home` 和 `project` 都是解开软链后的真实路径：扫描结果里的目录
/// 都是这样，比较时两边一致。
#[derive(Clone, Debug)]
pub struct Places {
    pub home: PathBuf,
    /// 换掉、删掉的去这里，能手动找回。
    pub trash: PathBuf,
    /// 当前项目在本机时的根目录。
    pub project: Option<PathBuf>,
}

impl Places {
    /// 这一层级的根：用户级是主目录，项目级是项目根。
    pub fn base(&self, project_scope: bool) -> Result<&Path, String> {
        if project_scope {
            self.project
                .as_deref()
                .ok_or_else(|| rust_i18n::t!("error.no_project").into_owned())
        } else {
            Ok(&self.home)
        }
    }

    /// 把主目录前缀缩成 `~`。
    pub fn pretty(&self, path: &Path) -> String {
        pretty(&self.home, path)
    }
}

pub fn pretty(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// 技能放在哪类目录里。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Universal,
    Legacy,
    /// 某家 agent 自己的目录（值是能力表里的名字）。
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
    /// 自己读 `.agents/skills`。
    Reads,
    /// 经兼容链接。
    Linked,
    /// 本体就在它的目录。
    Owner,
    /// 该有兼容链接却没有：还不能用。
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
    /// 这一层级的根（主目录或项目根）。
    pub base: PathBuf,
    pub dir: PathBuf,
    pub placement: Placement,
    pub modified: Option<SystemTime>,
    /// 经链接（单条或整个目录）读到它的 agent。
    pub bridges: Vec<&'static str>,
    /// 在用、需要兼容链接却还没有的 agent。
    pub missing_bridges: Vec<&'static str>,
    pub coverage: Vec<Coverage>,
    /// 同名的其余几份，各自独立，不是指向它的链接。
    pub conflicts: Vec<SkillCopy>,
}

impl Skill {
    /// 它自己这一份，和副本同样的形状，挑「以哪份为准」时并排列出。
    pub fn as_copy(&self) -> SkillCopy {
        SkillCopy {
            name: self.name.clone(),
            description: self.description.clone(),
            dir: self.dir.clone(),
            placement: self.placement,
            modified: self.modified,
        }
    }
}

/// 指向的东西已经不在的软链。不属于任何技能，单独列、单独清。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokenLink {
    pub path: PathBuf,
    pub project_scope: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Scan {
    /// 项目级在前，同级按名字。
    pub skills: Vec<Skill>,
    pub broken: Vec<BrokenLink>,
    /// 这台机器上在用的 agent。
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

/// 在用的 agent：只看用户主目录里有没有它的配置目录。项目里的 `.claude/` 往往是用过之后才
/// 出现，拿项目判断会让第一个项目级技能一条链接都建不出来。
pub fn installed_agents(home: &Path) -> BTreeSet<&'static str> {
    AGENTS
        .iter()
        .filter(|agent| home.join(agent.home).is_dir())
        .map(|agent| agent.label)
        .collect()
}

/// 需要补兼容链接的 agent：在用、有自己的目录、自己不读 `.agents/skills`。
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

/// 一个层级里要扫的技能根目录。
pub struct Root {
    pub path: PathBuf,
    pub placement: Placement,
    real: PathBuf,
    /// 整个技能目录软链到这里的 agent（如 `.claude/skills -> ../.agents/skills`）：里面是同一批
    /// 技能，不另算一处存放，只记下这些 agent 经整个目录读到它们。
    aliases: Vec<&'static str>,
}

/// 顺序同 [`Placement::rank`]。根目录解开后就是前面某个根目录时，记成别名，不再扫一遍。
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

/// 读一个目录的 `SKILL.md`。读不到、名字为空就不算技能。
fn read_copy(path: &Path, placement: Placement) -> Option<SkillCopy> {
    let skill_md = path.join("SKILL.md");
    let text = std::fs::read_to_string(&skill_md).ok()?;
    let frontmatter = Frontmatter::parse(&text);
    let name = frontmatter
        .name
        .unwrap_or_else(|| fsx::file_name(path))
        .trim()
        .to_string();
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
        // 点开头的是暂存、隐藏目录，不是技能。
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
                    // 解析出错（比如没有权限）：读不了，也不能当成失效链接去清理。
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

/// 在用的 agent 里，哪些读得到这份技能，怎么读到的。旧位置谁都读不到。
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

    // 指向这一层级之外的软链，本身就是一份独立存在的技能。
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

    // 按名字分组，保持第一次出现的先后。
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

/// `SKILL.md` 开头 YAML frontmatter 里的 `name` 和 `description`。只要这两个标量，不值得为此
/// 拉一个 YAML 库，但要认块标量：技能大量用 `description: |`、`>`。
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
        // 正在读的键、它的值、续行怎么接（`|` 保留换行，别的折成空格）。
        let mut pending: Option<(&str, String, &str)> = None;
        let finish = |pending: &mut Option<(&str, String, &str)>, out: &mut Self| {
            if let Some((key, value, _)) = pending.take() {
                let value = unquote(value.trim()).to_string();
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
                _ => (key, value.to_string(), " "),
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
