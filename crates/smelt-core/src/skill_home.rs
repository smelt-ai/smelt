//! 用户级技能放在哪、怎么搬：以 `~/.agents/skills` 为准。
//!
//! `~/.agents/skills` 是跨 agent 的通用位置（Pi、Codex、Copilot、Cursor、Gemini CLI 等都读），
//! 导入、移动都落到这里。Claude Code 只读自己的 `~/.claude/skills`，所以给它每个技能补一条
//! 指回通用目录的链接（桥）。桥是派生出来的：每次扫描、每次改动后重算，不是用户数据。
//!
//! 旧位置 `~/.pi/agent/skills`（以前从这里导入的）和 `~/.smelt/skills`（更早的私有目录）照常
//! 扫，标成旧位置，用户点一下移到通用目录。同名多份不自动合并，由用户挑一份为准。
//!
//! 换掉、删掉的一律移进 `~/.smelt/trash/skills/<时间>/`，能手动找回。软链只是引用：删链接
//! 不碰它指着的东西，搬本体时把指着它的链接一起清掉。

use std::path::{Component, Path, PathBuf};

use smelt_model::pi_plugin_catalog::{PiPlugin, PiPluginKind, plugin_id};

/// 用户级技能的几个位置。测试里换成临时目录。
#[derive(Clone, Debug)]
pub struct SkillHome {
    /// 通用目录 `~/.agents/skills`：导入、移动的落点。
    pub shared: PathBuf,
    /// 旧位置，按扫描先后：`~/.pi/agent/skills`、`~/.smelt/skills`。
    pub legacy: Vec<PathBuf>,
    /// Claude Code 的配置目录 `~/.claude`。不存在就是没在用，不替它建桥。
    pub claude_home: PathBuf,
    /// 换掉、删掉的去这里。
    pub trash: PathBuf,
}

impl SkillHome {
    /// 这台机器上的位置。取不到用户主目录时 `None`。
    pub fn current() -> Option<Self> {
        let home = dirs::home_dir()?;
        let smelt = smelt_paths::smelt_home().unwrap_or_else(|| home.join(".smelt"));
        Some(Self {
            shared: home.join(".agents").join("skills"),
            legacy: vec![
                crate::pi_model_settings::pi_agent_dir().join("skills"),
                smelt.join("skills"),
            ],
            claude_home: crate::claude_paths::config_home(),
            trash: smelt.join("trash").join("skills"),
        })
    }

