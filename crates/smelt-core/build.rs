//! 受管 Pi 运行时的唯一真相是 `packages/pi-agent/`。
//!
//! 手写 `include_str!` 清单会让新 `.ts` 编译通过、安装成功，却在握手时才缺文件。
//! 这里按目录生成嵌入表：新增源文件自动进二进制，漏文件变成编不过。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let package = manifest
        .join("../../packages/pi-agent")
        .canonicalize()
        .expect("packages/pi-agent");
    let src = package.join("src");
    let patches = package.join("patches");
    println!("cargo:rerun-if-changed={}", package.display());
    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-changed={}", patches.display());
    println!(
        "cargo:rerun-if-changed={}",
        package.join("package.json").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        package.join("bun.lock").display()
    );

    let manifest_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(package.join("package.json")).expect("读取 pi-agent package.json"),
    )
    .expect("解析 pi-agent package.json");
    let version = manifest_json["version"]
        .as_str()
        .expect("pi-agent package.json 缺少 version");

    let mut ts_files = collect_files(&src, &is_runtime_ts);
    ts_files.sort();
    let mut patch_files = collect_files(&patches, &|path| path.is_file());
    patch_files.sort();
    for path in ts_files.iter().chain(&patch_files) {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let mut code = String::from("// 由 build.rs 生成，不要手改。\n");
    code.push_str(&format!(
        "const PI_AGENT_RUNTIME_VERSION: &str = {version:?};\n"
    ));
    code.push_str(&format!(
        "const PI_AGENT_PACKAGE_JSON: &str = include_str!(\"{}\");\n",
        rust_path(package.join("package.json"))
    ));
    code.push_str(&format!(
        "const PI_AGENT_BUN_LOCK: &str = include_str!(\"{}\");\n",
        rust_path(package.join("bun.lock"))
    ));
    code.push_str("const PI_AGENT_RUNTIME_FILES: &[(&str, &str)] = &[\n");
    code.push_str("    (\"package.json\", PI_AGENT_PACKAGE_JSON),\n");
    code.push_str("    (\"bun.lock\", PI_AGENT_BUN_LOCK),\n");
    for path in ts_files.iter().chain(&patch_files) {
        let relative = path
            .strip_prefix(&package)
            .expect("Pi runtime 文件必须位于 package 内");
        code.push_str(&format!(
            "    ({:?}, include_str!(\"{}\")),\n",
            rust_path(relative),
            rust_path(path)
        ));
    }
    code.push_str("];\n");

    let out =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("pi_agent_runtime_files.rs");
    fs::write(&out, code).expect("写入 Pi 运行时嵌入表");
}

fn collect_files(dir: &Path, include: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in
        fs::read_dir(dir).unwrap_or_else(|error| panic!("读取 {} 失败：{error}", dir.display()))
    {
        let path = entry
            .unwrap_or_else(|error| panic!("枚举 {} 失败：{error}", dir.display()))
            .path();
        if path.is_dir() {
            files.extend(collect_files(&path, include));
        } else if include(&path) {
            files.push(path);
        }
    }
    files
}

fn is_runtime_ts(path: &Path) -> bool {
    path.is_file()
        && path.extension().is_some_and(|ext| ext == "ts")
        && !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".d.ts"))
}

fn rust_path(path: impl AsRef<Path>) -> String {
    path.as_ref().display().to_string().replace('\\', "/")
}
