# smelt-core 领域拆分指南

## 📋 目录

1. [问题分析](#问题分析)
2. [拆分方案](#拆分方案)
3. [具体步骤](#具体步骤)
4. [迁移策略](#迁移策略)
5. [实施时间表](#实施时间表)
6. [风险与缓解](#风险与缓解)

---

## 问题分析

### 当前状况

`crates/smelt-core/src/lib.rs` 包含 60+ 个 module：

```
lib.rs (60+ modules)
├─ 终端领域      (8 个)  term_text, terminal_theme, tty_color, osc, ...
├─ ACP/Agent 领域 (15 个) acp_conn, acp_chat, acp_client, acp_session, ...
├─ 自动化/PI      (10 个) automation, automation_store, pi_auth, pi_rpc, ...
├─ 项目/会话      (10 个) conversation, session_control, session_history, ...
├─ 存储/持久化    (8 个)  sqlite_state, daemon_state, session_metadata, ...
├─ 网络/远程      (6 个)  control_api, daemon_protocol, remote_config, ...
└─ 其他杂项      (3 个)  codex_app_server, copilot_quota, plugin_enablement, ...
```

### 问题

- ⚠️ **单一职责原则违反** - 60+ modules 在一个 crate 中很难维护
- ⚠️ **发现困难** - 新成员不知道该在哪里找代码
- ⚠️ **依赖关系混乱** - 跨域 module 互相依赖
- ⚠️ **代码重用性差** - 某些领域的代码无法独立打包/分享
- ⚠️ **编译时间长** - 全部混在一起，改一个文件要重新编整个 crate

---

## 拆分方案

### 目标状态（拆分后）

```
crates/
├─ smelt-core/                     ← 瘦身为纯基础设施
│  ├─ Cargo.toml
│  └─ src/lib.rs (thin re-export)
│
├─ smelt-terminal-core/           ← 新建（终端领域）
│  ├─ Cargo.toml
│  └─ src/
│     ├─ term_text.rs
│     ├─ terminal_theme.rs
│     ├─ tty_color.rs
│     ├─ osc.rs
│     └─ lib.rs
│
├─ smelt-agent-core/              ← 新建（Agent/ACP 领域）
│  ├─ Cargo.toml
│  └─ src/
│     ├─ acp_conn.rs
│     ├─ acp_chat.rs
│     ├─ acp_client.rs
│     ├─ acp_session/
│     ├─ agent_kind.rs
│     ├─ agent_status.rs
│     ├─ agent_definition.rs
│     ├─ pi_rpc.rs
│     └─ lib.rs
│
├─ smelt-automation-core/         ← 新建（自动化领域）
│  ├─ Cargo.toml
│  └─ src/
│     ├─ automation.rs
│     ├─ automation_store.rs
│     ├─ automation_transcript.rs
│     └─ lib.rs
│
├─ smelt-session-core/            ← 新建（会话管理领域）
│  ├─ Cargo.toml
│  └─ src/
│     ├─ conversation.rs
│     ├─ session_control.rs
│     ├─ session_history.rs
│     ├─ session_metadata.rs
│     ├─ daemon_state.rs
│     └─ lib.rs
│
├─ smelt-project-core/            ← 新建（项目领域）
│  ├─ Cargo.toml
│  └─ src/
│     ├─ project_catalog.rs
│     ├─ isolated_workspace.rs
│     ├─ workspace_menu.rs
│     └─ lib.rs
│
└─ smelt-remote-core/             ← 新建（远程操控）
   ├─ Cargo.toml
   └─ src/
      ├─ control_api.rs
      ├─ daemon_protocol.rs
      ├─ remote_config.rs
      └─ lib.rs
```

### 依赖关系（拆分后的架构）

```
第 1 层（基础设施）
├─ smelt-paths
├─ smelt-pairing
├─ smelt-event-bus
└─ smelt-store

       ↓

第 2 层（领域层）
├─ smelt-terminal-core      （无其他 -core 依赖）
├─ smelt-agent-core         （依赖：store）
├─ smelt-automation-core    （依赖：store）
├─ smelt-session-core       （依赖：store）
├─ smelt-project-core       （依赖：store）
└─ smelt-remote-core        （无其他 -core 依赖）

       ↓

第 2.5 层（兼容层）
└─ smelt-core/lib.rs        （re-export 所有上面的 crate）
   ← 提供过渡期的兼容性，允许调用方不改导入语句

       ↓

第 3+ 层（GUI/daemon）
├─ smeltd
├─ smelt
└─ 其他 crate
```

### 模块映射表

| 原位置 | 新位置 | 优先级 |
|--------|--------|--------|
| **终端领域** |  |  |
| term_text | smelt-terminal-core/ | P1.1 |
| terminal_theme | smelt-terminal-core/ | P1.1 |
| tty_color | smelt-terminal-core/ | P1.1 |
| osc | smelt-terminal-core/ | P1.1 |
| terminal_view 相关工具 | smelt-terminal-core/ | P1.1 |
| **ACP/Agent 领域** |  |  |
| acp_conn | smelt-agent-core/ | P1.2 |
| acp_chat | smelt-agent-core/ | P1.2 |
| acp_client | smelt-agent-core/ | P1.2 |
| acp_session/ | smelt-agent-core/ | P1.2 |
| agent_kind | smelt-agent-core/ | P1.2 |
| agent_status | smelt-agent-core/ | P1.2 |
| agent_bus | smelt-agent-core/ | P1.2 |
| agent_definition | smelt-agent-core/ | P1.2 |
| agent_event | smelt-agent-core/ | P1.2 |
| pi_auth | smelt-agent-core/ | P1.2 |
| pi_rpc | smelt-agent-core/ | P1.2 |
| pi_model_discovery | smelt-agent-core/ | P1.2 |
| pi_model_settings | smelt-agent-core/ | P1.2 |
| codex_app_server | smelt-agent-core/ | P1.2 |
| **自动化/PI** |  |  |
| automation | smelt-automation-core/ | P1.3 |
| automation_store | smelt-automation-core/ | P1.3 |
| automation_transcript | smelt-automation-core/ | P1.3 |
| **项目/会话** |  |  |
| conversation | smelt-session-core/ | P1.4 |
| session_control | smelt-session-core/ | P1.4 |
| session_handoff | smelt-session-core/ | P1.4 |
| session_history | smelt-session-core/ | P1.4 |
| session_metadata | smelt-session-core/ | P1.4 |
| session_title | smelt-session-core/ | P1.4 |
| daemon_state | smelt-session-core/ | P1.4 |
| new_session | smelt-session-core/ | P1.4 |
| **项目领域** |  |  |
| project_catalog | smelt-project-core/ | P1.5 |
| isolated_workspace | smelt-project-core/ | P1.5 |
| workspace_menu | smelt-project-core/ | P1.5 |
| workspace_override | smelt-project-core/ | P1.5 |
| worktree_inherit | smelt-project-core/ | P1.5 |
| **远程操控** |  |  |
| control_api | smelt-remote-core/ | P1.6 |
| control_client | smelt-remote-core/ | P1.6 |
| daemon_protocol | smelt-remote-core/ | P1.6 |
| remote_config | smelt-remote-core/ | P1.6 |
| **保留在 smelt-core** |  |  |
| block_on | smelt-core/ | - |
| subprocess | smelt-core/ | - |
| app_log | smelt-core/ | - |
| 其他跨域通用工具 | smelt-core/ | - |

---

## 具体步骤

### Sprint 1：准备与设计（Week 4）

#### 1.1 创建新 crate 框架

```bash
cd crates

# 创建 6 个新 crate
cargo new --lib smelt-terminal-core
cargo new --lib smelt-agent-core
cargo new --lib smelt-automation-core
cargo new --lib smelt-session-core
cargo new --lib smelt-project-core
cargo new --lib smelt-remote-core

# 验证编译
cargo build --workspace
```

#### 1.2 更新 Cargo.toml

根目录 `Cargo.toml` 的 workspace 成员列表：

```toml
[workspace]
members = [
    "crates/smelt-paths",
    "crates/smelt-plugin-api",
    "crates/smelt-plugin-host",
    "crates/smelt-event-bus",
    "crates/smelt-store",
    "crates/smelt-pairing",
    
    # 新加的 6 个 crate
    "crates/smelt-terminal-core",
    "crates/smelt-agent-core",
    "crates/smelt-automation-core",
    "crates/smelt-session-core",
    "crates/smelt-project-core",
    "crates/smelt-remote-core",
    
    "crates/smelt-remote-gateway",
    "crates/smelt-core",        # 保留但瘦身
    "crates/smelt-git",
    # ... 其他
]
```

每个新 crate 的 `Cargo.toml` 示例（以 smelt-terminal-core 为例）：

```toml
[package]
name = "smelt-terminal-core"
version.workspace = true
edition.workspace = true

[lints]
workspace = true

# 终端领域的无 UI 业务逻辑：ANSI 解析、颜色处理、终端状态等
# 不依赖 GPUI，可供移动端和桌面端共用

[dependencies]
smelt-paths = { path = "../smelt-paths" }
smelt-store = { path = "../smelt-store" }
alacritty_terminal = { version = "0.26", default-features = false }
serde = { version = "1", features = ["derive"] }
anyhow = "1"
```

#### 1.3 创建迁移计划文档

创建 `docs/SMELT_CORE_REFACTORING.md`：

```markdown
# smelt-core 领域拆分计划

## 目标
将 60+ modules 的 smelt-core 拆分为 6 个专业库，提高可维护性。

## 时间表
- Sprint 1 (Week 4)：准备与设计
- Sprint 2 (Week 5-6)：迁移执行
- Sprint 3 (Week 7)：验证与优化

## 迁移顺序（按依赖关系）
1. smelt-terminal-core （无依赖）
2. smelt-project-core （无依赖）
3. smelt-remote-core （无依赖）
4. smelt-session-core （依赖 store）
5. smelt-automation-core （依赖 store）
6. smelt-agent-core （依赖 store）

## 验收标准
- [ ] 所有 crate 编译通过
- [ ] 无循环依赖
- [ ] 所有测试通过
- [ ] 文档更新完成
```

---

### Sprint 2：迁移执行（Week 5-6）

#### 2.1 并行迁移策略

分配 3 个工程师，并行处理 3 个 crate：

**Team A：终端领域（1 人）**
- 迁移文件：term_text.rs, terminal_theme.rs, tty_color.rs, osc.rs, ...
- 目标 crate：smelt-terminal-core
- 工作量：2-3 days

**Team B：项目领域（1 人）**
- 迁移文件：project_catalog.rs, isolated_workspace.rs, workspace_menu.rs, ...
- 目标 crate：smelt-project-core
- 工作量：2-3 days

**Team C：远程操控（1 人）**
- 迁移文件：control_api.rs, daemon_protocol.rs, remote_config.rs, ...
- 目标 crate：smelt-remote-core
- 工作量：2-3 days

**第二周继续（同 3 人）：**

**Team A：会话管理**
- 迁移文件：conversation.rs, session_control.rs, session_history.rs, ...
- 目标 crate：smelt-session-core
- 工作量：3-4 days

**Team B：自动化**
- 迁移文件：automation.rs, automation_store.rs, automation_transcript.rs
- 目标 crate：smelt-automation-core
- 工作量：2-3 days

**Team C：Agent/ACP**
- 迁移文件：acp_conn.rs, acp_chat.rs, agent_kind.rs, agent_status.rs, ...
- 目标 crate：smelt-agent-core
- 工作量：4-5 days（最复杂）

#### 2.2 每个 crate 的迁移步骤

以 smelt-terminal-core 为例：

**步骤 1：复制源文件**
```bash
# 从 smelt-core/src/ 复制到 smelt-terminal-core/src/
cp crates/smelt-core/src/term_text.rs crates/smelt-terminal-core/src/
cp crates/smelt-core/src/terminal_theme.rs crates/smelt-terminal-core/src/
cp crates/smelt-core/src/tty_color.rs crates/smelt-terminal-core/src/
cp crates/smelt-core/src/osc.rs crates/smelt-terminal-core/src/
```

**步骤 2：创建 lib.rs，整理导出**
```rust
// crates/smelt-terminal-core/src/lib.rs
//! 终端领域的无 UI 业务逻辑
//!
//! 包含：ANSI 解析、终端主题、颜色处理、OSC 通知等
//! 不依赖 GPUI，可供桌面端和移动端共用

pub mod term_text;
pub mod terminal_theme;
pub mod tty_color;
pub mod osc;

// 方便调用方
pub use term_text::*;
pub use terminal_theme::*;
pub use tty_color::*;
pub use osc::*;
```

**步骤 3：更新依赖**
```toml
# crates/smelt-terminal-core/Cargo.toml
[dependencies]
alacritty_terminal = { version = "0.26", default-features = false }
serde = { version = "1", features = ["derive"] }
anyhow = "1"
```

**步骤 4：验证编译**
```bash
cd crates/smelt-terminal-core
cargo build
cargo test
```

**步骤 5：检查循环依赖**
```bash
cargo tree --depth 3 | grep smelt-terminal-core
```

#### 2.3 解决依赖冲突

**问题 1：跨域依赖（common pattern）**

例如 acp_conn 需要用到 daemon_state：
```rust
// smelt-agent-core/src/acp_conn.rs
use smelt_session_core::daemon_state::DaemonState;  // ✅ 现在可以这样导入
```

**问题 2：循环依赖**

如果出现 A crate 依赖 B，B 又依赖 A 的情况：
```
smelt-agent-core → uses agent_status
smelt-session-core → uses agent_status
→ 出现循环依赖风险
```

**解决方案：**
1. 提取共享数据结构到 `smelt-core/src/common.rs`
2. 或者调整依赖方向（谁更"基础"，谁被依赖）

**检查循环依赖：**
```bash
# 验证工作空间内没有循环依赖
cargo check --workspace

# 详细分析
cargo tree --depth 5 | grep -E "^\w" | sort | uniq -d
```

---

### Sprint 3：验证与优化（Week 7）

#### 3.1 完整编译与测试

```bash
# 编译全部
cargo build --workspace

# 运行所有测试
cargo test --workspace

# 检查 lints
cargo clippy --all-targets

# 生成文档
cargo doc --no-deps --open
```

#### 3.2 更新调用方的导入语句

**方案 A：逐个更新（建议用这个）**

修改所有调用 smelt-core 的地方：
```rust
// 旧的（来自 smelt-core）
use smelt_core::acp_conn::AcpConnection;
use smelt_core::terminal_theme::TerminalTheme;

// 新的（来自各专业库）
use smelt_agent_core::acp_conn::AcpConnection;
use smelt_terminal_core::terminal_theme::TerminalTheme;
```

**涉及的 crate：**
- `crates/smelt/src/` - GUI（最多导入）
- `crates/smeltd/src/` - daemon（次多）
- `crates/smelt-ui/src/` - UI 基建（少量）
- `crates/smelt-acp-view/src/` - ACP 视图（中等）

**脚本自动化更新：**
```bash
#!/bin/bash
# scripts/update-imports.sh

# 批量替换导入语句
find crates/smelt/src -name "*.rs" -exec sed -i \
  's/use smelt_core::acp_conn::/use smelt_agent_core::acp_conn::/g' {} \;

find crates/smelt/src -name "*.rs" -exec sed -i \
  's/use smelt_core::terminal_theme::/use smelt_terminal_core::terminal_theme::/g' {} \;

# ... 类似的批量替换
```

**方案 B：兼容层（过渡方案）**

在 smelt-core/src/lib.rs 中保留 re-export：
```rust
// crates/smelt-core/src/lib.rs
//! 兼容层：re-export 所有新的 domain-specific crate
//! 
//! 这是一个过渡期的兼容层，便于调用方不改导入语句。
//! 建议在下一个 major version 中删除。

pub use smelt_terminal_core::*;
pub use smelt_agent_core::*;
pub use smelt_automation_core::*;
pub use smelt_session_core::*;
pub use smelt_project_core::*;
pub use smelt_remote_core::*;

// 或者更精细地控制导出
pub mod acp_conn {
    pub use smelt_agent_core::acp_conn::*;
}

pub mod terminal_theme {
    pub use smelt_terminal_core::terminal_theme::*;
}

// ... 其他
```

这样调用方**不需要改导入语句**就能工作（暂时），但编译时会有 deprecation warning。

#### 3.3 文档更新

新增/修改文件：

**docs/architecture/crate-guidelines.md**（新建）
```markdown
# Crate 设计指南

## Crate 分层

### 第 1 层：基础设施
- smelt-paths：路径和沙箱
- smelt-event-bus：事件总线
- smelt-store：数据存储
- smelt-pairing：配对格式

### 第 2 层：领域库（无 UI）
- smelt-terminal-core：终端处理
- smelt-agent-core：Agent/ACP
- smelt-automation-core：自动化运行
- smelt-session-core：会话管理
- smelt-project-core：项目管理
- smelt-remote-core：远程操控

### 第 3 层：平台库
- smelt-plugin-host：插件管理
- smelt-iroh：P2P 隧道
- smeltd：守护进程
- smelt-remote-gateway：远程网关

### 第 4 层：表现层
- smelt-ui：UI 基建
- smelt-acp-view：ACP UI
- smelt-webview：WebView
- smelt：GUI 主程序

## 新增 Crate 的检查清单

新增 crate 时需要：
- [ ] 明确职责范围（1-2 句话说清楚）
- [ ] 确定应该在哪一层
- [ ] 列出依赖的其他 crate
- [ ] 编写 lib.rs 的文档注释
- [ ] 添加至少 3 个单元测试
- [ ] 更新这个文档

## 何时新增 Crate

新增 crate 的条件：
1. **跨多个 crate 共用** 的代码 → 新建独立 crate
2. **职责单一** - 一个 crate 做一件事
3. **可以独立测试** - 不需要整个应用启动
4. **可以独立打包** - 将来可能要单独发布

不要新增 crate 当：
- 只有一个地方用这些代码
- 代码太小（< 500 lines）
- 与现有 crate 有重叠职责
```

**更新 Cargo.toml 的注释**

每个新 crate 的 Cargo.toml 头部都要有清晰的注释：

```toml
# crates/smelt-agent-core/Cargo.toml

[package]
name = "smelt-agent-core"
version.workspace = true
edition.workspace = true

# Agent/ACP 领域的无 UI 业务逻辑：
# - 连接层（ACP JSONRPC）
# - 对话模型（消息、权限、工具）
# - Agent 定义、状态、总线
# - Pi/Codex/OpenCode 等特定 agent 的初始化
#
# 不依赖 GPUI，GUI 和 daemon 都使用这个 crate
#
# 这个 crate 曾经是 smelt-core 的一部分（60+ modules）
# 在 v0.10 版本分离出来，提高可维护性
```

---

## 迁移策略

### 打包发布策略

**v0.9.x（当前）**
- smelt-core 仍保留所有 60+ modules
- 新增 6 个 -core 专业库
- smelt-core/lib.rs 做 re-export 兼容层

**v0.10（下个大版本）**
- smelt-core 正式瘦身
- 移除 re-export 兼容层
- 所有调用方已迁移到新 crate
- 发布 MIGRATION.md 指南

**v1.0（未来）**
- 考虑把各个 domain-core 发布到 crates.io
- 允许第三方使用这些库

### 依赖版本策略

```toml
# 所有 -core crate 使用 workspace 版本
[package]
version.workspace = true  # 0.10.0 时统一更新

# 但可以单独发布 crates.io
# 使用 0.10.0, 0.11.0 等，逐个版本
```

---

## 实施时间表

### 详细计划（3 周）

| 周 | Team A | Team B | Team C | 同步点 |
|----|--------|--------|--------|--------|
| W4 | 创建 crate 框架 | 更新 Cargo.toml | 设计依赖关系 | 周五：确认框架无误 |
| W5 | terminal-core | project-core | remote-core | 周三：合并到 main |
| W5-W6 | session-core | automation-core | agent-core | 周五：所有 crate 可编译 |
| W7 | 更新导入语句 | 运行完整测试 | 文档更新 | 周五：merge 完成 |

### 工作量估算

| 任务 | 工程师-天 | 关键路径 |
|------|---------|---------|
| 框架准备 | 1×3 = 3 | 第一周 |
| 6 个 crate 迁移 | 3×10 = 30 | 第二、三周 |
| 更新导入语句 | 3×5 = 15 | 第三周 |
| 测试与修复 | 3×3 = 9 | 全程 |
| 文档更新 | 1×2 = 2 | 第三周 |
| **总计** | **59 人-天** | **3 周（3 人并行）** |

---

## 风险与缓解

### 风险 1：循环依赖

**风险：** Session 依赖 Agent，Agent 又依赖 Session

**缓解方案：**
1. 提前设计依赖关系（使用 cargo tree 验证）
2. 跨域共享数据结构放在 smelt-store
3. 事件驱动替代直接依赖（通过 EventHub）

**检查方法：**
```bash
# 每次迁移后立即检查
cargo tree --workspace | grep -A5 -B5 "cycle"
```

### 风险 2：遗漏文件

**风险：** 忘记迁移某个 module，导致编译失败

**缓解方案：**
1. 使用清单（模块映射表）
2. 从 lib.rs 中删除已迁移的 module
3. CI 检查编译成功

**验证方法：**
```bash
# 编译完成后验证
grep -n "mod " crates/smelt-core/src/lib.rs | wc -l
# 应该从 60+ 降到 10 以下
```

### 风险 3：性能下降

**风险：** 过多的 crate 导致编译时间增加

**缓解方案：**
1. 使用 cargo-build-cache
2. 启用 LTO 仅在 release 时
3. 并行构建各 crate（cargo 默认支持）

**验证方法：**
```bash
# 对比迁移前后的编译时间
time cargo build --release --workspace

# 应该差异不大（< 10%）
```

### 风险 4：文档不完整

**风险：** 新 crate 的职责不清晰，用户不知道用哪个

**缓解方案：**
1. 每个 crate 的 lib.rs 有完整的文档注释
2. docs/crate-guidelines.md 明确分层
3. 例子代码展示怎么用

**验证方法：**
```bash
# 生成文档，在浏览器中查看
cargo doc --open --no-deps

# 确保所有 module 都有文档注释
```

---

## 成功指标

迁移完成的标准：

- [ ] 所有 6 个新 crate 编译通过
- [ ] `cargo check --workspace` 无警告
- [ ] `cargo clippy` 无新的 clippy 警告
- [ ] `cargo test --workspace` 所有测试通过
- [ ] 无循环依赖（`cargo tree` 检查）
- [ ] lib.rs 文档 100% 有注释
- [ ] Cargo.toml 头部注释清晰
- [ ] docs/crate-guidelines.md 更新完毕
- [ ] 所有调用方的导入语句已更新
- [ ] CI 通过（GitHub Actions）

---

## 回滚计划

万一出现严重问题（如大范围循环依赖），回滚方案：

1. **不要立即删除文件** - 保留在新 crate，但在 git 分支中
2. **在 smelt-core 保留双份** - 新旧并存，标记废弃
3. **发布过渡版本** - v0.9.1 说明情况
4. **给用户时间迁移** - 下一个 minor version 才删除旧文件

**快速回滚命令：**
```bash
git checkout main
git reset --hard <before-refactor-commit>
cargo clean
cargo build
```

---

## 预期收益

完成拆分后：

| 指标 | 前 | 后 | 收益 |
|------|----|----|------|
| lib.rs 行数 | ~2000 | ~200 | 提高可读性 |
| 平均 module 大小 | 大杂烩 | 单一职责 | 易于维护 |
| 新成员入门时间 | 难以定位 | 快速找到相关 crate | 降低学习曲线 |
| 独立打包可能性 | 不可行 | 每个 -core 都可独立 | 提高复用性 |
| 循环依赖风险 | 易发生 | 架构清晰 | 降低 bug 率 |
| 编译时间 | 单体 | 并行 | 体验改善 |

