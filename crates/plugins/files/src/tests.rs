use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::client::protocol::git::GitStatusData;
use smelt_plugin_api::model::runtime_path::PathStyle;

use crate::ProjectRef;
use crate::model::{
    BreadcrumbSeg, DirCache, DirListing, FileTreeRow, SearchHit, TreeRoots, breadcrumb_segments,
    build_search_tree, collect_file_tree_rows, file_tree_row_id,
};
use crate::store::{FilesStore, SavedProject, Settings};

fn seg(label: &str, path: &str) -> BreadcrumbSeg {
    BreadcrumbSeg {
        label: label.into(),
        path: path.into(),
    }
}

#[test]
fn breadcrumb_segments_split_path_under_workspace_root() {
    let roots = vec!["/workspace/project".into()];
    assert_eq!(
        breadcrumb_segments(
            PathStyle::Posix,
            "/workspace/project/crates/smelt-core/tests/fake_llm.py",
            &roots
        ),
        vec![
            seg("project", "/workspace/project"),
            seg("crates", "/workspace/project/crates"),
            seg("smelt-core", "/workspace/project/crates/smelt-core"),
            seg("tests", "/workspace/project/crates/smelt-core/tests"),
            seg(
                "fake_llm.py",
                "/workspace/project/crates/smelt-core/tests/fake_llm.py"
            ),
        ]
    );
}

#[test]
fn breadcrumb_segments_prefers_longest_matching_root() {
    let roots = vec!["/repo".into(), "/repo/crates/smelt-core".into()];
    assert_eq!(
        breadcrumb_segments(
            PathStyle::Posix,
            "/repo/crates/smelt-core/src/lib.rs",
            &roots
        ),
        vec![
            seg("smelt-core", "/repo/crates/smelt-core"),
            seg("src", "/repo/crates/smelt-core/src"),
            seg("lib.rs", "/repo/crates/smelt-core/src/lib.rs"),
        ]
    );
}

#[test]
fn breadcrumb_segments_falls_back_to_filename_outside_roots() {
    let roots = vec!["/repo".into()];
    assert_eq!(
        breadcrumb_segments(PathStyle::Posix, "/tmp/scratch.rs", &roots),
        vec![seg("scratch.rs", "/tmp/scratch.rs")]
    );
}

#[test]
fn breadcrumb_segments_split_windows_paths_on_both_separators() {
    let roots = vec![r"C:\repo".into()];
    let segs = breadcrumb_segments(PathStyle::Windows, r"C:\repo\src\lib.rs", &roots);
    let labels: Vec<_> = segs.iter().map(|seg| seg.label.as_str()).collect();
    assert_eq!(labels, ["repo", "src", "lib.rs"]);
}

fn cached(entries: Vec<(&str, bool)>) -> DirListing {
    DirListing {
        entries: Rc::new(
            entries
                .into_iter()
                .map(|(name, is_dir)| (name.to_string(), is_dir))
                .collect(),
        ),
        stale: false,
    }
}

fn no_git(_root: &str) -> Option<&'static GitStatusData> {
    None
}

fn rows(
    roots: &[String],
    pinned: &[String],
    expanded: &HashSet<String>,
    collapsed: &HashSet<String>,
    cache: &DirCache,
) -> Vec<FileTreeRow> {
    collect_file_tree_rows(
        PathStyle::Posix,
        &TreeRoots {
            roots,
            pinned,
            collapsed,
        },
        expanded,
        cache,
        &no_git,
    )
}

#[test]
fn file_tree_rows_single_root_keeps_walk_order_without_header() {
    let mut cache = DirCache::new();
    cache.insert(
        "/repo".into(),
        cached(vec![("a.txt", false), ("src", true)]),
    );
    cache.insert("/repo/src".into(), cached(vec![("lib.rs", false)]));
    let expanded = HashSet::from(["/repo/src".into()]);
    let status = GitStatusData {
        files: vec![
            (" M".into(), "src/lib.rs".into()),
            ("??".into(), "a.txt".into()),
        ],
        ..GitStatusData::default()
    };
    let git: HashMap<String, GitStatusData> = HashMap::from([("/repo".into(), status)]);

    assert_eq!(
        collect_file_tree_rows(
            PathStyle::Posix,
            &TreeRoots {
                roots: &["/repo".into()],
                pinned: &[],
                collapsed: &HashSet::new(),
            },
            &expanded,
            &cache,
            &|root: &str| git.get(root),
        ),
        vec![
            FileTreeRow::Entry {
                depth: 0,
                name: "a.txt".into(),
                is_dir: false,
                path: "/repo/a.txt".into(),
                expanded: false,
                git_mark: Some('U'),
            },
            FileTreeRow::Entry {
                depth: 0,
                name: "src".into(),
                is_dir: true,
                path: "/repo/src".into(),
                expanded: true,
                git_mark: None,
            },
            FileTreeRow::Entry {
                depth: 1,
                name: "lib.rs".into(),
                is_dir: false,
                path: "/repo/src/lib.rs".into(),
                expanded: false,
                git_mark: Some('M'),
            },
        ]
    );
}

