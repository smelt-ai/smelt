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
