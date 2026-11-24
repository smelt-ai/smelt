---
name: smelt-rust
description: >-
  Project-specific Rust, GPUI, crate layering, runtime, and build/format conventions for
  Smelt (`crates/`). Read before editing Rust code in `crates/`, changing crate dependencies,
  writing GPUI `render` paths, adding timers/polling/waits, or running Cargo/rustfmt/snapshot
  checks in Smelt.
---

# Smelt Rust 开发约定

## crate 分层与依赖

- `crates/` 按层次和能力分、不按功能分（线上格式、共享类型、运行时内部、客户端访问、存储、组件库、插件接口与各能力 crate），应用（`smelt`、`smeltd`、`smelt-mobile`、`smelt-push-relay`）只做组装，不含功能代码。
- Rust 不认识任何插件：不写插件的视图、命令、布局 id，删掉任意插件 Rust 不用改一行。
- `smelt-model` 只放至少两端（运行时、桌面、手机）都用到的类型和纯函数。
- `plugins/` 承载全部 JS/TSX 应用插件（由 `crates/smelt-nsh` 运行时打包并在独立 QuickJS VM 中执行）；`crates/` 仅保留桌面宿主与能力 crate，新增/拆分功能先按 `docs/app-plugins.md` 区分「Rust 能力 + JS 插件」。crate 依赖规则由 `scripts/lint-crate-deps.sh` 检查。

## render 只读内存，不做 IO

- GPUI 视图重绘高频，每次重绘整段重跑 `render`。`render`（含它调用的 `render_*`、页面构建函数）里不读写 SQLite、不读文件、不起进程、不整份克隆大的全局状态。
- 耗时与 IO 操作放到选中、数据变化等事件或后台任务里，结果缓存在视图状态上，render 只读缓存。
- debug 构建下 `Workspace::render` 期间读写客户端库会在 stderr 报调用栈（`smelt_client_store::render_guard`），看到立即修，不要忽略。

## 等待一律事件驱动

- 等某件事发生时，阻塞在真正的事件上：进程退出（kqueue / pidfd / 进程句柄，见 `smelt_sys::process_wait`）、Condvar、channel、socket / 管道可读、文件变化通知、推送与全局状态订阅。
- 禁止 sleep 轮询循环、常驻周期定时器、拿短超时反复醒来和忙等。空闲时（无会话在跑、窗口在后台）`smelt` 和 `smeltd` 不应有周期唤醒。
- 超时只能当事件等待的上限；失败后的重试须退避有上限，能等到恢复事件（重新上线、Nudge 等）就提前叫醒。
- 确实没有事件可等（远端服务、第三方库心跳等），在定时器旁写一行注释：`// 轮询例外：<为什么不能用事件>；周期 <N>，上限 <M>。`

## 构建、测试与格式化

- 改完自己编译并跑相关测试，读错误自己修，不丢给用户。分批串行跑改动涉及的 crate：`cargo check -j 2 -p <crate>`、`cargo test -j 2 -p <crate>`（别跑整个 workspace、别 `-j 4+` 防 OOM 137；cargo 共用 target 锁，只能串行）。
- 判断测试通过看 `test result` 里的 `passed` 数，不只看退出码（包名或过滤写错时全部 filtered out 照样 exit 0）。
- 改了 `smelt-ui` 组件外观，跑 `cargo test -p smelt-ui --test snapshots`（可用 `SMELT_SNAPSHOT=<名字>`）核对 `target/tmp/snapshots/smelt-ui/` 深浅 PNG，新组件或状态补进 `tests/snapshots.rs`。
- 不跑 `cargo fmt`（main 非 fmt 干净，会改大量无关文件）。只检查自己改动的文件：`rustfmt --edition 2024 --config-path rustfmt.toml --emit stdout <文件> | diff <文件> -`。
