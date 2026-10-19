# 代码审查：过时注释与过时代码

**审查时点**：2026-10-19 约 13:50–17:45，工作区 `main`，HEAD `4e8ecc3` + 未提交改动。

**重要前提**：审查期间工作区有**并行的开发改动**（远程运行时升级功能：`RuntimeStatus::Upgrading`、`push_upgrade`、`docs/remote-runtime.html` 的 P4 等），部分文件在 17:26 之后被大幅重写。因此本文每条发现都标注了**当前状态**（仍存在 / 已被并行改动顺带修复）。行号以各自核实时的文件为准。

**范围**：Rust 387 个文件约 25 万行，另有 `packages/pi-agent`（TS 1.35 万行）、`mobile/`（Dart 2.6 万行）、`plugins/`、`website/`、文档与构建脚本。
**方法**：机械扫描（零调用方符号、失效引用、intra-doc link、注释中的符号/路径引用、未使用依赖）+ 分区人工深读。

**总体结论**：这个仓库的注释质量明显高于平均水平——没有注释掉的代码块、没有 `#[deprecated]`、没有失效的 intra-doc link、注释里引用的文件路径与 `crate::a::b` 路径全部有效、`TODO` 只有 2 处（还是搜索测试的 fixture）。真正的问题有三类：**构建脚本引用了不存在的文件**、**一批零调用方的函数/常量**，以及**重构后错位的文档块**。

---

## 一、仍存在的问题

### 1.1 `ci/mac/entitlements.plist` 不存在，签名会静默丢掉 entitlements（最高优先）

`ci/` 下只有 `android/`、`ios/`、`linux/`、`lib/`，**没有 `mac/`**（`ci/mac/ci.sh`、`ci/mac/env-setup.sh` 已从历史中删除）。

```bash
# scripts/package-mac.sh:294
ENTITLEMENTS="$ROOT/ci/mac/entitlements.plist"
# :306
[[ -f "$ENTITLEMENTS" ]] && standard_sign_opts+=( --entitlements "$ENTITLEMENTS" )
```

- 该 plist **从未被 git 跟踪**，全仓库没有任何生成它的逻辑（`find . -name '*entitlements*'` 只命中 `mobile/ios/` 下两个无关文件），`git check-ignore` 也确认它没被忽略。
- `-f` 守卫让条件恒假 → **Developer ID 签名静默不带 entitlements**。而 295-301 行的注释明确说这份例外是给 GPUI 的 wgpu/libloading/dlopen2 依赖用的（Hardened Runtime 只允许加载同 Team 签名的库）。
- 同一个脚本 :257、:319 还提示「公证在 `ci/mac/ci.sh` 的 notarize 阶段做」「下一步：`./ci/mac/ci.sh notarize`」——该脚本不存在。
- `ci/linux/env-setup.sh:25` 也写「和 `rust-toolchain.toml`、`ci/mac/env-setup.sh` 保持一致」。

本地自签名走 `else` 分支不受影响；只有正式发版命中。`scripts/test-package-mac-preflight.sh` 没有覆盖这一项。

### 1.2 零调用方的公开项

判定：全仓库 grep 只出现在定义处（含测试与注释），且不在 trait impl 内（已排除 gpui 回调、serde、objc delegate、flutter bridge 生成代码）。

