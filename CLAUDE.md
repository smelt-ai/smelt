# smelt

Mac 上的个人知识蒸馏引擎。后台监听用户行为，调用 Claude API 提炼 instincts，
写入 ~/.smelt/global.md 供 Claude Code 读取。

## 技术栈
- Rust 2021，tokio async
- clap（CLI）
- serde + serde_json
- rusqlite（~/.smelt/smelt.db）
- reqwest（调用 Claude API）
- notify（文件监听）
- anyhow（错误处理）

## CLI
smelt observe   # 启动后台采集守护进程
smelt digest    # 手动触发一次蒸馏
smelt show      # 打印当前 instincts
smelt install   # 注册 Mac LaunchAgent 开机自启

## 输出文件
~/.smelt/global.md        # 全局 instincts（主输出）
~/.smelt/projects/<name>/ # 项目级（按 git remote 区分）

## Instinct 结构
{ id, content, confidence: f32(0.3-0.9), domain: Vec<String>,
  evidence_count: u32, last_seen, scope: Global|Project }

## 原则
- 每步 cargo check 通过再继续
- 配置放 ~/.smelt/config.toml，包含 ANTHROPIC_API_KEY
