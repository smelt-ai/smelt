//! 技能磁盘逻辑单测（从旧 crates/plugins/skills/src/tests.rs 完整迁入 smeltd）。

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::fsx;
use super::ops::{self, Action, ScopeRequest};
use super::scan::{self, Access, Coverage, Frontmatter, Placement, Places};

struct Home {
    root: PathBuf,
    places: Places,
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Home {
    fn new() -> Self {
        let raw = std::env::temp_dir().join(format!("smeltd-skills-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&raw).unwrap();
        let home = fsx::canonical(&raw);
        fs::create_dir(home.join(".claude")).unwrap();
        let places = Places {
            trash: home.join(".smelt/trash/skills"),
            home: home.clone(),
            project: None,
        };
        Self { root: home, places }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.places.home.join(rel)
    }

    fn link_claude_root(&self) {
        fs::create_dir_all(self.path(".agents/skills")).unwrap();
        symlink("../.agents/skills", self.path(".claude/skills")).unwrap();
    }

    fn with_project(mut self) -> Self {
        let root = self.path("work/app");
        fs::create_dir_all(&root).unwrap();
        self.places.project = Some(root);
        self
    }
}

fn write_skill(dir: &Path, name: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} skill\n---\n"),
    )
    .unwrap();
}

fn universal(project_scope: bool) -> ScopeRequest {
    ScopeRequest {
        universal: true,
        labels: Vec::new(),
        project_scope,
    }
}

fn only(label: &str, project_scope: bool) -> ScopeRequest {
    ScopeRequest {
        universal: false,
        labels: vec![label.to_owned()],
        project_scope,
    }
}

fn is_link(path: &Path) -> bool {
    fsx::kind(path) == Some(fsx::Kind::Symlink)
}

#[test]
fn whole_agent_dir_linked_is_one_skill_read_through_the_link() {
    let home = Home::new();
    home.link_claude_root();
    write_skill(&home.path(".agents/skills/demo"), "demo");

    let skills = scan::scan(&home.places).skills;

    assert_eq!(skills.len(), 1);
    let skill = &skills[0];
    assert_eq!(skill.dir, home.path(".agents/skills/demo"));
    assert!(skill.conflicts.is_empty());
    assert_eq!(skill.bridges, vec!["Claude Code"]);
    assert!(skill.missing_bridges.is_empty());
    assert!(skill.coverage.contains(&Coverage {
        agent: "Claude Code",
        access: Access::Linked,
    }));
}

#[test]
fn migrating_with_whole_dir_link_does_not_link_into_its_own_dir() {
    let home = Home::new();
    home.link_claude_root();
    write_skill(&home.path(".smelt/skills/old"), "old");

    let outcome = ops::migrate_legacy(&home.places).unwrap();

    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.migrated, vec!["old"]);
    assert_eq!(
        fsx::kind(&home.path(".agents/skills/old")),
        Some(fsx::Kind::Dir)
    );
    let names: Vec<String> = fsx::entries(&home.path(".agents/skills"))
        .unwrap()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, vec!["old"]);
    assert!(fsx::kind(&home.path(".smelt/skills")).is_none());
}

#[test]
fn trashing_a_referenced_skill_only_removes_the_link() {
    let home = Home::new();
    home.link_claude_root();
    write_skill(&home.path("elsewhere/ext"), "ext");
    symlink(home.path("elsewhere/ext"), home.path(".agents/skills/ext")).unwrap();

    let skill = scan::scan(&home.places).skills.remove(0);
    assert_eq!(skill.dir, home.path(".agents/skills/ext"));
    ops::trash_skill(&home.places, &skill).unwrap();

    assert!(fsx::kind(&home.path(".agents/skills/ext")).is_none());
    assert!(home.path("elsewhere/ext/SKILL.md").is_file());
}

