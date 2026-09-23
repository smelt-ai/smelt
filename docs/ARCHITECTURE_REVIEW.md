# Smelt 项目架构综合审查报告

## 执行摘要

Smelt 是一个 macOS 原生桌面应用，采用 Rust + GPUI + 微服务架构。
**整体架构非常符合现代软件工程最佳实践**，但存在少量改进空间。

### 架构得分：8.5/10

| 维度 | 评分 | 备注 |
|------|------|------|
| **分层隔离** | 9/10 | 优秀的单一职责原则 |
| **依赖管理** | 9/10 | 清晰的层级关系，无循环依赖 |
| **可扩展性** | 8/10 | 插件化设计良好，但缺乏通用易用的扩展点 |
| **错误处理** | 7.5/10 | 统一模式，但缺乏顶层恢复机制 |
| **测试覆盖** | 8/10 | 有单元/集成测试，缺全链路E2E |
| **文档完整性** | 9/10 | 文档详尽，架构清晰 |
| **性能考量** | 8/10 | 异步设计恰当，但缺少显式优化指南 |
| **安全设计** | 8/10 | 多数场景考虑周全，P2P有尚未覆盖的风险 |

---

## 一、宏观架构评价

### 1.1 核心设计哲学（✅ 优秀）

**遵循原则：**
- ✅ 单一职责：每个 crate 职责明确
- ✅ 关注点分离：UI/业务逻辑/存储完全独立
- ✅ 无缝升级设计：`handoff_v2` 实现零停机升级
- ✅ 插件化架构：支持第三方扩展而不侵入主干

**符合的最佳实践：**
- SOLID 原则（特别是 Single Responsibility）
- Clean Architecture（分层明确）
- 微服务思想（daemon/gateway/notify 各司其职）
- 事件驱动（EventHub）

### 1.2 monorepo 的权衡（✅ 良好）

**优点：**
- 统一版本号管理（workspace 中央管理）
- 共享依赖版本（Cargo.lock 唯一真源）
- 编译器强制边界检查（crate 间不能有 GPUI 泄漏）

**潜在风险：**
- ⚠️ 18 个 crate，复杂度已现端倪
- 📋 建议：制定 crate 新增审核清单

---

## 二、分层架构评价

### 2.1 整体层级（✅ 优秀）

```
第 1 层（基础设施）
├─ smelt-paths          → 路径/沙箱管理
├─ smelt-pairing        → 配对码格式
├─ smelt-event-bus      → EventHub
└─ smelt-store          → SQLite/KV 存储

第 2 层（领域层/无 UI 业务逻辑）
├─ smelt-core           → 终端/ACP/权限/自动化/项目 ⚠️ 职责过宽
├─ smelt-git            → Git 操作（repo 发现/diff/写）✅ 独立性强
├─ smelt-plugin-api     → 插件协议 DTO
└─ smelt-remote-gateway → 远程网关（无 GPUI）

第 3 层（平台层）
├─ smelt-plugin-host    → 插件进程管理
├─ smelt-iroh           → P2P 隧道
├─ smelt-mobile         → 移动端桥
└─ smeltd               → 守护进程 ✅ 架构优秀

第 4 层（表现层/UI）
├─ smelt-ui             → 共享 UI 基建
├─ smelt-acp-view       → ACP 对话 UI
├─ smelt-webview        → WebView 容器
└─ smelt                → GUI 主程序（GPUI）
```

**评价：** ✅ 依赖指向单向，自上而下流动

### 2.2 核心 Crate 评价

#### ❌ `smelt-core` 职责过宽

**问题：** 包含 60+ 个 module，跨越 6 个业务域
- 终端领域 / ACP 领域 / 自动化/PI / 项目/会话 / 存储/持久化 / 网络/远程

**建议的重构：** 拆分为 5-6 个专业库
- `smelt-terminal-core/` ← 终端领域
- `smelt-agent-core/` ← Agent/ACP 领域
- `smelt-automation-core/` ← 自动化领域
- `smelt-session-core/` ← 会话管理领域
- `smelt-project-core/` ← 项目领域
- `smelt-remote-core/` ← 远程操控

**优先级：** 🟠 P1（4-6 周，可分 3 个 sprint 推进）

#### ✅ `smeltd` 守护进程架构优秀

**亮点：**
- 单一入口，event loop 清晰
- 会话隔离完善（session_catalog / terminal_registry）
- 无缝升级成熟（`handoff_v2` 4000+ 行，两阶段提交）
- ANSI keyframe 快照（而非字节重放），避免 CSI 中断

#### ⚠️ `smelt-acp-view` 与 `smelt-ui` 职责边界模糊

