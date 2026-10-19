//! release 把 `make pi-runtime-pack` 写下的归档 SHA-256 编进二进制。
//!
//! 归档本身随应用分发，不 `include_bytes!`。debug 不读这份校验值，直接用仓库里的
//! `pi-runtime/`。

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let sha_path = manifest.join("../../pi-runtime/dist/archive.sha256");
    println!("cargo:rerun-if-changed={}", sha_path.display());

    let profile = env::var("PROFILE").unwrap_or_default();
    let sha = if profile == "release" {
        let text = fs::read_to_string(&sha_path).unwrap_or_else(|_| {
            panic!(
                "release 构建需要 Pi 运行时归档校验值 {}。先运行 make pi-runtime-pack",
                sha_path.display()
            )
        });
        let sha = text.trim();
        if sha.len() != 64
            || !sha
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            panic!("pi-runtime/dist/archive.sha256 必须是 64 位小写十六进制");
        }
        sha.to_string()
    } else {
        String::new()
    };

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("pi_runtime_archive.rs");
    fs::write(
        out,
        format!("const PI_RUNTIME_ARCHIVE_SHA256: &str = {sha:?};\n"),
    )
    .expect("写入 Pi 运行时归档校验值");
}
