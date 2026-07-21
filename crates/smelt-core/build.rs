//! 保证 `remote-web/dist` 在编译期存在，供 `rust-embed` 打进 smeltd/gateway。
//!
//! Docker / CI 若只跑 `cargo build` 而不先 `npm run build`，以前会回退到旧 HTML，
//! 手机端样式全乱。这里在缺产物时自动尝试 npm；仍失败则写入占位页，避免编译挂掉，
//! 但日志会打 warning。
//!
//! **必须住在 smelt-core**：rust-embed 的 `#[folder]` 在编译本 crate 时展开，build
//! script 得赶在那之前跑；`rerun-if-changed` 也得挂在本 crate 上，SPA 重新构建后
//! smelt-core 才会重编、把新产物嵌进去。放根 crate 两条都不成立（依赖先于根编译）。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    // 本 crate 在 crates/smelt-core，remote-web 在仓库根。
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
<title>smelt remote — 未构建 SPA</title></head>
<body style="font-family:system-ui;padding:1.5rem;background:#0a0a0c;color:#ececef">
<h1>remote-web 未构建</h1>
<p>Docker / CI 请在 <code>cargo build</code> 之前执行：</p>
<pre style="background:#161618;padding:1rem;border-radius:8px">cd remote-web && npm ci && npm run build</pre>
<p>或确保镜像里有 Node，让 build.rs 自动跑 npm。</p>
</body></html>
"#,
        )
        .expect("write remote-web/dist stub");
        println!(
            "cargo:warning=remote-web/dist 缺失：已嵌入占位页。发布前务必 npm run build，否则手机端样式错误。"
        );
    } else {
        // 粗测：真实 Vite 产物会引用 /assets/
        if let Ok(html) = fs::read_to_string(&index) {
            if !html.contains("/assets/") && !html.contains("assets/") {
                println!(
                    "cargo:warning=remote-web/dist/index.html 看起来不像 Vite 产物，请检查构建步骤"
                );
            }
        }
    }

    // rerun-if-changed 的相对路径按 CARGO_MANIFEST_DIR（crates/smelt-core）解析，
    // 必须用拼好的绝对路径指向仓库根下的 remote-web。
    println!("cargo:rerun-if-changed={}", index.display());
    println!("cargo:rerun-if-changed={}", remote_web.join("package.json").display());
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
    let ci = Command::new(&npm)
        .args(["ci"])
        .current_dir(remote_web)
        .status();
    if !matches!(ci, Ok(s) if s.success()) {
        // lock 或离线失败时退到 install
        let _ = Command::new(&npm)
            .args(["install"])
            .current_dir(remote_web)
            .status();
    }
    let build = Command::new(&npm)
        .args(["run", "build"])
        .current_dir(remote_web)
        .status();
    match build {
        Ok(s) if s.success() => println!("cargo:warning=remote-web npm build 成功"),
        Ok(s) => println!("cargo:warning=remote-web npm build 失败，exit={s}"),
        Err(e) => println!("cargo:warning=remote-web npm build 无法启动：{e}"),
    }
}

fn which_npm() -> Option<PathBuf> {
    if let Ok(p) = env::var("NPM") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let out = Command::new("which").arg("npm").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}
