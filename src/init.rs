//! init：交互式首次配置引导。建立 ~/.smelt/config.toml（写入 DeepSeek key、
//! 可选的 scan_dirs），并可顺手注册开机自启，把首次上手的手动步骤收进一个命令。

use crate::db;
use crate::install;
use anyhow::{Context, Result};
use std::io::{self, Write};

pub fn run() -> Result<()> {
    let dir = db::smelt_dir()?;
    let cfg = dir.join("config.toml");

    println!("🔧 smelt 初始化");
    println!("配置目录: {:?}\n", dir);

    if cfg.exists() && !confirm("config.toml 已存在，覆盖？", false)? {
        println!("已保留现有配置。");
        return maybe_install();
    }

    // 1. DeepSeek API Key（必填）。注意：输入会在终端回显。
    let key = prompt("请输入 DeepSeek API Key (sk-...): ")?;
    let key = key.trim();
    if key.is_empty() {
        anyhow::bail!("未输入 key，已取消初始化。");
    }

    // 2. 采集范围（可选）。默认用 Spotlight 扫描整个 home。
    println!("\n采集范围：默认用 Spotlight 全盘扫描你的 home（零配置，推荐）。");
    let scan = prompt("如需缩小到指定目录，输入冒号分隔路径；直接回车=默认全盘: ")?;

    // 3. 写入配置。
    let content = render_config(key, &scan);
    std::fs::write(&cfg, &content).with_context(|| format!("写入 {:?} 失败", cfg))?;
    println!("✅ 已写入 {:?}", cfg);

    // 4. 可选注册开机自启。
    maybe_install()
}

/// 询问是否注册开机自启；同意则调用 install。
fn maybe_install() -> Result<()> {
    if confirm("\n现在注册开机自启（后台采集守护进程）？", true)? {
        install::run()?;
    } else {
        println!("已跳过。之后可随时运行 `smelt install`。");
    }
    println!("\n完成 🎉 试试 `smelt scan` 看分身发现了哪些文件，或 `smelt digest` 立即提炼一次。");
    Ok(())
}

/// 渲染 config.toml 内容（纯函数，便于测试）。
fn render_config(key: &str, scan_dirs: &str) -> String {
    let mut c = format!("DEEPSEEK_API_KEY = \"{}\"\n", key.trim());
    let scan = scan_dirs.trim();
    if !scan.is_empty() {
        c.push_str(&format!("scan_dirs = \"{scan}\"\n"));
    }
    c
}

/// 打印提示并读取一行输入。
fn prompt(msg: &str) -> Result<String> {
    print!("{msg}");
    io::stdout().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    Ok(s)
}

/// Y/N 确认，带默认值。
fn confirm(msg: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("{msg} {hint} ");
    io::stdout().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return Ok(default_yes);
    }
    Ok(matches!(s.as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_config_without_scan_dirs() {
        let c = render_config("sk-abc", "");
        assert_eq!(c, "DEEPSEEK_API_KEY = \"sk-abc\"\n");
        assert!(!c.contains("scan_dirs"));
    }

    #[test]
    fn render_config_with_scan_dirs() {
        let c = render_config("  sk-abc  ", "  ~/dev:~/work  ");
        assert!(c.contains("DEEPSEEK_API_KEY = \"sk-abc\"\n"));
        assert!(c.contains("scan_dirs = \"~/dev:~/work\"\n"));
    }
}
