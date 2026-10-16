use super::*;
use std::os::unix::fs::MetadataExt;

fn scratch(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "smeltd-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn content_fingerprint_matches_plugin_host() {
    let root = scratch("fingerprint");
    let path = root.join("image");
    std::fs::write(&path, b"same-bytes").unwrap();
    assert_eq!(
        smelt_core::daemon_image::content_fingerprint(&path).unwrap(),
        smelt_plugin_host::executable_fingerprint(&path).unwrap()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn symlink_launch_name_pins_the_image_file() {
    let root = scratch("pin-symlink");
    let src = root.join("src");
    std::fs::write(&src, b"mapped-image").unwrap();
    let image = smelt_core::daemon_image::publish_image(&root, &src).unwrap();
    smelt_core::daemon_image::retarget_launch_symlink(&root, &image).unwrap();

    let pinned = open_mapped_image(&root.join("smeltd")).expect("应从启动名钉住映像");
    let image_meta = std::fs::symlink_metadata(&image).unwrap();
    assert_eq!(pinned.path, image);
    assert_eq!(pinned.dev, image_meta.dev());
    assert_eq!(pinned.ino, image_meta.ino());
    assert_ne!(
        pinned.ino,
        std::fs::symlink_metadata(root.join("smeltd"))
            .unwrap()
            .ino(),
        "钉住的必须是映像文件，不是启动名这根链接"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn replaced_directory_entry_is_not_the_session_host_image() {
    let root = scratch("pinned-image");
    let path = root.join("smeltd-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    std::fs::write(&path, b"running-image").unwrap();
    let meta = std::fs::symlink_metadata(&path).unwrap();
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

    let error = session_host_executable_from(&path, Some((dev, ino)))
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
fn matching_inode_keeps_the_pinned_path() {
    let root = scratch("same-inode");
    let path = root.join("smeltd-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    std::fs::write(&path, b"running-image").unwrap();
    let meta = std::fs::symlink_metadata(&path).unwrap();
    let chosen = session_host_executable_from(&path, Some((meta.dev(), meta.ino()))).unwrap();
    assert_eq!(chosen, path);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retargeted_launch_symlink_does_not_change_the_session_host() {
    let root = scratch("retarget");
    let old_src = root.join("old");
    let new_src = root.join("new");
    std::fs::write(&old_src, b"running-image").unwrap();
    std::fs::write(&new_src, b"next-image").unwrap();
    let old_image = smelt_core::daemon_image::publish_image(&root, &old_src).unwrap();
    let new_image = smelt_core::daemon_image::publish_image(&root, &new_src).unwrap();
    smelt_core::daemon_image::retarget_launch_symlink(&root, &old_image).unwrap();
    let pinned = open_mapped_image(&old_image).unwrap();

    smelt_core::daemon_image::retarget_launch_symlink(&root, &new_image).unwrap();

    let chosen =
        session_host_executable_from(&pinned.path, Some((pinned.dev, pinned.ino))).unwrap();
    assert_eq!(chosen, old_image);
    assert_ne!(chosen, root.join("smeltd"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sibling_with_the_old_inode_is_not_a_fallback() {
    let root = scratch("no-fallback");
    let image =
        root.join("smeltd-cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc");
    std::fs::write(&image, b"original-running-image").unwrap();
    let stable = root.join("smeltd");
    std::fs::hard_link(&image, &stable).unwrap();
    let meta = std::fs::symlink_metadata(&image).unwrap();
    let dev = meta.dev();
    let ino = meta.ino();

    let replacement = root.join("replacement");
    std::fs::write(&replacement, b"newer-image").unwrap();
    std::fs::rename(&replacement, &image).unwrap();

    let error = session_host_executable_from(&image, Some((dev, ino)))
        .expect_err("旁边就算还留着旧 inode，也不能拿它当会话宿主");
    assert!(
        error.to_string().contains("另一份映像"),
        "实际错误：{error}"
    );
    assert!(stable.is_file());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unpinned_session_host_is_refused() {
    let root = scratch("unpinned");
    let path = root.join("smeltd");
    std::fs::write(&path, b"running-image").unwrap();
    let error = session_host_executable_from(&path, None).expect_err("没钉住就不能启动宿主");
    assert!(
        error.to_string().contains("没有钉住守护映像"),
        "实际错误：{error}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pending_symlink_is_the_successor_image() {
    let root = scratch("pending-successor");
    let live_src = root.join("live-src");
    let next_src = root.join("next-src");
    std::fs::write(&live_src, b"running").unwrap();
    std::fs::write(&next_src, b"pending").unwrap();
    let live = smelt_core::daemon_image::publish_image(&root, &live_src).unwrap();
    assert_eq!(staged_successor_executable(&live), None);

    let next = smelt_core::daemon_image::publish_image(&root, &next_src).unwrap();
    smelt_core::daemon_image::retarget_pending_symlink(&root, &next).unwrap();
    assert_eq!(
        staged_successor_executable(&live).as_deref(),
        Some(std::fs::canonicalize(&next).unwrap().as_path())
    );
    assert_eq!(
        staged_successor_executable(&next),
        None,
        "已经在跑这份映像时不能把自身再当成待升级候选"
    );

    std::fs::remove_dir_all(root).unwrap();
}
