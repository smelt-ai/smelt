use super::view::{BreadcrumbSeg, breadcrumb_segments};
use super::{SearchHit, build_search_tree, search_project};

#[test]
fn breadcrumb_segments_split_path_under_workspace_root() {
    let roots = vec!["/Users/c.chen/nio/smelt".into()];
    assert_eq!(
        breadcrumb_segments(
            "/Users/c.chen/nio/smelt/crates/smelt-core/tests/fake_llm.py",
            &roots
        ),
        vec![
            BreadcrumbSeg {
                label: "smelt".into(),
                path: "/Users/c.chen/nio/smelt".into(),
            },
            BreadcrumbSeg {
                label: "crates".into(),
                path: "/Users/c.chen/nio/smelt/crates".into(),
            },
            BreadcrumbSeg {
                label: "smelt-core".into(),
                path: "/Users/c.chen/nio/smelt/crates/smelt-core".into(),
            },
            BreadcrumbSeg {
                label: "tests".into(),
                path: "/Users/c.chen/nio/smelt/crates/smelt-core/tests".into(),
            },
            BreadcrumbSeg {
                label: "fake_llm.py".into(),
                path: "/Users/c.chen/nio/smelt/crates/smelt-core/tests/fake_llm.py".into(),
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

#[test]
fn search_project_respects_gitignore_and_matches_content() {
    let temp = std::env::temp_dir().join(format!("smelt-search-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();
    let src_dir = temp.join("src");
    let target_dir = temp.join("target");
    let venv_dir = temp.join(".venv");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&target_dir).unwrap();
    std::fs::create_dir_all(&venv_dir).unwrap();

    std::fs::write(temp.join(".gitignore"), "target/\n.venv/\nignored.txt\n").unwrap();
    std::fs::write(
        src_dir.join("main.rs"),
        "fn main() {\n    println!(\"hello smelt_search\");\n}\n",
    )
    .unwrap();
    std::fs::write(
        temp.join("README.md"),
        "# Welcome to smelt_search project\n",
    )
    .unwrap();
    std::fs::write(target_dir.join("build.rs"), "smelt_search target file\n").unwrap();
    std::fs::write(venv_dir.join("lib.py"), "smelt_search venv file\n").unwrap();
    std::fs::write(temp.join("ignored.txt"), "smelt_search ignored text\n").unwrap();

    let (hits, truncated) = search_project(temp.to_str().unwrap(), "smelt_search");
    assert!(!truncated);

    let hit_paths: Vec<_> = hits.iter().map(|h| h.rel.as_str()).collect();
    assert!(
        hit_paths.contains(&"README.md"),
        "README.md should be matched"
    );
    assert!(
        hit_paths.contains(&"src/main.rs"),
        "src/main.rs should be matched"
    );
    assert!(
        !hit_paths.iter().any(|p| p.starts_with("target/")),
        "target should be ignored"
    );
    assert!(
        !hit_paths.iter().any(|p| p.starts_with(".venv/")),
        ".venv should be ignored"
    );
    assert!(
        !hit_paths.contains(&"ignored.txt"),
        "ignored.txt should be ignored"
    );

    let _ = std::fs::remove_dir_all(&temp);
}