| 位置 | 符号 | 说明 |
|---|---|---|
| `smelt-core/src/agent_definition_store.rs:708-733` | `list_/create_/update_/delete_/sync_agent_definition` **五个** wrapper | 实际工作全走 `*_on(store, …)` 变体；`smelt-store` 里的同名方法容易误判，逐条 grep 确认无调用方 |
| `smelt-core/src/agent_definition_store.rs:116` | `load_session_agent_definition_ids` | 文档写着「调用方可以直接查」，但零调用方 |
| `smelt-core/src/acp_session/mod.rs:1234` | `is_cancel_requested` | 紧邻的 `accepts_mid_turn_steer` 直接用 `state.cancel_requested` 字段 |
| `smelt-core/src/new_session.rs:75` | `stored_new_session_sections` | 注释称「守护/移动端侧的完整目录」，两端都没调 |
| `smelt-core/src/session_control.rs:872` | `list_history` | smeltd 用的是 `list_history_for` |
| `smelt-core/src/session_metadata.rs:347` | `resume_agent_definition_id` | — |
| `smelt-model/src/new_session.rs:305` | `find_action` | 注释称「移动端只回传 key」，移动端未使用 |
| `smelt-model/src/session_control.rs:323` | `workspace_projects` | 按 `order` 排序的小工具 |
| `smelt-model/src/acp_session.rs:1325` | `to_line_bytes` | `AcpSnapshotFrame` 的固有方法 |
| `smelt-runtime-client/src/local.rs:39` | `daemon_response_with_timeout` | 与在用的 `daemon_request_with_timeout` 孪生 |
| `smelt-event-protocol/src/lib.rs:74` | `string_id!` 生成的 `into_string` | 宏用于 5 个类型，但没人调用 `.into_string()` |
| `smelt-event-protocol/src/lib.rs:8` | `MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION` | 协商只比对 `EVENT_BUS_PROTOCOL_VERSION`（`smeltd/src/protocol/subscribe.rs:187`）。对照组 `CONTROL_API_MIN_SUPPORTED_VERSION` 是有用的。该常量由 2026-10-17「删除旧插件体系」引入后无人使用 |
| `smelt-event-bus/src/lib.rs:25-26` | `DEFAULT_CURSOR_BATCH_SIZE` / `DEFAULT_CURSOR_MAX_WAIT_MS` | 同组 `DEFAULT_WRITER_*` 都在用，只有这两个 cursor 常量零引用 |
| `smelt-ui/src/motion.rs:225` | `PRESS_MS` | 同文件其它动画常量都在用 |

`#[allow(dead_code)]` 只有 7 处，全部是平台/测试条件编译，属于正当用法，没有用来掩盖死代码。

### 1.3 重构后错位的文档块

这类最隐蔽：注释本身写得很好，只是挂错了函数。

1. **`smelt-core/src/provider_quota.rs:576`** — 整段「从 Grok CLI 的 `auth.json` 里挑出 grok.com 会话 bearer」挂在 `fn local_cursor_access_token()`（Cursor 的 state.vscdb）上方；真正该有文档的 `fn grok_token_from_auth_json`（:630）反而没有。
2. **`smelt-core/src/acp_session/mod.rs:276`** — 描述「从交接快照重建活体状态」的整段文档，和「工具参数表变了」的文档挤在一起，落在 `fn note_tool_debug_changed`（只递增 generation）上方；真正的 `fn from_snapshot`（:288）无文档。
3. **`smelt-core/src/acp_session/mod.rs:1229`** — 「取消后窗口期不能 steer」的文档挂在 getter `is_cancel_requested` 上，而这段逻辑全在 `accepts_mid_turn_steer` 里。

### 1.4 死代码：重构后的兼容壳

- **`smelt-core/src/agent_bus.rs:5`** — 注释说跨 Agent 开关「和 Agent UI 配置共用同一个文档」，实际读 SQLite 类型化快照；为旧 JSON 写的 `cross_agent_enabled_from_config` 只剩 `#[cfg(test)]`。
- **`smelt-core/src/agent_definition_store.rs:28`** — `AgentUiProjection` 的 `Deserialize`（含遗留 `alias = "agent_presets"`）运行时不再使用，只有单测反序列化。

### 1.5 Cargo.toml 里声明但零引用的依赖

| crate | 未引用依赖 |
|---|---|
| `smelt` | `reqwest`、`futures-util` |
| `smelt-acp-view` | `smelt-sys`、`serde` |
| `smelt-client` | `smelt-paths` |
| `smelt-core` | `smelt-event-protocol` |
| `smelt-ui` | `smelt-sys` |

建议先用 `cargo machete` 复核再删；其中 `smelt-core` 声明了事件协议 crate 却零引用，与它「客户端与运行时共用逻辑」的定位对照看更像重构残留。

### 1.6 `smeltd` 的交接文档仍在描述 v1 的 exec 语义

`handoff_v2` 早已改为「spawn successor + 老进程退出」（两阶段 READY/COMMIT），且生产启动不再读交接文件，但多处文档仍按 exec-self 写：

