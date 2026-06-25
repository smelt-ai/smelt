//! install：写入 Mac LaunchAgent plist，实现开机自启 `smelt observe`。

use anyhow::{Context, Result};

const LABEL: &str = "com.smelt";

/// 写入 ~/Library/LaunchAgents/com.smelt.plist。
pub fn run() -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("无法定位 home 目录"))?;
    let exe = std::env::current_exe().context("无法获取当前可执行文件路径")?;
    let exe_str = exe.to_string_lossy();
    let home_str = home.to_string_lossy();

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe_str}</string>
        <string>observe</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{home_str}/.smelt/observe.log</string>
    <key>StandardErrorPath</key>
    <string>{home_str}/.smelt/observe.err.log</string>
</dict>
</plist>
"#
    );

    let dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{LABEL}.plist"));
    std::fs::write(&path, plist).with_context(|| format!("写入 {:?} 失败", path))?;

    println!("已写入 LaunchAgent: {:?}", path);
    println!("执行以下命令加载：\n  launchctl load {:?}", path);
    Ok(())
}