    /// 扫描顺序：通用目录在前，同名时它那份为准。
    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.shared.as_path()).chain(self.legacy.iter().map(PathBuf::as_path))
    }

    fn claude_skills(&self) -> PathBuf {
        self.claude_home.join("skills")
    }

    /// 同名的合成一条：第一份为准，别的独立副本记进 `duplicates`；指着同一份的链接不算。
    /// Claude Code 目录里同名的真目录也算一份：它在，Claude Code 就读不到通用目录那份。
    pub fn merge(&self, skills: Vec<PiPlugin>) -> Vec<PiPlugin> {
        let mut out: Vec<PiPlugin> = Vec::new();
        for mut skill in skills {
            if let Some(primary) = out.iter_mut().find(|it| it.id == skill.id) {
                if skill.broken.is_none()
                    && primary.broken.is_none()
                    && !same_file(&primary.path, &skill.path)
                    && !primary
                        .duplicates
                        .iter()
                        .any(|path| same_file(path, &skill.path))
                {
                    primary.duplicates.push(skill.path);
                }
                continue;
            }
            skill.legacy_location = skill.broken.is_none() && !skill.path.starts_with(&self.shared);
            out.push(skill);
        }
        let claude = self.claude_skills();
        for skill in &mut out {
            let Some(name) = skill.path.file_name() else {
                continue;
            };
            let candidate = claude.join(name);
            if is_real_entry(&candidate)
                && !same_file(&candidate, &skill.path)
                && !skill.duplicates.iter().any(|it| same_file(it, &candidate))
            {
                skill.duplicates.push(candidate);
            }
        }
        out
    }

    /// 给 Claude Code 补齐指回通用目录的链接，清掉指进通用目录却已悬空的。只动链接，
    /// 不碰真目录：同名真目录是一份冲突，留给用户挑。Claude Code 没在用时什么都不做。
    pub fn sync_bridges(&self) -> Result<(), String> {
        // Windows 上建目录链接要权限，先不替 Claude Code 建桥。
        if cfg!(not(unix)) || !self.claude_home.is_dir() {
            return Ok(());
        }
        let claude = self.claude_skills();
        // 整个目录链到了通用目录：Claude Code 已经全读到，再补单条链接会落进本体目录。
        if let (Ok(a), Ok(b)) = (
            std::fs::canonicalize(&claude),
            std::fs::canonicalize(&self.shared),
        ) && a == b
        {
            return Ok(());
        }
        if let Ok(entries) = std::fs::read_dir(&claude) {
            for entry in entries.flatten() {
                let link = entry.path();
                if let Some(target) = link_target(&link)
                    && target.starts_with(&self.shared)
                    && !link.exists()
                {
                    remove_entry(&link)?;
                }
            }
        }
        let Ok(entries) = std::fs::read_dir(&self.shared) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') || !path.join("SKILL.md").is_file() {
                continue;
            }
            let bridge = claude.join(&name);
            if std::fs::symlink_metadata(&bridge).is_ok() {
                continue;
            }
            std::fs::create_dir_all(&claude)
                .map_err(|error| format!("couldn't create {}: {error}", claude.display()))?;
            symlink_dir(&path, &bridge)?;
        }
        Ok(())
    }

    /// 导入到 `root`（技能是通用目录）。先复制进同目录下的暂存处（点开头，扫描跳过），
    /// 再换上去；已有同名的整个换下来进回收目录。返回装好的路径。
    pub fn install(&self, root: &Path, source: &Path) -> Result<PathBuf, String> {
        let name = source
            .file_name()
            .ok_or_else(|| "can't determine the plugin name".to_string())?;
        std::fs::create_dir_all(root)
            .map_err(|error| format!("couldn't create the directory: {error}"))?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let staged = root.join(format!(".import-{token}"));
        let copied = if source.is_dir() {
            copy_dir(source, &staged)
        } else {
            std::fs::copy(source, &staged)
                .map(|_| ())
                .map_err(|error| format!("copy failed: {error}"))
        };
        if let Err(error) = copied {
            let _ = remove_entry(&staged);
            return Err(error);
        }
        let target = root.join(name);
        let replaced = if std::fs::symlink_metadata(&target).is_ok() {
            let aside = root.join(format!(".replaced-{token}"));
            if let Err(error) = std::fs::rename(&target, &aside) {
                let _ = remove_entry(&staged);
                return Err(format!("couldn't replace {}: {error}", target.display()));
            }
            Some(aside)
        } else {
            None
        };
        if let Err(error) = std::fs::rename(&staged, &target) {
            if let Some(aside) = &replaced {
                let _ = std::fs::rename(aside, &target);
            }
            let _ = remove_entry(&staged);
            return Err(format!("couldn't install {}: {error}", target.display()));
        }
        if let Some(aside) = replaced {
            self.trash(&aside, name)?;
        }
        Ok(target)
    }

    /// 删掉一个技能或扩展。链接只删链接本身；本体进回收目录，指着它的链接一起清掉。
    pub fn remove(&self, path: &Path) -> Result<(), String> {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|error| format!("couldn't read {}: {error}", path.display()))?;
        if meta.file_type().is_symlink() {
            return remove_entry(path);
        }
        let name = path
            .file_name()
            .ok_or_else(|| format!("not a plugin path: {}", path.display()))?;
        self.trash(path, name)?;
        self.forget_links_to(path)
    }

    /// 以 `keep` 这份为准：同名的别的副本进回收目录（链接直接删），`keep` 不在通用目录就
    /// 移过去，再补桥。`copies` 是这个技能现有的全部副本，`keep` 必须在里面。
    pub fn adopt(&self, keep: &Path, copies: &[PathBuf]) -> Result<PathBuf, String> {
        if !copies.iter().any(|copy| copy == keep) {
            return Err(format!("{} isn't a copy of this skill", keep.display()));
        }
        let name = keep
            .file_name()
            .ok_or_else(|| format!("not a skill path: {}", keep.display()))?
            .to_owned();
        for copy in copies.iter().filter(|copy| *copy != keep) {
            self.remove(copy)?;
        }
        let target = self.shared.join(&name);
        if keep != target {
            std::fs::create_dir_all(&self.shared)
                .map_err(|error| format!("couldn't create the directory: {error}"))?;
            // 留的是一条链接：在通用目录建一条指向同一处的，相对链接搬过去会断。
            if let Some(points_at) = link_target(keep) {
                symlink_dir(&points_at, &target)?;
                remove_entry(keep)?;
            } else {
                std::fs::rename(keep, &target).map_err(|error| {
                    format!(
                        "couldn't move {} to {}: {error}",
                        keep.display(),
                        target.display()
                    )
                })?;
                self.forget_links_to(keep)?;
            }
        }
        self.sync_bridges()?;
        Ok(target)
    }

    /// 移进 `trash/<时间>-<随机>/<name>`。跨卷改名不了时复制过去再删。
    fn trash(&self, path: &Path, name: &std::ffi::OsStr) -> Result<(), String> {
        let bin = self.trash.join(format!(
            "{}-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S"),
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir_all(&bin)
            .map_err(|error| format!("couldn't create {}: {error}", bin.display()))?;
        let to = bin.join(name);
        if std::fs::rename(path, &to).is_ok() {
            return Ok(());
        }
        if path.is_dir() {
            copy_dir(path, &to)?;
        } else {
            std::fs::copy(path, &to).map_err(|error| format!("copy failed: {error}"))?;
        }
        remove_entry(path)
    }

    /// 各处指着 `path` 的链接（本体已经不在原处了）。
    fn forget_links_to(&self, path: &Path) -> Result<(), String> {
        let gone = normalize(path);
        let claude = self.claude_skills();
        for root in self.roots().chain(std::iter::once(claude.as_path())) {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let link = entry.path();
                if link_target(&link).is_some_and(|target| normalize(&target) == gone) {
                    remove_entry(&link)?;
                }
            }
        }
        Ok(())
    }
}

