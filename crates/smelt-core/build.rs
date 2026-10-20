//! Pi 运行时的版本号跟 `packages/pi-agent/package.json` 走。
//!
//! 源码、`bun.lock` 不再嵌进 Rust 二进制。用户机器上跑的是编译好的 `smelt-pi`。

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let package_json = manifest.join("../../packages/pi-agent/package.json");
    println!("cargo:rerun-if-changed={}", package_json.display());

    let manifest_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&package_json).expect("读取 pi-agent package.json"))
            .expect("解析 pi-agent package.json");
    let version = manifest_json["version"]
        .as_str()
        .expect("pi-agent package.json 缺少 version");

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("pi_agent_version.rs");
    fs::write(
        out,
        format!("const PI_AGENT_RUNTIME_VERSION: &str = {version:?};\n"),
    )
    .expect("写入 Pi 运行时版本");
}
