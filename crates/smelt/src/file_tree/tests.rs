use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use super::view::{
    BreadcrumbSeg, FileTreeRow, breadcrumb_segments, collect_file_tree_rows, file_tree_row_id,
};
use super::{DirCache, SearchHit, build_search_tree};
use crate::git_panel::GitStatusData;
use smelt_model::runtime_path::PathStyle;

#[test]
fn breadcrumb_segments_split_path_under_workspace_root() {
    let roots = vec!["/workspace/project".into()];
    assert_eq!(
        breadcrumb_segments(
            "/workspace/project/crates/smelt-core/tests/fake_llm.py",
            &roots
        ),
        vec![
            BreadcrumbSeg {
                label: "project".into(),
                path: "/workspace/project".into(),
            },
            BreadcrumbSeg {
                label: "crates".into(),
                path: "/workspace/project/crates".into(),
            },
            BreadcrumbSeg {
                label: "smelt-core".into(),
                path: "/workspace/project/crates/smelt-core".into(),
            },
            BreadcrumbSeg {
                label: "tests".into(),
                path: "/workspace/project/crates/smelt-core/tests".into(),
            },
            BreadcrumbSeg {
                label: "fake_llm.py".into(),
                path: "/workspace/project/crates/smelt-core/tests/fake_llm.py".into(),
            },
        ]
    );
}

#[test]
fn breadcrumb_segments_prefers_longest_matching_root() {
    let roots = vec!["/repo".into(), "/repo/crates/smelt-core".into()];
    assert_eq!(
        breadcrumb_segments("/repo/crates/smelt-core/src/lib.rs", &roots),
        vec![
            BreadcrumbSeg {
                label: "smelt-core".into(),
                path: "/repo/crates/smelt-core".into(),
            },
            BreadcrumbSeg {
                label: "src".into(),
                path: "/repo/crates/smelt-core/src".into(),
            },
            BreadcrumbSeg {
                label: "lib.rs".into(),
                path: "/repo/crates/smelt-core/src/lib.rs".into(),
            },
        ]
    );
}

#[test]
fn breadcrumb_segments_falls_back_to_filename_outside_roots() {
    let roots = vec!["/repo".into()];
    assert_eq!(
        breadcrumb_segments("/tmp/scratch.rs", &roots),
        vec![BreadcrumbSeg {
            label: "scratch.rs".into(),
            path: "/tmp/scratch.rs".into(),
        }]
    );
}

fn cached(entries: Vec<(&str, bool)>) -> (Instant, Rc<Vec<(String, bool)>>) {
    (
        Instant::now(),
        Rc::new(
            entries
                .into_iter()
                .map(|(name, is_dir)| (name.to_string(), is_dir))
                .collect(),
        ),
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
    let mut status = GitStatusData::default();
    status.files = vec![
        (" M".into(), "src/lib.rs".into()),
        ("??".into(), "a.txt".into()),
    ];
    let git = HashMap::from([("/repo".into(), (Instant::now(), status))]);

    assert_eq!(
        collect_file_tree_rows(
            PathStyle::Posix,
            &["/repo".into()],
            &expanded,
            &HashSet::new(),
            &cache,
            &git,
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
    let collapsed = collect_file_tree_rows(
        PathStyle::Posix,
        &["/repo".into()],
        &HashSet::new(),
        &HashSet::new(),
        &cache,
        &HashMap::new(),
    );
    let expanded_dirs = HashSet::from(["/repo/src".into()]);
    let expanded = collect_file_tree_rows(
        PathStyle::Posix,
        &["/repo".into()],
        &expanded_dirs,
        &HashSet::new(),
        &cache,
        &HashMap::new(),
    );

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
fn file_tree_rows_multi_root_header_collapses_children() {
    let mut cache = DirCache::new();
    cache.insert("/a".into(), cached(vec![("a.txt", false)]));
    cache.insert("/b".into(), cached(vec![("b.txt", false)]));
    let collapsed = HashSet::from(["/b".into()]);

    assert_eq!(
        collect_file_tree_rows(
            PathStyle::Posix,
            &["/a".into(), "/b".into()],
            &HashSet::new(),
            &collapsed,
            &cache,
            &HashMap::new(),
        ),
        vec![
            FileTreeRow::RootHeader {
                path: "/a".into(),
                open: true,
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