**建议明确化：**
- `smelt-ui` → 真正跨模块的基建（色板/md/image）
- `smelt-acp-view` → ACP 专用 UI（消息/工具卡片）

---

## 三、关键架构决策评价

### 3.1 GUI vs Daemon 分离（✅ 杰出）

**优点：**
- ✅ GUI 崩溃，会话继续运行（tmux 式）
- ✅ 无缝升级时 PTY 直接交接
- ✅ 多客户端观看（watch 模式）
- ✅ 移动端/远程网关共用一个 daemon

**值得其他 Electron/GPUI 应用参考。**

### 3.2 无缝升级的 `handoff_v2`（✅ 杰出）

**设计三要素：**
1. SPAWN_GATE 阻止新 fork
2. READY/COMMIT 两阶段提交
3. ANSI keyframe 而非字节重放

**优点：**
- ✅ 零停机升级
- ✅ 任何步骤失败都能回滚
- ✅ PTY/ACP fd 直接交接

**缺点：**
- 复杂性极高（4000+ 行）→ 维护难度大

### 3.3 事件驱动状态管理（✅ 良好但不完美）

**现状：** EventHub 是神经中枢，GUI 订阅事件

**问题：**
- ⚠️ 缺乏全局状态机：状态转移散在各处
- ⚠️ 事件缺乏强类型检查
- ⚠️ 没有单一真源（各自管理各自状态）

**建议：** 引入 Redux/Elm-style 状态管理（可选）
**优先级：** 🟡 P2（可维护性优化，非关键路径）

### 3.4 SQLite 数据源（✅ 符合指导原则）

**遵循的原则：** "持久化数据使用 SQLite，不使用 JSON"

**现状：**
- ✅ 所有业务数据进 SQLite
- ✅ JSON 仅用于：网络传输/配置文件/临时数据

**建议：** 补充 schema ERD 文档 + 数据保留政策

---

## 四、跨层设计评价

### 4.1 远程访问架构（⚠️ 有显著风险）

**三层设计：**
1. 嵌入式网关 (`remote_start/stop`) → 内网回环 + token
2. iroh 隧道 (`iroh_start/stop`) → P2P 打洞 + 中继
3. 移动客户端 (iOS/Android) → Flutter

**现有安全考虑：**
- ✅ 配对码一次性使用
- ✅ 默认关闭
- ✅ 绑定回环

**潜在风险：**

**R1: token 管理不完善**
- 问题：同机恶意程序可读 token 文件 → 伪造手机连接
- 建议：一次性 token → session key 转换，或 TLS client cert

**R2: iroh 中继的信任根**
- 问题：跨网络依赖第三方中继，可能嗅探/篡改
- 建议：文档化是否端到端加密，如不是则加 TLS/noise

**R3: 手机离线的状态一致性**
- 缓解：`REMOTE_VIEWPORT_GRACE` 已有
- 建议：明确超时时间常量与恢复策略

**优先级：** 🔴 P0（2-3 weeks，影响用户安全）

### 4.2 插件生态（✅ 良好，但还需完善）

**支持类型：** Bun/Node.js / Web / 未来 Native

**现有设计亮点：**
- ✅ 插件 API 独立 crate
- ✅ 宿主与插件进程隔离
- ✅ 版本兼容性检查

**缺陷：**
- ⚠️ 权限模型不清（能否调用所有 API？能否读 agent 定义？）
- ⚠️ 插件市场不存在
- ⚠️ 同一进程的插件缺乏隔离

**建议：** 实施 Capability-based 权限模型
**优先级：** 🟠 P1（3-4 weeks，等插件生态发展）

### 4.3 Git 领域层（✅ 值得称赞）

**优点：**
- ✅ 完全无 UI 依赖（甚至无 GPUI）
- ✅ 充分测试
- ✅ 文件监听而非轮询

**建议：** 发布为独立 crate（crates.io），供其他项目使用

---

## 五、测试架构评价

### 5.1 现状分析

**已有：**
- ✅ 单元测试 + 集成测试
- ✅ 无缝升级测试（4000+ 行 `handoff_v2_tests.rs`）
- ✅ ACP/自动化/持久化测试

**缺失：**
- ⚠️ 没有 E2E 测试（GUI → daemon 全链路）
- ⚠️ 没有并发压力测试
- ⚠️ 没有负向测试（故障恢复）
- ⚠️ 没有 code coverage 工具

**优先级：** 🔴 P0（2-3 weeks，建立 E2E 框架）

### 5.2 改进计划

**短期（1-2 sprint）：**
- 加入属性测试（proptest socket protocol fuzzing）
- 编写 E2E 测试框架（无头 GUI + daemon 集成）
- 覆盖 3-5 个关键用户流程

