//! 把 `pilot/` 照 `nsh build` 打包、编成字节码，放到 `$OUT_DIR/pilot/bundle.nshc`，
//! 运行时 `Bundle::read_trusted` 读。打包按 NSH 自带的 `nsh/base`；宿主给的模块和组件
//! 运行时按名字接（`nativeModule`、`nativeComponent`），打包不用知道。

use std::{fs, path::PathBuf};

use nsh_bundler::BundleOptions;

fn main() {
    let dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("pilot");
    println!("cargo:rerun-if-changed={}", dir.display());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("pilot");
    let release = std::env::var("PROFILE").is_ok_and(|p| p == "release");
    if let Err(message) = build(&dir, &out, release) {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn build(dir: &std::path::Path, out: &std::path::Path, release: bool) -> Result<(), String> {
    let ui_sets = vec![nsh_core::element::description::SetDescription::from(
        nsh_base::registry(),
    )];
    let bundle = nsh_bundler::bundle(&BundleOptions {
        entry: dir.join("App.tsx"),
        minify: release,
        split: false,
        ui_sets: ui_sets.clone(),
        development: !release,
        ..Default::default()
    })
    .map_err(|e| e.plain)?;
    let bytecode =
        nsh_js::compile_bytecode_with(&bundle.code, release, &ui_sets).map_err(|e| e.message)?;
    let _ = fs::remove_dir_all(out);
    fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let write = |name: &str, bytes: &[u8]| {
        let path = out.join(name);
        fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
    };
    write("bundle.nshc", &bytecode)?;
    write(
        "bundle.js.map",
        bundle.source_map.as_deref().unwrap_or_default().as_bytes(),
    )?;
    // 应用 id 定它的数据放哪（`Bundle::read`）。
    write("app.json", r#"{"id":"ai.smelt.nsh-pilot"}"#.as_bytes())?;
    Ok(())
}
