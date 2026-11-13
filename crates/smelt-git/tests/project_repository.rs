use std::{fs, path::Path, process::Command};

use smelt_git::project_repository::{
    bind, clone_repository, remove_link, repository_root, validate_name,
};

#[test]
fn repository_names_cannot_escape_the_project_catalog() {
    for name in [
        "",
        ".",
        "..",
        "../api",
        "/api",
        "api\\child",
        " api",
        ".api",
        "a\nb",
    ] {
        assert!(validate_name(name).is_err(), "{name:?}");
    }
}

fn init(path: &Path) {
    fs::create_dir_all(path).unwrap();
    assert!(
        Command::new("git")
            .arg("init")
            .arg(path)
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn internal_repository_needs_no_link_and_subdirectories_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("repos/api");
    init(&source);
    let bound = bind(temp.path(), &source, "api").unwrap();
    assert!(bound.managed_link.is_none());
    fs::create_dir(source.join("sub")).unwrap();
    assert!(repository_root(&source.join("sub")).is_err());
}

#[test]
fn project_root_can_be_bound_without_a_link() {
    let project = tempfile::tempdir().unwrap();
    init(project.path());
    let bound = bind(project.path(), project.path(), "project").unwrap();
    assert!(bound.managed_link.is_none());
    assert_eq!(bound.root, fs::canonicalize(project.path()).unwrap());
}

#[test]
fn clone_publishes_an_existing_local_git_repository() {
    let project = tempfile::Builder::new()
        .prefix("smelt project ")
        .tempdir()
        .unwrap();
    let source = tempfile::Builder::new()
        .prefix("smelt source ")
        .tempdir()
        .unwrap();
    init(source.path());
    let project_root = fs::canonicalize(project.path()).unwrap();
    let bound = clone_repository(&project_root, source.path().to_str().unwrap(), "api").unwrap();
    assert!(bound.root.join(".git").exists());
    assert!(bound.managed_link.is_none());
    assert!(source.path().join(".git").exists());
}

#[cfg(unix)]
#[test]
fn removing_owned_link_preserves_external_repository() {
    let project = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    init(source.path());
    let bound = bind(project.path(), source.path(), "api").unwrap();
    let link = bound.managed_link.as_ref().unwrap();
    assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    remove_link(project.path(), link, &bound.root).unwrap();
    assert!(source.path().join(".git").exists());
    assert!(!link.exists());
}

#[test]
fn binding_and_cloning_never_overwrite_an_existing_destination() {
    let project = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    init(source.path());
    fs::create_dir_all(project.path().join("repos/api")).unwrap();
    fs::write(project.path().join("repos/api/keep"), "keep").unwrap();
    assert!(bind(project.path(), source.path(), "api").is_err());
    assert!(clone_repository(project.path(), source.path().to_str().unwrap(), "api").is_err());
    assert!(project.path().join("repos/api/keep").exists());
}

#[cfg(unix)]
#[test]
fn a_replaced_link_is_not_removed() {
    let project = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let replacement = tempfile::tempdir().unwrap();
    init(source.path());
    let bound = bind(project.path(), source.path(), "api").unwrap();
    let link = bound.managed_link.as_ref().unwrap();
    fs::remove_file(link).unwrap();
    std::os::unix::fs::symlink(replacement.path(), link).unwrap();
    assert!(remove_link(project.path(), link, &bound.root).is_err());
    assert!(link.exists());
}

#[test]
fn invalid_clone_keeps_existing_files_and_does_not_publish_a_repository() {
    let project = tempfile::tempdir().unwrap();
    assert!(clone_repository(project.path(), "/nonexistent/smelt-repository", "api").is_err());
    assert!(!project.path().join("repos/api").exists());
}

#[cfg(unix)]
#[test]
fn removing_an_already_missing_owned_link_is_safe() {
    let project = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    init(source.path());
    let bound = bind(project.path(), source.path(), "api").unwrap();
    let link = bound.managed_link.as_ref().unwrap();
    fs::remove_file(link).unwrap();
    fs::remove_dir(project.path().join("repos")).unwrap();
    remove_link(project.path(), link, &bound.root).unwrap();
    assert!(source.path().join(".git").exists());
}