**中期（3-6 个月）：**
- 压力测试（1000 会话并发）
- 故障恢复测试（网络/存储故障注入）
- 添加 code coverage（tarpaulin/llvm-cov）

---

## 六、依赖管理审查

### 6.1 关键依赖风险

| 依赖 | 版本 | 健康度 | 风险 |
|------|------|--------|------|
| GPUI | git commit 钉死 | ⚠️ 中 | Zed 更新时需手动同步；长期缺陷可能无法修复 |
| gpui-component | git commit 钉死 | ⚠️ 中 | 同上 |
| agent-client-protocol | =2.1.0 精确版本 | ⚠️ 中 | 可能无法获得 bug 修复 |
| tokio | 1 (semver) | ✅ 好 | 活跃维护 |
| serde | 1 (semver) | ✅ 好 | 事实标准 |

### 6.2 改进建议

**1. GPUI 依赖管理：**
- 方案 A：创建 fork + CI 每周跟踪上游
- 方案 B：等 GPUI 发布到 crates.io 后迁移

**2. 版本锁定放松：**
- 将 `agent-client-protocol = "=2.1.0"` 改为 `"~2.1"`（允许 patch 更新）

**3. CI 补充依赖审计：**
- 添加 `cargo audit` 检查安全漏洞
- 每月生成依赖清单报告

---

## 七、编译和部署

### 7.1 构建系统（✅ 成熟）

**优点：**
- ✅ Makefile 前端清晰
- ✅ 不依赖完整 Xcode（`runtime_shaders` feature）
- ✅ Rust 版本锁定（rust-toolchain.toml）

**建议：**
- 添加 `make ci-check` 快速验证 target（pre-commit）
- 明确化 feature flag 文档

### 7.2 发版流程（⚠️ 缺少文档）

**现状：**
- ✅ 版本号统一管理
- ✅ CI 校验
- ❌ 缺 CHANGELOG.md
- ❌ 缺语义化版本指南

**建议：**
1. 补充 CHANGELOG.md（Keep a Changelog 格式）
2. 编写 RELEASE.md 指导流程
3. 自动生成 release notes（从 commit 消息）

### 7.3 二进制管理（⚠️ 需明确化）

**当前有 4 个二进制：** smelt / smeltd / gateway / smelt-notify

**缺失的打包策略：**
- `smelt.dmg` 中是否包含 gateway？
- 用户如何单独运行 gateway（远程调试）？
- smelt-notify 如何分发？

**建议：**
- 主 DMG：包含 smelt + smeltd + smelt-notify
- 额外 "Smelt CLI Tools"：gateway / smelt-sync-plugins

---

## 八、运维与可观测性

### 8.1 日志系统（⚠️ 不成熟）

**现状：**
- `app_log.rs` 存在但缺乏文档
- 没有统一配置（level/rotate/output）
- 没有结构化日志（JSON）

**建议的改进：**
- 统一日志配置（使用 tracing/structlog）
- 本地轮转保存（~/.smelt/smelt.log）
- 结构化 JSON 输出

**优先级：** 🟡 P2（1 week，完善质量）

### 8.2 性能监控（⚠️ 缺失）

**建议添加：**
- Metrics：socket 延迟 / terminal grid 更新 / 内存占用
- 本地 prometheus 端点
- Profiling 指南（如何采集 flame graph）

**优先级：** 🟡 P2（1-2 weeks）

### 8.3 崩溃收集（⚠️ 不明确）

**建议：**
- 本地崩溃日志保存
- 可选的自动上报（Sentry）

---

## 九、安全架构审查

### 9.1 输入验证（✅ 基本完善）

**检查覆盖：**
- ✅ socket 协议版本化
- ✅ ACP 消息 schema 校验
- ⚠️ 文件路径：能否跳出 workspace？
- ⚠️ 命令注入：shell 命令转义？

**建议：** 补充路径和命令的深度检查

### 9.2 权限模型（⚠️ 不完整）

**现状：**
- 只有 Approve/Deny/Reply 三态
- 缺乏细粒度权限（read/write/exec/network）

**建议：** 实施 Capability-based 权限模型
```rust
pub enum AgentPermission {
    ReadWorkspace,
    WriteWorkspace,
    ExecuteShell,
    AccessSecrets { allowed_paths: Vec<PathBuf> },
    NetworkAccess,
}
```

**优先级：** 🔴 P0（高，特别是对外开放插件时）

### 9.3 数据隐私（⚠️ 政策不清）

**敏感数据流向不明确：**
- Claude 对话 → Claude API
- 本地代码 → ACP prompt
- 用户 SSH key → ？

