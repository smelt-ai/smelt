use super::*;

#[test]
fn renamed_staging_executable_falls_back_to_stable_smeltd_path() {
    let root = std::env::temp_dir().join(format!(
        "smeltd-renamed-executable-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let staged = root.join("smeltd.next");
    let stable = root.join("smeltd");
    std::fs::write(&stable, b"current daemon image").unwrap();

    assert_eq!(
        daemon_executable_from_current(staged).unwrap(),
        stable,
        "macOS rename 后 current_exe 仍可能指向已不存在的 smeltd.next"
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn existing_staging_executable_wins_until_the_rename_finishes() {
    let root = std::env::temp_dir().join(format!(
        "smeltd-staged-executable-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let staged = root.join("smeltd.install.42.next");
    let stable = root.join("smeltd");
    std::fs::write(&staged, b"new daemon image").unwrap();
    std::fs::write(&stable, b"old daemon image").unwrap();

    assert_eq!(
        daemon_executable_from_current(staged.clone()).unwrap(),
        staged,
        "rename 前必须继续派生暂存的新版本，不能退回旧的稳定文件"
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn handoff_staging_executable_is_promoted_before_daemon_startup() {
    let root = std::env::temp_dir().join(format!(
        "smeltd-promote-staging-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let staged = root.join("smeltd.next");
    let stable = root.join("smeltd");
    std::fs::write(&staged, b"new daemon image").unwrap();
    std::fs::write(&stable, b"old daemon image").unwrap();

    assert_eq!(
        promote_staged_handoff_executable(&staged, true).unwrap(),
        Some(stable.clone()),
        "handoff 启动必须在初始化 daemon 前把暂存映像提升到正式路径"
    );
    assert!(!staged.exists(), "提升后不能继续遗留 smeltd.next");
    assert_eq!(std::fs::read(&stable).unwrap(), b"new daemon image");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ordinary_staging_launch_does_not_install_itself() {
    let root = std::env::temp_dir().join(format!(
        "smeltd-no-promote-without-handoff-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let staged = root.join("smeltd.install.42.next");
    let stable = root.join("smeltd");
    std::fs::write(&staged, b"candidate daemon image").unwrap();
    std::fs::write(&stable, b"stable daemon image").unwrap();

    assert_eq!(
        promote_staged_handoff_executable(&staged, false).unwrap(),
        None,
        "没有 SMELTD_HANDOFF 时不能把手工运行的暂存文件安装成正式 daemon"
    );
    assert!(staged.is_file());
    assert_eq!(std::fs::read(&stable).unwrap(), b"stable daemon image");

    std::fs::remove_dir_all(root).unwrap();
}