#[test]
fn missing_bridge_is_reported_and_repaired() {
    let home = Home::new();
    write_skill(&home.path(".agents/skills/demo"), "demo");

    let skill = scan::scan(&home.places).skills.remove(0);
    assert_eq!(skill.missing_bridges, vec!["Claude Code"]);
    assert!(skill.coverage.contains(&Coverage {
        agent: "Claude Code",
        access: Access::Missing,
    }));

    ops::repair_bridges(&home.places, &skill).unwrap();

    let link = home.path(".claude/skills/demo");
    assert!(is_link(&link));
    let skill = scan::scan(&home.places).skills.remove(0);
    assert!(skill.missing_bridges.is_empty());
    assert!(skill.conflicts.is_empty());
}

#[test]
fn scope_change_round_trip_moves_the_body_and_its_links() {
    let home = Home::new();
    let dir = home.path(".agents/skills/demo");
    write_skill(&dir, "demo");
    let skill = scan::scan(&home.places).skills.remove(0);
    ops::repair_bridges(&home.places, &skill).unwrap();

    let plan = ops::plan_scope(&home.places, &dir, &only("Claude Code", false)).unwrap();
    assert_eq!(
        plan,
        vec![
            Action::Unlink {
                at: home.path(".claude/skills/demo"),
            },
            Action::Move {
                from: dir.clone(),
                to: home.path(".claude/skills/demo"),
            },
        ]
    );
    let moved = ops::apply_scope(&home.places, &dir, &only("Claude Code", false)).unwrap();
    assert_eq!(moved, home.path(".claude/skills/demo"));
    assert_eq!(fsx::kind(&moved), Some(fsx::Kind::Dir));
    assert!(fsx::kind(&dir).is_none());
    let skill = scan::scan(&home.places).skills.remove(0);
    assert_eq!(skill.placement, Placement::Agent("Claude Code"));

    assert!(
        ops::plan_scope(&home.places, &moved, &only("Claude Code", false))
            .unwrap()
            .is_empty()
    );

    let back = ops::apply_scope(&home.places, &moved, &universal(false)).unwrap();
    assert_eq!(back, dir);
    assert_eq!(fsx::kind(&dir), Some(fsx::Kind::Dir));
    assert!(is_link(&home.path(".claude/skills/demo")));
    let skill = scan::scan(&home.places).skills.remove(0);
    assert!(skill.missing_bridges.is_empty());
}

#[test]
fn user_skill_moves_into_the_project() {
    let home = Home::new().with_project();
    let dir = home.path(".agents/skills/demo");
    write_skill(&dir, "demo");

    let moved = ops::apply_scope(&home.places, &dir, &universal(true)).unwrap();

    assert_eq!(moved, home.path("work/app/.agents/skills/demo"));
    let skills = scan::scan(&home.places).skills;
    assert_eq!(skills.len(), 1);
    assert!(skills[0].project_scope);
    assert!(is_link(&home.path("work/app/.claude/skills/demo")));
}

#[test]
fn project_level_needs_an_open_project() {
    let home = Home::new();
    let dir = home.path(".agents/skills/demo");
    write_skill(&dir, "demo");

    assert!(ops::plan_scope(&home.places, &dir, &universal(true)).is_err());
}

#[test]
fn import_copies_links_and_replaces_into_trash() {
    let home = Home::new();
    let source = home.path("Downloads/demo");
    write_skill(&source, "demo");
    fs::write(source.join("notes.md"), "v2").unwrap();
    let existing = home.path(".agents/skills/demo");
    write_skill(&existing, "demo");
    fs::write(existing.join("notes.md"), "v1").unwrap();

    let plan = ops::plan_import(&home.places, &universal(false), &source).unwrap();
    assert!(plan.replaces);

    let installed = ops::import(&home.places, &universal(false), &source).unwrap();

    assert_eq!(installed, existing);
    assert_eq!(fs::read_to_string(existing.join("notes.md")).unwrap(), "v2");
    assert!(source.join("SKILL.md").is_file());
    assert!(is_link(&home.path(".claude/skills/demo")));
    assert!(fsx::kind(&home.path(".agents/skills/.staging")).is_none());
    let trashed: Vec<PathBuf> = fs::read_dir(&home.places.trash)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("demo/notes.md"))
        .collect();
    assert_eq!(trashed.len(), 1);
    assert_eq!(fs::read_to_string(&trashed[0]).unwrap(), "v1");
}