**建议：**
- 编写 PRIVACY.md
- 明确声明：什么数据永不离开本机，什么会上报

**优先级：** 🟠 P1（2 weeks）

---

## 十、社区与维护性

### 10.1 代码风格（✅ 优秀）

**工具：** rustfmt + clippy + Conventional Commits

**lints 配置：**
```toml
[workspace.lints.clippy]
dbg_macro = "deny"      # 禁止 dbg!
todo = "deny"           # 禁止 todo!（push 前必清）
```

**评价：** ✅ 零容忍政策很好，推荐所有项目采用

### 10.2 文档质量（✅ 非常好）

**已有：**
- README（双语）
- workspace.md（feature + 架构）
- docs/ 深度设计文档
- 代码注释详尽

**缺失：**
- ⚠️ 架构决策记录（ADR）
- ⚠️ 贡献指南（如何新增 crate）
- ⚠️ API 文档（OpenAPI spec）

**建议：**
```
docs/
├─ architecture/
│  ├─ adr/          ← 架构决策记录
│  └─ crate-guidelines.md
├─ api/
│  ├─ control-api.openapi.yaml
│  └─ plugin-api.openapi.yaml
└─ contributing/
   ├─ README.md
   └─ pull-request-checklist.md
```

**优先级：** 🟢 P3（持续积累）

---

## 十一、总结与建议

### 核心优点（按重要性排序）

1. **daemon + GUI 解耦** —— tmux 式设计，GUI 崩溃不影响会话
2. **无缝升级成熟** —— handoff_v2 零停机升级
3. **文档详尽** —— 架构决策清晰
4. **SQLite 数据源** —— 类型安全，符合最佳实践
5. **事件驱动** —— 异步 + 响应式更新

### 主要缺陷（按影响力排序）

1. **远程安全模型模糊** —— token 管理/认证边界不清
2. **测试覆盖不完全** —— 缺 E2E/压力测试
3. **smelt-core 职责过宽** —— 60+ module，需拆分
4. **全局状态管理散** —— 缺乏单一真源/状态机
5. **可观测性不足** —— 缺日志/metrics/trace

---

## 优先级改进清单

### 🔴 P0（关键，1-2 sprint）

1. **远程访问安全加固** (2-3 weeks)
   - 一次性 token → session key 转换
   - docs/SECURITY.md 威胁模型

2. **E2E 测试框架** (2-3 weeks)
   - 无头 GUI 测试能力
   - 3-5 个关键流程覆盖

### 🟠 P1（重要，1-3 months）

3. **smelt-core 领域重构** (4-6 weeks，3 sprint)
   - 拆分为 5-6 专业库
   - 迭代推进

4. **插件权限模型** (3-4 weeks)
   - Capability-based 权限
   - 权限检查与审计日志

5. **数据隐私政策** (2 weeks)
   - PRIVACY.md 文档
   - 第三方数据流向说明

### 🟡 P2（改善，backlog）

6. **日志系统完善** (1 week)
   - 统一配置 + 结构化日志
   - 本地轮转

7. **发版流程自动化** (1 week)
   - CHANGELOG 生成
   - 语义化版本检查

8. **Metrics/Profiling** (1-2 weeks)
   - 关键路径采集
   - 本地 prometheus 端点

### 🟢 P3（可选，持续）

9. **架构决策记录（ADR）**
   - docs/adr/ 补充
   - 每次重大决策都记录

10. **Cargo.toml 精简**
    - 整理依赖注释
    - 明确化 features

---

## 最终评价

### 总体结论

**smelt 项目架构设计水准高于行业平均。**

特别优秀的地方：
- daemon + GUI 分离（tmux 式）
- 无缝升级实现（零停机）
- 多协议支持（ACP/control/remote/iroh）

需要重点投入的三个方向：
1. **安全性** —— 补齐认证/授权/审计
2. **可维护性** —— 领域重构 + 状态统一
3. **可观测性** —— 日志/metrics/trace 体系

### 建议的下一阶段重点

**推荐 timeline：**
- **Month 1**：完成 P0 远程安全 + E2E 框架
- **Month 2-3**：启动 smelt-core 拆分 (Sprint 1-2)
- **Month 3-4**：完成 smelt-core 拆分 (Sprint 3) + 插件权限模型

---

## 附录：参考文献

本审查遵循以下最佳实践：
- SOLID 原则（Martin Robert C.）
- Clean Architecture（Uncle Bob）
- Microservices Architecture（Newman Sam）
- Release It! 2nd Edition（Nygard Michael T.）
- Building Microservices（Newman Sam）

文档版本：v1.0
审查日期：2025-01-23