#[test]
fn file_tree_row_identity_follows_path_when_a_directory_expands() {
    let mut cache = DirCache::new();
    cache.insert(
        "/repo".into(),
        cached(vec![("src", true), ("b.txt", false)]),
    );
    cache.insert("/repo/src".into(), cached(vec![("a.rs", false)]));
    let roots = ["/repo".to_string()];
    let collapsed = rows(&roots, &[], &HashSet::new(), &HashSet::new(), &cache);
    let expanded_dirs = HashSet::from(["/repo/src".into()]);
    let expanded = rows(&roots, &[], &expanded_dirs, &HashSet::new(), &cache);

    fn id_of(rows: &[FileTreeRow], path: &str) -> Option<(&'static str, String)> {
        rows.iter()
            .find(|row| row.path() == Some(path))
            .map(file_tree_row_id)
            .map(|(kind, key)| (kind, key.to_string()))
    }
    assert_eq!(
        id_of(&collapsed, "/repo/b.txt"),
        Some(("file", "/repo/b.txt".to_string()))
    );
    assert_eq!(
        id_of(&expanded, "/repo/b.txt"),
        Some(("file", "/repo/b.txt".to_string()))
    );
    assert_ne!(
        collapsed
            .iter()
            .position(|row| row.path() == Some("/repo/b.txt")),
        expanded
            .iter()
            .position(|row| row.path() == Some("/repo/b.txt")),
    );
}

#[test]
fn file_tree_rows_multi_root_header_collapses_children_and_marks_pinned() {
    let mut cache = DirCache::new();
    cache.insert("/a".into(), cached(vec![("a.txt", false)]));
    cache.insert("/b".into(), cached(vec![("b.txt", false)]));
    let collapsed = HashSet::from(["/b".into()]);

    assert_eq!(
        rows(
            &["/a".into(), "/b".into()],
            &["/b".into()],
            &HashSet::new(),
            &collapsed,
            &cache,
        ),
        vec![
            FileTreeRow::RootHeader {
                path: "/a".into(),
                open: true,
                pinned: false,
            },
            FileTreeRow::Entry {
                depth: 1,
                name: "a.txt".into(),
                is_dir: false,
                path: "/a/a.txt".into(),
                expanded: false,
                git_mark: None,
            },
            FileTreeRow::RootHeader {
                path: "/b".into(),
                open: false,
                pinned: true,
            },
        ]
    );
}

#[test]
fn file_tree_rows_show_loading_for_uncached_root() {
    let cache = DirCache::new();
    assert_eq!(
        rows(
            &["/a".into(), "/b".into()],
            &[],
            &HashSet::new(),
            &HashSet::new(),
            &cache,
        ),
        vec![
            FileTreeRow::RootHeader {
                path: "/a".into(),
                open: true,
                pinned: false,
            },
            FileTreeRow::Loading {
                depth: 1,
                root: "/a".into(),
            },
            FileTreeRow::RootHeader {
                path: "/b".into(),
                open: true,
                pinned: false,
            },
            FileTreeRow::Loading {
                depth: 1,
                root: "/b".into(),
            },
        ]
    );
}

#[test]
fn search_tree_keeps_matching_files_under_their_ancestor_directories() {
    let hits = vec![
        SearchHit {
            path: "/repo/crates/smelt-core/src/lib.rs".into(),
            rel: "crates/smelt-core/src/lib.rs".into(),
            line: None,
        },
        SearchHit {
            path: "/repo/mobile/lib/pages/qr_scan.dart".into(),
            rel: "mobile/lib/pages/qr_scan.dart".into(),
            line: Some((8, "library".into())),
        },
    ];

    let tree = build_search_tree(&hits);
    let crates = tree.dirs.get("crates").expect("crates ancestor exists");
    let core = crates
        .dirs
        .get("smelt-core")
        .expect("crate ancestor exists");
    assert_eq!(core.dirs["src"].files, vec![0]);
    assert_eq!(tree.dirs["mobile"].dirs["lib"].dirs["pages"].files, vec![1]);
}

// ===================== 插件自己的库 =====================

fn temp_store() -> (tempfile::TempDir, FilesStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = FilesStore::open_at(&dir.path().join("files.sqlite3"));
    (dir, store)
}

#[test]
fn store_defaults_when_nothing_saved() {
    let (_dir, store) = temp_store();
    assert_eq!(store.settings(), Settings::default());
    let project = ProjectRef {
        runtime: RuntimeKey::Local,
        root: "/repo".into(),
    };
    assert_eq!(store.project(&project), SavedProject::default());
}

#[test]
fn store_round_trips_layout_and_pinned_roots_in_order() {
    let (_dir, store) = temp_store();
    store.save_tree_layout(250.0, false);
    store.save_pinned(&["/z".into(), "/a".into()]);
    assert_eq!(
        store.settings(),
        Settings {
            tree_width: 250.0,
            tree_open: false,
            pinned: vec!["/z".into(), "/a".into()],
        }
    );
    // 再存一次是整份替换，去掉的不留下来。
    store.save_pinned(&["/a".into()]);
    assert_eq!(store.settings().pinned, vec!["/a".to_string()]);
}

#[test]
fn store_keeps_projects_apart_by_machine_and_root() {
    let (_dir, store) = temp_store();
    let local = ProjectRef {
        runtime: RuntimeKey::Local,
        root: "/repo".into(),
    };
    let remote = ProjectRef {
        runtime: RuntimeKey::Remote("box".into()),
        root: "/repo".into(),
    };
    let saved = SavedProject {
        selected: Some("/repo/src".into()),
        open_file: Some("/repo/src/lib.rs".into()),
        expanded: HashSet::from(["/repo/src".into()]),
        collapsed_roots: HashSet::from(["/pinned".into()]),
    };
    store.save_project(&local, &saved);
    assert_eq!(store.project(&local), saved);
    assert_eq!(store.project(&remote), SavedProject::default());

    // 整份替换：收起的目录少了就真的少了。
    let fewer = SavedProject {
        expanded: HashSet::new(),
        ..saved
    };
    store.save_project(&local, &fewer);
    assert_eq!(store.project(&local), fewer);
}
