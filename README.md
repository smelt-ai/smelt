# smelt

Mac 上的 AI coding 驾驶舱：一个基于 [GPUI](https://gpui.rs) 的桌面工作台，内嵌真终端，
专为「同时指挥多个 Claude Code agent 干活」设计——多项目 × 多标签，会话状态一目了然。

> 状态：working prototype，持续迭代中。

## 运行

```sh
cargo run --bin workspace   # GUI 主程序
```

后台还有一个可选的持久化守护进程：

```sh
cargo run --bin smeltd   # 类 tmux：GUI 退出/崩溃不影响 shell 存活，重开 GUI 自动 reattach
```

## 已实现能力

- 多标签内嵌终端：完整 ANSI/256 色/24-bit 色、Nerd Font、IME、框选复制、滚动回看
- 文件树浏览 + 文件名/内容搜索
- Git diff 视图
- 桌面宠物（可选接 LLM 大脑，OpenAI 兼容协议）
- 终端会话持久化（`smeltd`，字节流 + 重放 + 尺寸协商）

详细架构与已实现功能清单见 [`docs/workspace.md`](docs/workspace.md)，
待做点子见 [`docs/roadmap.md`](docs/roadmap.md)。

## 技术栈

Rust 2021 · tokio · GPUI + gpui-component · portable-pty · alacritty_terminal ·
smol · syntect · similar · reqwest · anyhow。

## 测试

```sh
cargo check --all-targets
cargo test
```

## License

MIT —— 见 `LICENSE`。
