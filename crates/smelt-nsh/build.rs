//! 构建期任务：
//! 1. 把 `bench/` 压测入口打包、编成字节码，放到 `$OUT_DIR/<名字>/bundle.nshc` 给 `examples/bench.rs`；
//! 2. 把主壳 `shell/MainShell.tsx` 连同 `src/plugins/` 外壳桥、SDK 与内置对话组件预打包、编成 QuickJS 字节码
//!    `$OUT_DIR/main-shell/bundle.nshc`，供 `MainShell::new` 冷启时零打包直接装载。

use std::{
    fs,
    path::{Path, PathBuf},
};

use nsh_bundler::BundleOptions;
use nsh_core::element::description::{ExportDescription, ExportKind, SetDescription};

/// （名字，入口，应用 id）。
const APPS: [(&str, &str, &str); 7] = [
    ("bench-side", "bench/Side.tsx", "ai.smelt.nsh-bench"),
    (
        "bench-side-idle",
        "bench/SideIdle.tsx",
        "ai.smelt.nsh-bench",
    ),
    ("bench-whole", "bench/Whole.tsx", "ai.smelt.nsh-bench"),
    ("bench-whole4", "bench/Whole4.tsx", "ai.smelt.nsh-bench"),
    (
        "bench-chat-stream",
        "bench/ChatStream.tsx",
        "ai.smelt.nsh-bench",
    ),
    (
        "bench-chat-stream-30",
        "bench/ChatStream30.tsx",
        "ai.smelt.nsh-bench",
    ),
    (
        "bench-chat-stream-idle",
        "bench/ChatStreamIdle.tsx",
        "ai.smelt.nsh-bench",
    ),
];

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let release = std::env::var("PROFILE").is_ok_and(|p| p == "release");

    println!("cargo:rerun-if-changed={}", root.join("build.rs").display());
    println!("cargo:rerun-if-changed={}", root.join("bench").display());
    println!("cargo:rerun-if-changed={}", root.join("shell").display());
    println!(
        "cargo:rerun-if-changed={}",
        root.join("src/plugins").display()
    );

    if let Err(message) = build_main_shell(&root, &out, release) {
        eprintln!("{message}");
        std::process::exit(1);
    }

    for (name, entry, id) in APPS {
        if let Err(message) = build(&root.join(entry), &out.join(name), id, release) {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}

/// 与 `src/module.rs::registry()` 一致的纯数据描述（避免 `build.rs` 链接整个 `smelt-nsh`）。
fn smelt_host_set_description() -> SetDescription {
    SetDescription {
        specifier: "smelt/host".to_owned(),
        exports: vec![
            ExportDescription {
                name: "Icon".to_owned(),
                kind: ExportKind::Component,
            },
            ExportDescription {
                name: "Vector".to_owned(),
                kind: ExportKind::Component,
            },
            ExportDescription {
                name: "Smelt".to_owned(),
                kind: ExportKind::Module,
            },
        ],
        declarations: String::new(),
        reexported_values: Vec::new(),
        reexported_types: Vec::new(),
        native_values: Vec::new(),
        assets: Vec::new(),
        undocumented: Vec::new(),
    }
}

fn build_main_shell(root: &Path, out: &Path, release: bool) -> Result<(), String> {
    let stage = out.join("main-shell-stage");
    let target = out.join("main-shell");
    let _ = fs::remove_dir_all(&stage);
    let _ = fs::remove_dir_all(&target);

    let plugins_dir = root.join("src/plugins");
    let copies = [
        (plugins_dir.join("shell.nsh.json"), "nsh.json"),
        (plugins_dir.join("App.tsx"), "App.tsx"),
        (plugins_dir.join("provider.tsx"), "provider.tsx"),
        (plugins_dir.join("slots.tsx"), "slots.tsx"),
        (plugins_dir.join("sdk.tsx"), "sdk.tsx"),
        (root.join("shell/HostSettings.tsx"), "HostSettings.tsx"),
        (root.join("shell/MainShell.tsx"), "Shell.tsx"),
    ];
    for (src, rel) in copies {
        copy_file(&src, &stage.join(rel))?;
    }

    let ui_sets = vec![
        SetDescription::from(nsh_base::registry()),
        smelt_host_set_description(),
    ];
    let bundle = nsh_bundler::bundle(&BundleOptions {
        entry: stage.join("App.tsx"),
        minify: release,
        split: false,
        ui_sets: ui_sets.clone(),
        development: !release,
        ..Default::default()
    })
    .map_err(|e| e.plain)?;
    let strip = if release {
        nsh_js::Strip::Source
    } else {
        nsh_js::Strip::Nothing
    };
    let bytecode =
        nsh_js::compile_bytecode_with(&bundle.code, strip, &ui_sets).map_err(|e| e.message)?;

    fs::create_dir_all(&target).map_err(|e| format!("create {}: {e}", target.display()))?;
    fs::write(target.join("bundle.nshc"), &bytecode)
        .map_err(|e| format!("write {}: {e}", target.join("bundle.nshc").display()))?;
    let _ = fs::remove_dir_all(&stage);
    Ok(())
}

fn copy_file(src: &Path, dst: &Path) -> Result<(), String> {
    println!("cargo:rerun-if-changed={}", src.display());
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    fs::copy(src, dst)
        .map(|_| ())
        .map_err(|e| format!("copy {} -> {}: {e}", src.display(), dst.display()))
}

fn build(entry: &Path, out: &Path, id: &str, release: bool) -> Result<(), String> {
    let ui_sets = vec![SetDescription::from(nsh_base::registry())];
    let bundle = nsh_bundler::bundle(&BundleOptions {
        entry: entry.to_path_buf(),
        minify: release,
        split: false,
        ui_sets: ui_sets.clone(),
        development: !release,
        ..Default::default()
    })
    .map_err(|e| e.plain)?;
    let strip = if release {
        nsh_js::Strip::Source
    } else {
        nsh_js::Strip::Nothing
    };
    let bytecode =
        nsh_js::compile_bytecode_with(&bundle.code, strip, &ui_sets).map_err(|e| e.message)?;
    let _ = fs::remove_dir_all(out);
    fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let write = |name: &str, bytes: &[u8]| -> Result<(), String> {
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