/// 一条技能的全部副本：为准的那份加上同名的另几份。
pub fn copies(skill: &PiPlugin) -> Vec<PathBuf> {
    std::iter::once(skill.path.clone())
        .chain(skill.duplicates.iter().cloned())
        .collect()
}

/// 技能 id 换成它在通用目录里的样子：根目录下的 `.md` 技能按文件名。
pub fn skill_id_at(path: &Path) -> Option<String> {
    let name = if path.is_file() {
        path.file_stem()
    } else {
        path.file_name()
    }?;
    Some(plugin_id(PiPluginKind::Skill, &name.to_string_lossy()))
}

/// 软链指向的路径（相对的按链接所在目录解开）；不是软链是 `None`。
fn link_target(link: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(link).ok()?;
    Some(if target.is_absolute() {
        target
    } else {
        link.parent().unwrap_or(Path::new("")).join(target)
    })
}

/// 存在、且自己不是软链。
fn is_real_entry(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.file_type().is_symlink())
}

/// 两个路径解开后是同一份。有一个解不开就不算。
fn same_file(a: &Path, b: &Path) -> bool {
    matches!(
        (std::fs::canonicalize(a), std::fs::canonicalize(b)),
        (Ok(a), Ok(b)) if a == b
    )
}

/// 只按字面去掉 `.` 和 `..`：目标可能已经不在，不能 canonicalize。
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn remove_entry(path: &Path) -> Result<(), String> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    let result = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|error| format!("couldn't delete {}: {error}", path.display()))
}

