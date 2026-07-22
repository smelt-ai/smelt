//! 保证 remote-web/dist 存在，供 rust-embed 打进 smelt-signal（公网信令同域托管 SPA）。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest.parent().unwrap().parent().unwrap().to_path_buf();
    let remote_web = repo_root.join("remote-web");
    let dist = remote_web.join("dist");
    let index = dist.join("index.html");

    if !index.is_file() {
        try_npm_build(&remote_web);
    }

    if !index.is_file() {
        let _ = fs::create_dir_all(dist.join("assets"));
        fs::write(
            &index,
            r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"/><meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>smelt signal — SPA 未构建</title></head>
<body style="font-family:system-ui;padding:1.5rem;background:#0a0a0c;color:#ececef">
<h1>remote-web 未构建</h1>
<p>发布前：<code>cd remote-web && npm ci &amp;&amp; npm run build</code></p>
</body></html>
"#,
        )
        .expect("write stub");
        println!("cargo:warning=remote-web/dist 缺失：smelt-signal 将嵌入占位页");
    }

    println!("cargo:rerun-if-changed={}", index.display());
    println!(
        "cargo:rerun-if-changed={}",
        remote_web.join("package.json").display()
    );
    if let Ok(entries) = fs::read_dir(dist.join("assets")) {
        for e in entries.flatten() {
            println!("cargo:rerun-if-changed={}", e.path().display());
        }
    }
}

fn try_npm_build(remote_web: &Path) {
    if !remote_web.join("package.json").is_file() {
        return;
    }
    let npm = which_npm();
    let Some(npm) = npm else {
        println!("cargo:warning=未找到 npm，无法自动构建 remote-web");
        return;
    };
    println!("cargo:warning=remote-web/dist 缺失，尝试 npm ci && npm run build …");
    let _ = Command::new(&npm)
        .args(["ci"])
        .current_dir(remote_web)
        .status();
    let _ = Command::new(&npm)
        .args(["run", "build"])
        .current_dir(remote_web)
        .status();
}

fn which_npm() -> Option<PathBuf> {
    Command::new("npm")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| PathBuf::from("npm"))
}
