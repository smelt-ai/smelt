# smelt

Mac 上的个人知识蒸馏引擎。`smelt` 在后台监听你的 shell 历史，调用 LLM 从中
提炼出可复用的编码 **instinct**（编码直觉 / 习惯），写入 `~/.smelt/global.md`
—— 一个 Claude Code（或任何工具）都能读取、用来理解「你是怎么干活的」的文件。

> 状态：可用的原型（working prototype）。当前仅产出全局 instinct，项目级作用域为预留。

## 工作原理

```
observe ──监听 ~/.zsh_history──▶ digest ──DeepSeek──▶ SQLite ──▶ global.md
                                   ▲                               ▲
                                   └─────────── merge（语义去重）───┘
```

1. **observe**：监听 `~/.zsh_history`，文件变化时触发一次蒸馏；两次之间至少间隔 30 分钟（节流，避免频繁调 API）。
2. **digest**：汇聚多个数据源（shell 历史 + 可选的「最近改动文件」元数据 + Claude Code 会话提问）→ 过滤敏感行 → 调 DeepSeek 提炼 3–5 条 instinct → 写入 SQLite → 刷新 `global.md`。
3. **merge**：当条目变多时，让 LLM 把语义重复的 instinct 归并；`confidence` 取组内最大值、`evidence_count` 求和，**均在本地计算**，不交给 LLM，保证数值准确。

## 安装

```sh
cargo build --release
# 可执行文件位于 ./target/release/smelt，按需放进 PATH
```

## 配置

最简单的方式是跑 `smelt init` 交互式生成配置。或手动把 DeepSeek 的 API Key 放进
`~/.smelt/config.toml`，或导出环境变量 `DEEPSEEK_API_KEY`：

```toml
DEEPSEEK_API_KEY = "sk-..."
```

读取顺序：**先环境变量，后配置文件**。Key 不会写入仓库。

可选：配置要扫描的代码目录（冒号分隔，支持 `~`），开启「最近改动文件」数据源；不配置则该源静默跳过：

```toml
scan_dirs = "~/dev:~/work"
```

## 命令

```sh
smelt init      # 交互式首次配置：写 config.toml、可选注册自启（推荐第一步）
smelt observe   # 启动后台采集守护进程（前台阻塞，通常交给 LaunchAgent 托管）
smelt digest    # 立即手动触发一次蒸馏
smelt merge     # 对已有 instinct 做语义去重合并
smelt scan      # 预览分身发现的最近改动文件（采集范围 / 隐私核对）
smelt sessions  # 预览从 Claude Code 会话提取的提问（采集 / 隐私核对）
smelt show      # 按 confidence 降序打印当前 instinct
smelt install   # 注册 macOS LaunchAgent，开机自启 observe
```

### 开机自启

```sh
smelt install
launchctl load ~/Library/LaunchAgents/com.smelt.plist
```

`install` 会写入 plist（`RunAtLoad` + `KeepAlive`），日志输出到
`~/.smelt/observe.log` 与 `~/.smelt/observe.err.log`。

## 接入 Claude Code

`~/.smelt/global.md` 是纯 markdown，把它的内容（或引用）放进你的
`CLAUDE.md` / 全局指令，Claude 就能读到这些蒸馏出的 instinct。文件由 smelt
自动生成，**请勿手动编辑**——下次蒸馏会被覆盖。

## 数据与隐私

- 全部数据留在**本地**：`~/.smelt/` 下的 SQLite 与 markdown，不上传任何服务（除调用 LLM 提炼外）。
- 发送给 LLM 前，`digest` 会**逐行过滤**疑似含密钥/凭据的历史（命中 `key`/`token`/`secret`/`password`/`sk-`/`ghp_`/`-----begin`/`ssh-rsa` 等关键词的行直接剔除，并打印剔除行数）。
- 「最近改动文件」源**只采集路径与扩展名等元数据，绝不读取文件内容**，并自动跳过 `.git`/`target`/`node_modules` 等目录和隐藏文件。
- 「Claude Code 会话」源**只提取你的提问文本**，跳过工具结果与系统注入，并复用同一套敏感词过滤（含疑似密钥的提问整条丢弃）。所有原始会话内容都留在本地，仅脱敏后的提问参与提炼。
- 这是 best-effort 的启发式过滤，**不是安全保证**；敏感操作请自行确认。

## 输出文件

| 路径 | 用途 |
| --- | --- |
| `~/.smelt/global.md` | 渲染后的 instinct（主输出） |
| `~/.smelt/smelt.db` | SQLite 存储 |
| `~/.smelt/config.toml` | API Key / 配置 |
| `~/.smelt/observe.log` · `observe.err.log` | LaunchAgent 日志 |

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `DEEPSEEK_API_KEY` | DeepSeek API Key（优先于 config.toml） |
| `SMELT_HISTORY_FILE` | 覆盖默认的 `~/.zsh_history` 路径（便于测试） |
| `SMELT_SCAN_DIRS` | 要扫描的代码目录（冒号分隔，优先于 config.toml 的 scan_dirs） |

## Instinct 数据结构

```rust
struct Instinct {
    id: String,            // 内容哈希（FNV-1a，稳定去重）
    content: String,       // instinct 正文
    confidence: f32,       // 置信度，clamp 到 0.3–0.9
    domain: Vec<String>,   // 领域标签，如 ["rust", "git"]
    evidence_count: u32,   // 支撑证据出现次数
    last_seen: String,     // 最近观察时间（RFC3339）
    scope: Scope,          // Global | Project（Project 为预留）
}
```

## 技术栈

Rust 2021 · tokio · clap · serde · rusqlite · reqwest · notify · anyhow · chrono · dirs。
LLM 后端：DeepSeek（`deepseek-chat`，OpenAI 兼容接口）。

## 测试

```sh
cargo test
```

`digest` 模块覆盖了稳定哈希、JSON 解析（含 markdown 围栏容错）、敏感行过滤等用例。

## License

MIT —— 见 `LICENSE`。
