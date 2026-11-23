//! 把下面这几个 TSX 入口照 `nsh build` 打包、编成字节码，放到 `$OUT_DIR/<名字>/bundle.nshc`，
//! 运行时 `Bundle::read_trusted` 读，只给 `examples/bench.rs`。主壳和插件不在这里：运行时
//! 打包（`src/plugins.rs`），改插件不重编。打包按 NSH 自带的 `nsh/base`；宿主给的模块和组件
//! 运行时按名字接（`nativeModule`、`nativeComponent`），打包不用知道。

use std::{
    fs,
    path::{Path, PathBuf},
};

use nsh_bundler::BundleOptions;

/// （名字，入口，应用 id）。
const APPS: [(&str, &str, &str); 4] = [
    ("bench-side", "bench/Side.tsx", "ai.smelt.nsh-bench"),
    (
        "bench-side-idle",
        "bench/SideIdle.tsx",
        "ai.smelt.nsh-bench",
    ),
    ("bench-whole", "bench/Whole.tsx", "ai.smelt.nsh-bench"),
    ("bench-whole4", "bench/Whole4.tsx", "ai.smelt.nsh-bench"),
];

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let release = std::env::var("PROFILE").is_ok_and(|p| p == "release");
    println!("cargo:rerun-if-changed={}", root.join("bench").display());
    for (name, entry, id) in APPS {
        if let Err(message) = build(&root.join(entry), &out.join(name), id, release) {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}

fn build(entry: &Path, out: &Path, id: &str, release: bool) -> Result<(), String> {
    let ui_sets = vec![nsh_core::element::description::SetDescription::from(
        nsh_base::registry(),
    )];
    let bundle = nsh_bundler::bundle(&BundleOptions {
        entry: entry.to_path_buf(),
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
    write("app.json", format!(r#"{{"id":"{id}"}}"#).as_bytes())?;
    Ok(())
}