- `smeltd/src/main.rs:27` — `{"op":"upgrade"} → 回 {"ok":true} 后 exec 磁盘上的新二进制`（实际是 spawn successor）
- `smeltd/src/main.rs:683` —「upgrade 从快照到 exec 全程持有这把锁」；`main.rs:251` 的 SPAWN_GATE 理由仍写「upgrade 清 CLOEXEC 准备 exec」，而 v2 用 SCM_RIGHTS 直接 dup，不做 CLOEXEC 手术
- `smeltd/src/handoff_v2/predecessor.rs:12` —「hosted 会话永不进 blockers」，但 `acp_upgrade_blockers` 对 hosted/direct 一视同仁
- `smeltd/src/handoff_v2/mod.rs:6` —「transport/predecessor/successor 后续补齐；legacy 文件读端保留一代兼容」，实际兄弟模块全部存在、legacy 读端已删
- `smeltd/src/acp_runtime_host.rs:120` —「session host 仍是同一个直接子进程，可继续按原 pid waitpid」，而 v2 successor 不是父进程（走 child_monitor 收养）
- `smeltd/src/remote.rs:4`、`:329`、`:569` —「exec 前停掉」「exec 不跑 Drop」
- `smeltd/src/handoff_v2/successor.rs:226`、`predecessor.rs:324` — 引用不存在的 `--import-handoff` 模式与 `SMELTD_MENUBAR`（实际用 `SMELTD_HANDOFF_SOCK`）

> 这批来自 `smeltd` 分区深读，抽查过的几条（:27、:683、predecessor.rs:12）属实；其余建议按行号复核后一并改。

---

## 二、已被并行改动顺带修复的（记录供核对）

以下在审查期间成立，收尾时确认已修——都在你那轮远程运行时改动里被一起处理了：

| 原问题 | 现状 |
|---|---|
| 根 `Cargo.toml:8`「三个二进制（smeltd / gateway / smelt-notify）」 | 已改为 `smelt-notify / smelt-agent-mcp` |
| `smelt-core/src/lib.rs:1`「守护（smeltd / gateway）共用」 | 已改为「守护（smeltd）」 |
| `smelt-core/src/session_control.rs` 中带「GUI and gateway callers」注释的 `remote_session_snapshot` | 整个函数已删除（确认无调用方，安全） |
| `docs/workspace.md` 列着已删的 `smelt-process-wait`、`smelt-remote-gateway`，且缺 5 个 crate 的说明 | 已完整重写，18 个 crate 全部覆盖 |
| `crates/smelt-mobile/src/iroh_tunnel.rs:392-396` 的 `#[ignore]` 测试教怎么跑已删的 `smelt-iroh-host` | 已清理 |
| `crates/smelt/src/main.rs:4140` 注释指向已删的 `smelt_remote_gateway::agent_from_launch` | 已清理 |
| `README.zh-CN.md` 列举 `gateway` | 已清理 |

**仍残留 1 处**：`README.md:74`「axum for the remote gateway」——远程网关已删，但 `axum` 确实还在用（`smeltd/src/webhook.rs`，自动化外部触发），所以这句是**用途说错了**，不是依赖消失了。我最初把这条写成「axum 依赖也随之消失」，是错的，已在此更正。

---

## 三、机械检查结果（均为通过，列出以免重复劳动）

- **注释掉的代码**：0 处
- **`#[deprecated]`**：0 处
- **`TODO`/`FIXME`/`XXX`/`HACK`**：Rust 侧 2 处（`pi_rpc.rs` 把 "TODO" 当搜索关键词的测试 fixture）；TS/Dart/JS 侧 0 处
- **intra-doc link 失效**：0 处
- **注释中 `crate::a::b` 路径失效**：0 处
- **注释中引用的文件路径**：无失效（`auth.json`、`mcp.json` 等是外部工具配置文件，属正当引用）
- **`Makefile` / `scripts/` 引用的二进制**：`smelt`、`smeltd`、`smelt-notify`、`smelt-agent-mcp`、`smelt-installer` 全部存在
- **gpui-kit rev 一致性**：根 `[patch]` 与 5 个 crate 的 `gpui-kit` 全部 `af456ddb…`，与注释一致
- **`.gitignore` 条目**：全部是「本就不该存在于仓库」的忽略项，无失效

---

## 四、建议的修复顺序

1. **`ci/mac/entitlements.plist`**（1.1）——唯一可能影响线上产品的项。要么把 plist 补回仓库（注释已写清要求：无 XML 注释的最精简 plist，只放 library validation 例外），要么删掉这段签名逻辑与注释；同时决定公证步骤是恢复还是改指向。
2. **错位文档块**（1.3）——改动最小、收益最直接。
3. **`smeltd` 的 exec 时代文档**（1.6）——按行号批量改。这批最影响后来人理解交接机制。
4. **死代码**（1.2、1.4）——删五个 wrapper 前先确认不是「生产路径本该走 default_store 却自己造了 store」的漏接线。
5. **未使用依赖**（1.5）——`cargo machete` 复核后删。
