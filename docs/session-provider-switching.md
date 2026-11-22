# 同一 session 切换 provider

对应 [issue #369](https://github.com/smelt-ai/smelt/issues/369)。状态：已实现，假 ACP 回归及真实 Codex→Pi→Codex 验收通过；Claude 尚未完成真实验收。

## 行为

应用 session（`WorkspaceAcp.sid`）、工作目录和侧栏对话保持不变。Codex、Claude、Pi
等运行时各自保有原生会话；profile / agent definition 不同也属于不同续接身份。
切换只能影响下一轮，不能隐式取消当前工作或自动发出新请求。

首次切到 B 时新建 B 的原生会话；切回 A 时恢复 A 的原生 ID。下一条用户请求带上目标
尚未见过的可共享历史，受字符预算约束，注明来源、覆盖范围及省略。隐藏思考、旧审批
responder 和原始工具调用不能变成发给目标的新指令。参考 T3 的
[身份分层](https://github.com/pingdotgg/t3code/blob/main/docs/orchestration-v2/provider-switching-and-context.md)
和[切换决策](https://github.com/pingdotgg/t3code/blob/main/apps/server/src/orchestration-v2/ProviderSessionTransitionPolicy.ts)。

## 现有边界

- `WorkspaceAcp` / `acp_detail` 只有当前 agent 和单个历史 ID。
- `ensure_acp_session_inner` 更换启动规格时优先使用已知历史 ID；不能直接用它跨 provider。
- 原生恢复会重建当前 provider 的消息投影；不能把混合历史当成某一家的原生历史重放。
- 原文由各 provider 自己的存档持有，运行时的投影应继续从这些权威来源恢复。
- 守护已提供生命周期锁、旧进程退出确认、原生会话独占、输入闸门和启动后配置，可复用。
- 当前上游存储版本是 40。新增关系状态必须有一次版本迁移，不能塞回 JSON 或通用 KV。

## 数据库改动

从 v40 升至 v41；保留已有表及列，不删除或覆盖旧原生历史。

| 新表 | 字段 | 原因 |
| --- | --- | --- |
| `acp_provider_thread` | `session_id`, `provider_key`, `agent`, `profile_id`, `agent_definition_id`, `history_session_id`, `covered_segment` | 每个 provider 身份各自续接，按身份隔离上下文覆盖水位 |
| `acp_provider_segment` | `session_id`, `position`, `provider_key`, `history_session_id`, `first_turn`, `end_turn` | 记录混合对话的原生历史分段引用；原文仍只在 provider 存档，按分段组合可见历史 |
| `acp_context_handoff` | `session_id`, `delivery_id`, `provider_key`, `history_session_id`, `through_segment`, `status` | 记录交接准备、受理及结果未知，防止错误推进覆盖水位或自动重复投递 |

所有 `session_id` 引用 `acp_detail(session_id)` 并随对话删除。segment 的 `provider_key`
引用本 session 下的 provider thread；旧 segment 同时保留原生 ID，避免恢复失败后新建
原生会话导致引用指向新存档。轮次范围为半开区间；不采用随适配器分块变化的 UI entry 下标。
所有位置和覆盖范围非负，表中不存凭据、隐藏思考或整包 JSON。

旧对话第一次切换前才登记 provider thread 和首段；没有切换过的对话继续现有恢复路径。
`acp_detail` 仍是当前活动 provider 的权威入口，与 provider 映射更新在同一事务内完成。
原生历史缺失或分段无法可靠定位时显式失败，不把空历史当成完整上下文。

## 修改范围

1. 对话插件：切换入口、目标选择、当前 provider 呈现、错误反馈；不把功能写进 Workspace。
2. 共享协议与 runtime client：有前置身份的切换请求、结果；本机和远程走同一路径。
3. smeltd：串行化切换与输入，验证目标配置、会话空闲及工具限制，确认退出后拉起目标。
   历史 ID 仅从目标身份读取；旧客户端 attach 不允许把运行时切回旧 provider。
4. smelt-store：上述三表、类型化读写、v40→v41 原子迁移及旧数据保留测试。
5. 历史与上下文：按原生轮次引用组合历史，首次完整交接、返回增量交接，准备和确认分开。

运行中回合、排队输入、未处理审批/提问、压缩、后台任务或子智能体未完成时拒绝切换。
自动化和具有额外路由绑定的受管会话先拒绝切换，避免绕过工具限制或改变任务的执行身份。
失败保留旧 provider 引用及历史，不能把旧 provider 的配置、会话 ID 或凭据注入目标。

## 验证

- 假运行时 A→B→A：稳定 session ID / cwd，首次不传 A 的 ID 给 B，返回只恢复 A。
- 数据库重开、daemon 重建、客户端重新连接后的身份和分段顺序。
- 忙碌、排队、审批、后台工作以及两个客户端竞争切换的拒绝路径。
- 目标启动/恢复失败、未知交接结果、旧进程未退出；不自动重投用户 prompt。
- 完整与增量交接、超预算省略、隐藏思考排除；历史缺失不能静默成功。
- 只串行运行受影响 crate 的 check/test/clippy（`-j 2`）；按仓库要求仅检查改动文件格式。

## 验证记录

使用 `-j 2` 串行运行涉及 crate 的测试；共 2,247 项通过，另单独运行 1 项离屏截图测试。

| crate | 通过 | 默认忽略 |
| --- | ---: | ---: |
| `smelt-core`（含集成测试） | 878 | 5 |
| `smelt-model` | 324 | 0 |
| `smelt-runtime-client` | 61 | 0 |
| `smelt-store` | 90 | 0 |
| `smelt-conversation` | 203 | 2 |
| `smeltd`（含 smelt-notify） | 691 | 1 |

本地 IPC 和系统证书测试在沙箱外重跑通过。深浅两套混合历史截图已检查；crate 依赖检查通过。
格式仅检查改动文件，不格式化整仓；Clippy 保留仓库既有警告，不新增警告。

### 真实 provider 验收（2026-11-23）

使用独立 `SMELT_HOME`、全新临时 SQLite 和空工作目录；未打开或迁移用户主库。
运行当前分支的 debug `smeltd` 和用仓库钉死 Bun 构建的 `smelt-agent`。
A 为 Codex CLI 0.171.0 / `gpt-6.1-sol`，B 为 Pi / `zai-coding-cn/glm-5.3`。
本轮未调用工具、读写项目文件或自动批准权限请求。

| 步骤 | 结果 |
| --- | --- |
| A 记住随机 alpha，切至 B | B 的请求不提供 alpha 值，仍准确返回 alpha，并记住新增 beta |
| B 活跃时正常停止并重启 smeltd | 以旧 Codex 启动信息重连仍恢复 B；B 原生 ID、可见历史不变，准确回忆 alpha/beta |
| B→A | sid/cwd 不变，恢复最初 A 原生 ID；只交接 Pi 来源的遗漏段，A 准确返回新增 beta |
| A 活跃时再次重启 smeltd | A 原生 ID、可见历史不变，准确回忆 alpha/beta |
| 原生存档和 SQLite 只读核对 | 两个独立原生 ID、两个有序引用段、两次 delivered 交接；完整性和外键检查通过 |

最终路径共五轮真实模型请求。原始请求、响应、推送、原生存档副本和关系表快照保留在本地
临时验收目录，不上传凭据或用户存档。两次重启前后分别比较用户/助手文本和 provider 分隔顺序，
没有把交接封装或 AGENTS 指令显示成用户消息。原生存档保留三个 A 用户轮次、两个 B 用户轮次。

真实数据发现并补上两项回归：Codex 的 `# AGENTS.md instructions` 封装不能计作用户轮次；
带 `<smelt_context_handoff>` 的真实用户请求不能被旧的尖括号过滤规则删掉。
两项均先复现失败，再修复并重跑 core（878 项）及 smeltd（691 项）测试、Clippy。

Claude Code 2.1.330 探测返回未登录，实际请求报 `Authentication required`，未将其计为通过；
未修改用户认证。初次用系统 Bun 1.3.14 编译 Pi 的启动失败也单独保留，改用仓库钉死版本后通过。
附加观察：`session.create` 指定的标题在最终目录 `custom_title` 中未保留；标题元数据不属于本轮
上下文、原生 ID 和重启恢复的通过项，未据此宣称改名行为已验收。

## 升级、兼容与回退

- 新版自动迁移 v40 旧库到 v41，原有表、列和数据保留；迁移前用 SQLite
  `VACUUM INTO` 生成同目录 `smelt.sqlite3.bak-v40`，建表和版本更新在同一事务内。
- 旧版 v40 程序不能直接读写 v41 库：它既检查版本号，也核对完整 schema。
  不要只改回 `user_version`。旧版的目录更新会删除并重建会话行，绕过检查可能经外键
  级联清掉新增的 provider 关联，所以这也不是安全的写兼容方式。
- 回退须先关闭所有数据库写入方，保留当前数据库及其 WAL/SHM，再用 v40 备份恢复主库。
  备份不能与 v41 的 WAL/SHM 混用。恢复后使用旧版程序；这会丢失备份之后的 SQLite 变更。
- 备份只覆盖 SQLite，工作区文件和各 provider 自己的历史文件不会随之回退。
- 本功能的测试使用临时库，不打开或迁移开发者的真实主库。

## 当前能力边界

- 支持已有原生历史读取器的 provider；通用自定义 ACP 没有历史读取器时明确拒绝。
- 混合历史中的已结束段展示可共享的用户/助手文本。原始工具明细、隐藏思考与旧图片不作为
  跨 provider 导入内容；原生文件仍保持不变。交接有 24,000 字符上限，优先首条目标和近期
  完整消息，明确报告被省略的条目，不把半条历史指令截出来当成完整上下文。
- 切换本身不发模型请求；下一条用户消息才带交接包。受理确认前记录投递未知；只有明确
  收到 provider 回应才推进覆盖位置。重启后可以再次带上历史上下文，但不会自动重发旧用户请求。
- 历史引用依赖原生存档存在。存档丢失、被外部截短或无法定位时失败，不静默当作空对话。
- 混合对话暂不提供原生回退、重试回退和手动压缩，以免改变已引用的原生历史区间。
  自动化、额外输入路由或工具限制绑定的受管对话拒绝切换。
