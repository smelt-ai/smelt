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
    let leftover_pin = root.join(".smeltd.image.11309");
    assert_eq!(
        daemon_executable_from_current(leftover_pin).unwrap(),
        stable,
        "历史硬链被删后必须回退到正式 smeltd，不能 ENOENT 把宿主 spawn 打挂"
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

#[test]
fn replaced_directory_entry_is_not_the_session_host_image() {
    let root = std::env::temp_dir().join(format!(
        "smeltd-pinned-image-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("smeltd");
    std::fs::write(&path, b"running-image").unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    use std::os::unix::fs::MetadataExt;
    let dev = meta.dev();
    let ino = meta.ino();

    // 必须 rename 覆盖。直接 write 会截断同一个 inode，测不到「路径还在、映像已经换了」。
    let replacement = root.join("smeltd.replaced");
    std::fs::write(&replacement, b"replaced-without-session-host").unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert!(
        !path_has_inode(&path, dev, ino),
        "覆盖路径之后不能再把目录项当成正在跑的映像"
    );

    let error = session_host_executable_from(path, Some((dev, ino)))
        .expect_err("路径已换 inode 时必须拒绝启动，不能去跑那份新文件");
    assert!(
        error.to_string().contains("另一份映像"),
        "实际错误：{error}"
    );
    assert!(
        !root.join(format!("smeltd.image-{ino}")).exists(),
        "不能靠拷贝一份映像来绕过"
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn matching_inode_keeps_the_current_exe_path() {
    let root = std::env::temp_dir().join(format!(
        "smeltd-same-inode-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("smeltd");
    std::fs::write(&path, b"running-image").unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    use std::os::unix::fs::MetadataExt;
    let chosen =
        session_host_executable_from(path.clone(), Some((meta.dev(), meta.ino()))).unwrap();
    assert_eq!(chosen, path);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn staged_successor_is_the_sibling_next_file() {
    let root = std::env::temp_dir().join(format!(
        "smeltd-staged-successor-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let live = root.join("smeltd");
    let next = root.join("smeltd.next");
    std::fs::write(&live, b"running").unwrap();
    assert_eq!(staged_successor_executable(&live), None);

    std::fs::write(&next, b"pending").unwrap();
    assert_eq!(
        staged_successor_executable(&live).as_deref(),
        Some(next.as_path())
    );
    assert_eq!(
        staged_successor_executable(&next),
        None,
        "已经在跑 .next 时不能把自身再当成待升级候选"
    );

    std::fs::remove_dir_all(root).unwrap();
}