#[test]
fn import_rejects_bad_names_and_flags_user_level_shadowing() {
    let home = Home::new().with_project();
    let bad = home.path("Downloads/bad");
    fs::create_dir_all(&bad).unwrap();
    fs::write(bad.join("SKILL.md"), "---\nname: -bad\n---\n").unwrap();
    assert!(ops::inspect_import(&home.places, &bad).is_err());

    let source = home.path("Downloads/demo");
    write_skill(&source, "demo");
    assert!(
        !ops::inspect_import(&home.places, &source)
            .unwrap()
            .has_global
    );
    write_skill(&home.path(".agents/skills/demo"), "demo");
    assert!(
        ops::inspect_import(&home.places, &source)
            .unwrap()
            .has_global
    );
    assert!(
        !ops::inspect_import(&home.places, &home.path(".agents/skills/demo"))
            .unwrap()
            .has_global
    );
}

#[test]
fn conflict_keeps_the_chosen_copy() {
    let home = Home::new();
    let universal_copy = home.path(".agents/skills/demo");
    let claude_copy = home.path(".claude/skills/demo");
    write_skill(&universal_copy, "demo");
    write_skill(&claude_copy, "demo");

    let skill = scan::scan(&home.places).skills.remove(0);
    assert_eq!(skill.dir, universal_copy);
    assert_eq!(skill.conflicts.len(), 1);
    assert!(ops::plan_scope(&home.places, &skill.dir, &universal(false)).is_err());

    ops::resolve_conflict(&home.places, &skill.dir, &claude_copy).unwrap();

    assert!(fsx::kind(&universal_copy).is_none());
    let skill = scan::scan(&home.places).skills.remove(0);
    assert_eq!(skill.dir, claude_copy);
    assert!(skill.conflicts.is_empty());
}

#[test]
fn broken_links_are_listed_and_pruned_without_touching_dirs() {
    let home = Home::new();
    fs::create_dir_all(home.path(".claude/skills")).unwrap();
    symlink(home.path("gone"), home.path(".claude/skills/gone")).unwrap();
    write_skill(&home.path(".agents/skills/demo"), "demo");

    let found = scan::scan(&home.places);
    assert_eq!(found.broken.len(), 1);
    assert_eq!(found.skills.len(), 1);

    ops::prune_links(&found.broken).unwrap();

    assert!(fsx::kind(&home.path(".claude/skills/gone")).is_none());
    assert!(home.path(".agents/skills/demo/SKILL.md").is_file());
}

#[test]
fn frontmatter_reads_plain_quoted_and_block_scalars() {
    let text = "---\nname: \"demo\"\ndescription: |\n  line one\n  line two\nother: x\n---\nbody";
    assert_eq!(
        Frontmatter::parse(text),
        Frontmatter {
            name: Some("demo".into()),
            description: Some("line one\nline two".into()),
        }
    );
    let folded = "---\ndescription: >\n  folded\n  text\nname: x\n---\n";
    assert_eq!(
        Frontmatter::parse(folded).description.as_deref(),
        Some("folded text")
    );
    assert_eq!(Frontmatter::parse("no frontmatter"), Frontmatter::default());
}

#[test]
fn skill_names_are_validated() {
    assert_eq!(
        ops::validate_skill_name(" ok_name-1 ").unwrap(),
        "ok_name-1"
    );
    assert!(ops::validate_skill_name("").is_err());
    assert!(ops::validate_skill_name("_x").is_err());
    assert!(ops::validate_skill_name("a b").is_err());
    assert!(ops::validate_skill_name(&"a".repeat(65)).is_err());
}