/// 复制目录，跳过软链：软链是别人的引用，不是技能的内容。
pub(crate) fn copy_dir(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target)
        .map_err(|error| format!("couldn't create the directory: {error}"))?;
    let entries = std::fs::read_dir(source)
        .map_err(|error| format!("couldn't read the directory: {error}"))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            continue;
        };
        let to = target.join(entry.file_name());
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|error| format!("copy failed: {error}"))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|error| format!("couldn't link {}: {error}", link.display()))
}

#[cfg(not(unix))]
fn symlink_dir(_target: &Path, link: &Path) -> Result<(), String> {
    Err(format!(
        "can't create links on this system: {}",
        link.display()
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture {
        _dir: tempfile::TempDir,
        home: SkillHome,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let home = SkillHome {
            shared: root.join(".agents/skills"),
            legacy: vec![root.join(".pi/agent/skills"), root.join(".smelt/skills")],
            claude_home: root.join(".claude"),
            trash: root.join(".smelt/trash/skills"),
        };
        Fixture { _dir: dir, home }
    }

    fn skill(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {body}\n---\n"),
        )
        .unwrap();
        path
    }

    fn plugin(path: &Path) -> PiPlugin {
        PiPlugin {
            id: skill_id_at(path).unwrap(),
            kind: PiPluginKind::Skill,
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            description: String::new(),
            path: path.to_path_buf(),
            origin: String::new(),
            broken: None,
            builtin: false,
            legacy_location: false,
            duplicates: Vec::new(),
        }
    }

    fn trashed(home: &SkillHome) -> Vec<String> {
        let mut names = Vec::new();
        for bin in std::fs::read_dir(&home.trash)
            .into_iter()
            .flatten()
            .flatten()
        {
            for entry in std::fs::read_dir(bin.path()).unwrap().flatten() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        names
    }

    #[test]
    fn same_name_copies_merge_and_links_to_the_same_copy_do_not_count() {
        let Fixture { _dir, home } = fixture();
        let shared = skill(&home.shared, "review", "shared");
        let pi = skill(&home.legacy[0], "review", "pi");
        std::fs::create_dir_all(&home.legacy[1]).unwrap();
        symlink(&shared, home.legacy[1].join("review")).unwrap();
        let claude = skill(&home.claude_skills(), "review", "claude");
        let only_pi = skill(&home.legacy[0], "notes", "pi");

        let merged = home.merge(vec![
            plugin(&shared),
            plugin(&pi),
            plugin(&home.legacy[1].join("review")),
            plugin(&only_pi),
        ]);
        assert_eq!(merged.len(), 2);
        let review = &merged[0];
        assert_eq!(review.path, shared);
        assert!(!review.legacy_location);
        assert_eq!(review.duplicates, vec![pi, claude]);
        assert!(merged[1].legacy_location, "只在旧位置的标出来");
        assert!(merged[1].duplicates.is_empty());
    }

    #[test]
    fn bridges_follow_the_shared_dir_and_leave_real_dirs_alone() {
        let Fixture { _dir, home } = fixture();
        let shared = skill(&home.shared, "review", "shared");
        skill(&home.shared, "plan", "shared");
        // Claude Code 没在用：不建桥。
        home.sync_bridges().unwrap();
        assert!(!home.claude_skills().exists());

        std::fs::create_dir_all(&home.claude_home).unwrap();
        let own = skill(&home.claude_skills(), "plan", "claude");
        home.sync_bridges().unwrap();
        let bridge = home.claude_skills().join("review");
        assert_eq!(std::fs::read_link(&bridge).unwrap(), shared);
        assert!(is_real_entry(&own), "同名真目录留给用户挑");

        std::fs::remove_dir_all(&shared).unwrap();
        home.sync_bridges().unwrap();
        assert!(std::fs::symlink_metadata(&bridge).is_err(), "悬空的桥清掉");
    }

    #[test]
    fn a_whole_dir_link_needs_no_bridges() {
        let Fixture { _dir, home } = fixture();
        skill(&home.shared, "review", "shared");
        std::fs::create_dir_all(&home.claude_home).unwrap();
        symlink(&home.shared, home.claude_skills()).unwrap();
        home.sync_bridges().unwrap();
        assert!(std::fs::symlink_metadata(home.shared.join("review/review")).is_err());
    }

    #[test]
    fn installing_over_a_skill_swaps_it_and_trashes_the_old_one() {
        let Fixture { _dir, home } = fixture();
        let source_dir = tempfile::tempdir().unwrap();
        let source = skill(source_dir.path(), "review", "new");
        symlink("/nowhere", source.join("stray")).unwrap();
        skill(&home.shared, "review", "old");
        std::fs::create_dir_all(&home.claude_home).unwrap();
        home.sync_bridges().unwrap();

        let installed = home.install(&home.shared, &source).unwrap();
        assert_eq!(installed, home.shared.join("review"));
        let text = std::fs::read_to_string(installed.join("SKILL.md")).unwrap();
        assert!(text.contains("new"));
        assert!(!installed.join("stray").exists(), "软链不复制");
        assert!(
            home.claude_skills().join("review/SKILL.md").is_file(),
            "桥一直指着同一个路径"
        );
        assert_eq!(trashed(&home), vec!["review"]);
        let leftovers: Vec<_> = std::fs::read_dir(&home.shared)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("review")]);
    }

    #[test]
    fn removing_trashes_the_body_and_clears_links_to_it() {
        let Fixture { _dir, home } = fixture();
        let shared = skill(&home.shared, "review", "shared");
        std::fs::create_dir_all(&home.claude_home).unwrap();
        home.sync_bridges().unwrap();
        std::fs::create_dir_all(&home.legacy[0]).unwrap();
        let pi_link = home.legacy[0].join("review");
        symlink("../../../.agents/skills/review", &pi_link).unwrap();

        home.remove(&shared).unwrap();
        assert!(!shared.exists());
        assert!(std::fs::symlink_metadata(&pi_link).is_err());
        assert!(std::fs::symlink_metadata(home.claude_skills().join("review")).is_err());
        assert_eq!(trashed(&home), vec!["review"]);
    }

    #[test]
    fn removing_a_link_keeps_what_it_points_at() {
        let Fixture { _dir, home } = fixture();
        let shared = skill(&home.shared, "review", "shared");
        std::fs::create_dir_all(&home.legacy[0]).unwrap();
        let link = home.legacy[0].join("review");
        symlink(&shared, &link).unwrap();
        home.remove(&link).unwrap();
        assert!(shared.join("SKILL.md").is_file());
        assert!(trashed(&home).is_empty());
    }

    #[test]
    fn adopting_a_legacy_copy_moves_it_to_the_shared_dir() {
        let Fixture { _dir, home } = fixture();
        let shared = skill(&home.shared, "review", "shared");
        let pi = skill(&home.legacy[0], "review", "pi");
        std::fs::create_dir_all(&home.claude_home).unwrap();
        let claude = skill(&home.claude_skills(), "review", "claude");
        let copies = vec![shared.clone(), pi.clone(), claude.clone()];

        let kept = home.adopt(&pi, &copies).unwrap();
        assert_eq!(kept, shared);
        let text = std::fs::read_to_string(shared.join("SKILL.md")).unwrap();
        assert!(text.contains("pi"));
        assert!(!pi.exists());
        assert_eq!(std::fs::read_link(&claude).unwrap(), shared, "换成桥");
        assert_eq!(trashed(&home), vec!["review", "review"]);

        assert!(home.adopt(&home.legacy[0].join("x"), &copies).is_err());
    }

    #[test]
    fn root_markdown_skills_keep_their_stem_as_id() {
        let Fixture { _dir, home } = fixture();
        std::fs::create_dir_all(&home.legacy[0]).unwrap();
        let file = home.legacy[0].join("brief.md");
        std::fs::write(&file, "---\ndescription: x\n---\n").unwrap();
        assert_eq!(skill_id_at(&file).as_deref(), Some("skill:brief"));
        let kept = home.adopt(&file, std::slice::from_ref(&file)).unwrap();
        assert_eq!(kept, home.shared.join("brief.md"));
        assert!(kept.is_file());
    }
}
