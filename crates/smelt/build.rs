//! Windows 目标把应用图标嵌进 `smelt.exe`（设计文档 D5）。
//!
//! 清单仍由 `gpui-pre` 的 `windows-manifest` 嵌（`RT_MANIFEST`）。这里只加
//! `ICON`，两种资源类型不同，不会互相覆盖。非 Windows 目标什么都不做。

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("../../assets/smelt.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let icon = icon.canonicalize().unwrap_or(icon);
    let icon_rc = icon.to_string_lossy().replace('\\', "/");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc_path = out_dir.join("smelt-icon.rc");
    std::fs::write(&rc_path, format!("1 ICON \"{icon_rc}\"\n")).expect("write icon resource script");

    // 缺了资源编译器就失败，不要悄悄打出没有图标的 exe。
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .expect("embed Windows application icon");
}
