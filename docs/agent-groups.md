# 异构多智能体群组会话（Agent Group Chat）架构设计

状态：**设计已定，未开始实现**（采用独立运行时库 `~/.smelt/groups.sqlite3`，不动 `smelt.sqlite3` v42；落地前须将 `groups.sqlite3` 登记进 `smelt_paths::DECLARED_ENTRIES` 并确认第 3 节 5 张表清单）。

> **目标**：让 Smelt 托管的全部异构智能体——内置 **Pi**、产品智能体（`agent:<id>`）、**Codex**、**Claude Code**、**Antigravity（`agy`）**、OpenCode / Cursor / Copilot 及任意自定义 ACP Profile（含跨机器远程节点上的 Agent，且支持选择远程常驻机器作为群主节点）——能够与人类一起组成**一等公民的群组会话**，既能高质量并行协作写代码、做跨模型方案辩论与代码评审，又彻底杜绝自由广播群聊的 token 爆炸、从众效应、代码写冲突、静默提权、跨机越权与死循环失控。

---

## 1 · 核心架构原则与 Rust / JS 边界

严格遵循 Smelt 的架构红线（[`AGENTS.md`](../AGENTS.md)、[`app-plugins.md`](app-plugins.md) 与 [`local-storage.md`](local-storage.md)）：

### 1.1 为什么“运行时引擎”必须在 `smeltd`，“功能与界面”必须在 JS 插件？

按「三问」逐项判定：

| 模块 | 归属 | 理由 |
| :--- | :--- | :--- |
| **群消息权威落库、路由扇出、增量游标、盲评栅栏、跳数/乒乓熔断、权限水位线、MCP/CLI 工具鉴权、双层 Worktree 与文件锁、回合结束自动桥接、跨机器双向复用流与零信任校验** | **Rust（`smeltd` + `smelt-store` + `smelt-model`）** | 1) 桌面、手机（Flutter）、CLI、远程节点、外部 Agent 进程全部共用；2) 依赖进程树鉴权、ACP `mcpServers` 注入、挂起会话唤醒、`ApprovalPosture` 权限校验，JS 做不对；3) 桌面窗口关闭后，常驻节点的后台多 Agent 接力与手机端操控仍须完整运转。 |
| **建群向导、群聊时间线、成员状态条、`@mention` 补全输入、盲评并排对比卡、共享任务板视图、Worktree Diff/合并面板、右侧私有会话钻入抽屉、顶栏与左侧活跃群组视图、右键菜单入口** | **JS/TSX 插件（`plugins/groups/`）** | 纯业务交互、文案、布局与视觉表达。复用共享模块 `"smelt/conversation"`（`<SmeltConversation>`）实现私有会话钻入。**删掉 `plugins/groups/`，桌面宿主 `crates/smelt/src/` 与 `smeltd` 不改一行代码。** |

### 1.2 总体分层架构

```mermaid
flowchart TB
    subgraph Clients["多端界面与入口（薄展示层，不存权威副本）"]
        Desktop["桌面 JS 插件 plugins/groups/\n(三栏群视图 + 任务板 + <SmeltConversation> 钻入)"]
        Mobile["Flutter 手机端\n(群聊 / 盲评 / 审批 / 暂停)"]
        CLI["smelt group CLI\n(脚本 / 无 MCP Agent 兜底)"]
    end

    subgraph Daemon["Host Node smeltd 运行时引擎（群单一权威，本机或远程常驻节点）"]
        API["Control API v3 (group.*) & 独立事件话题 (group.changed / group.message_appended)"]
        Engine["GroupRuntime\n· @mention 解析 & 模式路由\n· Delta Cursor + Delta Coalescing\n· Blind Round 栅栏\n· Chain Hop & 乒乓熔断\n· ApprovalPosture 放行卡\n· Shared Task Board & File Lease"]
        Store[("~/.smelt/groups.sqlite3\n(独立运行时库：4 张核心表 + 1 张跨机白名单表)")]
        WT["Worktree Manager\n(复用 worktree_setup / workset_control)"]
        CrossNode["peer_nodes (iroh)\nop=node_group_link / node_group_attach"]
    end

    subgraph Transports["节点本地全引擎统一接入层"]
        PiSock["Pi 原生 RPC\n(SMELT_BG_SOCK)"]
        MCP["smeltd --peer-mcp\n(ACP mcpServers 注入，动态暴露群工具)"]
        CLIFallback["smelt group CLI\n(peer_pid 进程树鉴权兜底)"]
        AutoBridge["TurnEnded 自动桥接\n(防第三方引擎忘调群工具)"]
    end

    subgraph LocalAgents["群主节点隔离成员会话"]
        Pi["Pi / 产品智能体"]
        Claude["Claude Code"]
        Codex["Codex"]
        Agy["Antigravity (agy)"]
        Custom["自定义 ACP"]
    end

    subgraph RemoteNode["Remote Node smeltd（如 Linux GPU 机 / 办公机 / 反向附着的笔记本）"]
        RemoteAttach["RemoteGroupAttachment\n(纯内存绑定 + remote_group_member_grant 白名单)"]
        RemoteMCP["远端 --peer-mcp / Pi RPC / CLI\n(调用经双向复用流回传 Host)"]
        RemoteAgents["远端成员会话 (Codex / Pi / Claude / Custom)\n+ 远端本地 Worktree / 已发布产物读取"]
    end

    Desktop <--> API
    Desktop -. "op=forward 直连看远端私有抽屉" .-> RemoteAgents
    Mobile <--> API
    CLI <--> API
    API <--> Engine
    Engine <--> Store
    Engine <--> WT
    Engine <--> CrossNode

    Engine <--> PiSock <--> Pi
    Engine <--> MCP <--> Claude
    Engine <--> MCP <--> Codex
    Engine <--> MCP <--> Agy
    Engine <--> CLIFallback <--> Custom
    AutoBridge -.-> Engine

    CrossNode <== "QUIC 对称建链·双向复用\n(兼容 inbound=0 / DIAL_ONLY 与零信任隔离)" ==> RemoteAttach
    RemoteAttach <--> RemoteMCP <--> RemoteAgents
```

---

## 2 · 核心领域模型与协作范式

### 2.1 三种群协作模式（可按需实时切换）

群组是一等公民实体，不把单一模式写死，支持 3 种互补且无冲突的协作模式：

1. **`mention`（默认 · `@mention` 点名制）**
   - **机制**：任何成员默认休眠（零轮询、零 token）；只有消息显式包含 `@alias`（或 `@all`）时，才唤醒被点名成员。用户不 `@` 任何人直接发消息时，**仅写入群消息流**（供后续被点名成员按增量游标一并读到），同时消息下方提供一键补点名芯片（`@成员` / `@all` / `/blind`），零意外消耗。
   - **适合**：日常人机混排讨论、按需点名不同强项的模型（例如 `@claude` 做架构分析、`@codex` 写核心算法、`@agy` 查 UI/代码库）。
2. **`coordinator`（群主调度制 · Hierarchical Team）**
   - **机制**：指定一名成员为 `coordinator`（如 Pi 架构师或 Claude）。
     - 人类在群里直接发言（未 `@` 特定人）→ 默认只唤醒 `coordinator`；
     - `coordinator` 拆解任务写进共享任务板，并通过 `@worker` 派单；
     - Worker 完成工作后默认只回报 `coordinator`（不随意横向互相 `@`，除非群设置允许横向直连）。
   - **适合**：大型功能开发、重构、多步骤复杂交付，彻底消除多人横向刷屏。
3. **`blind_round`（并发盲评 / 独立辩论轮次 · 反从众机制）**
   - **解决痛点**：传统群聊中，第一个发言的 Agent 会强烈锚定并带偏后续所有 Agent（Groupthink / 从众效应），导致跨模型代码审查和架构评审价值大打折扣。
   - **机制**：
     1. 人类（如输入 `/blind @claude @codex @agy 评审这段鉴权设计`）或 Coordinator 开启一个 `BlindRound`，指定参与成员集合 $P$ 与出题内容；
     2. 宿主**并发**将题目投递给 $P$ 中所有成员；
     3. 在**栅栏未打开前**，成员提交的 `blind_submission` 仅对人类可见，**对 $P$ 中其他成员严格不可见**（`group_read` 与增量打包都会硬过滤未揭晓轮次的提交）；
     4. 当 $P$ 中全部成员交卷（或达到超时 / 人类点击「立即揭晓」），栅栏原子打开：所有盲评答卷按固定顺序一次性公开到群时间线，UI 渲染为**并排对比视图**；
     5. 人类或 Coordinator 可紧接着发起第二轮「交叉质证」（`cross_examine`），让各方互相指出盲点并收敛出 `decision` 结论卡。

### 2.2 成员隔离与生命周期（懒启动 + 归属区分 + 独立游标）

- **底层依然是普通 ACP/Pi 会话**：每个群成员关联一个 `session_id`。成员自身的工具调用、终端输出、长文件阅读全部留在私有会话里，**绝不污染主群上下文**。
- **区分两类归属（`ownership`）**：
  - `group_owned`（群内新建成员）：封装在群内部，不刷屏主侧栏 `session-list`，在群顶栏成员轨和右侧抽屉按需查看，随群归档一并清理；任意成员进入运行中/待审批/报错状态时，角标直接冒泡汇总到群行。
  - `linked`（从外部拉入的已有单聊会话）：保留它原本在主侧栏的位置并挂群角标，群归档或移除成员时不删除原单聊会话。
- **懒启动 + 单一权威绑定（Dormant Session）**：成员加入群时，直接绑定已有 `session_id`，或在对应节点创建一个 **dormant 的普通 `session` 行**（未唤醒时不启子进程、零内存占用）。因此 `cwd`、引擎类型、`profile_id`、绑定工作树（`workset_id`）100% 沿用 Smelt 现有的 `session` + `acp_detail` + `workset` 为唯一权威，**群表绝不镜像重复列**。闲置超时后仍按现有 `acp_idle` 自动挂起，下次被 `@` 时事件驱动唤醒。
- **结构化成员名册**：
  - `alias`：群内唯一短名（校验 `^[a-z0-9][a-z0-9_-]{0,23}$`，禁用保留词 `all`、`user`、`system`、`coordinator`、`everyone`）。
  - `role_kind`：`coordinator | worker | reviewer | observer`。
  - `role_title` / `role_prompt`：简短职责与群内行为准则（首次唤醒时随入群 Briefing 注入）。
  - `workspace_mode`：`isolated_worktree | shared_readonly | shared_leased`（仅声明协作策略，实际路径与分支直接读 `session.cwd` 与 `workset`）。
  - `runtime_id`：`NULL` 表示群主本机，非空表示经 `iroh` 配对的远程机器节点。

---

## 3 · SQLite 持久化设计（独立运行时库 `~/.smelt/groups.sqlite3` · 5 张极简表）

严格遵守 [`local-storage.md`](local-storage.md) 的单一权威与按字段建表原则，并吸收 `@earendil-works/pi-durable` 的 **Commit-before-Publish** 与启动 **`resume()` 自愈**机制：

### 3.1 为什么用独立运行时库 `~/.smelt/groups.sqlite3`（而非塞进 `smelt.sqlite3`）？

- **归属仍是 `smeltd` 独占写入**（WAL、5 秒 busy timeout、Unix `0600` 权限，在 `smelt_paths::DECLARED_ENTRIES` 登记，独立版本号 `GROUP_STORE_SCHEMA_VERSION = 1`），不是桌面插件库（确保桌面关闭、手机直连、远程服务器当群主时仍完全可用）。
- **收益**：
  1. **Schema 零污染**：群组作为新模块迭代升级时，不牵连已有 43 张表的核心 `smelt.sqlite3`（`STORE_SCHEMA_VERSION = 42` 保持不动）；
  2. **IO 与体积隔离**：群消息是高频追加型长文本日志，拆库后不会拖慢工作区目录读写，也便于日后独立归档裁剪、`VACUUM` 或导出；
  3. **跨库引用代价极低**：跨机器成员的 `session_id` 本来就在远端节点、无法建 SQLite 物理外键；本机 `workset_id` / `session_id` 采用与现有 `automation_run.session_id` 一致的松耦合引用，创建成员时先幂等建 dormant `session` 再写群表，删除/失效时优雅降级。
- 桌面插件 `plugins/groups/` 的 `plugin-data/smelt.groups/store.sqlite3` **只存纯本地 UI 状态**（草稿、抽屉宽度、群折叠分组），绝不存群消息或成员副本。

### 3.2 5 张精简表 DDL（本机核心 4 张 + 跨机零信任 1 张）

> **裁剪说明**：
> - Worktree / 分支 / `cwd` / 引擎全部单一权威读取既有 `workset`、`session`、`acp_detail`，零冗余列；
> - 盲评轮次直接复用 `agent_group_message`（`blind_revealed_at_ms`）+ `agent_group_message_mention`（`mention_kind='blind_invite'`），省掉 2 张独立盲评表；
> - 群内任务复用 `kind='task'` 消息 + `task_status`（天然带上下文和指派链），省掉独立任务表；
> - 产物引用按消息整块读写、不做局部查询，存 `attachments_json BLOB`（同 `automation.trigger_json` 例外），省掉独立产物表；
> - 短期文件租约（`file_lease`，TTL 10 分钟）与跳数链/乒乓计数均属运行时瞬态状态，放 `smeltd` 内存，不落 SQLite。

```sql
-- 1. 群组主表
CREATE TABLE agent_group (
    id                      TEXT PRIMARY KEY,          -- 'grp-' + 12位hex
    title                   TEXT NOT NULL,
    description             TEXT NOT NULL DEFAULT '',
    workset_id              TEXT,                      -- 可选：绑定既有 workset（复用其项目根/集成分支/worktree，不重复存路径与分支）
    mode                    TEXT NOT NULL DEFAULT 'mention' CHECK (mode IN ('mention', 'coordinator')),
    coordinator_member_id   TEXT,                      -- mode='coordinator' 时的群主 member_id
    allow_worker_cross_talk INTEGER NOT NULL DEFAULT 1 CHECK (allow_worker_cross_talk IN (0, 1)),
    max_chain_hops          INTEGER NOT NULL DEFAULT 10,
    max_chain_depth         INTEGER NOT NULL DEFAULT 6,
    max_pair_ping_pong      INTEGER NOT NULL DEFAULT 3,
    auto_bridge_mentions    INTEGER NOT NULL DEFAULT 0 CHECK (auto_bridge_mentions IN (0, 1)),
    paused                  INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
    next_seq                INTEGER NOT NULL DEFAULT 1,-- 群内严格单调自增消息序号
    archived_at_ms          INTEGER,
    created_at_ms           INTEGER NOT NULL,
    updated_at_ms           INTEGER NOT NULL
);

-- 2. 群成员表（每个成员直接绑定已有或新建的 dormant session_id；cwd/引擎/worktree 均以 session+acp_detail+workset 为单一权威）
CREATE TABLE agent_group_member (
    id                       TEXT PRIMARY KEY,          -- 'gm-' + 12位hex
    group_id                 TEXT NOT NULL REFERENCES agent_group(id) ON DELETE CASCADE,
    alias                    TEXT NOT NULL,             -- 群内 @ 名，同群唯一
    display_name             TEXT NOT NULL,
    ownership                TEXT NOT NULL DEFAULT 'group_owned' CHECK (ownership IN ('group_owned', 'linked')),
                                                        -- group_owned=群新建封装成员(不占主侧栏,随群归档); linked=外部拉入的已有会话(保留在主侧栏并挂群角标)
    runtime_id               TEXT,                      -- NULL=群主本机，非空=peer_node.runtime_id（远程节点）
    session_id               TEXT NOT NULL,             -- 目标节点上的会话 id（入群即绑定已有会话或建 dormant session）
    role_kind                TEXT NOT NULL DEFAULT 'worker' CHECK (role_kind IN ('coordinator', 'worker', 'reviewer', 'observer')),
    role_title               TEXT NOT NULL DEFAULT '',
    role_prompt              TEXT NOT NULL DEFAULT '',
    workspace_mode           TEXT NOT NULL DEFAULT 'shared_readonly' CHECK (workspace_mode IN ('isolated_worktree', 'shared_readonly', 'shared_leased')),
    tool_restriction_profile TEXT NOT NULL DEFAULT 'default', -- 'default' | 'readonly' | 'no_git_push'
    briefing_sent            INTEGER NOT NULL DEFAULT 0 CHECK (briefing_sent IN (0, 1)),
    last_delivered_seq       INTEGER NOT NULL DEFAULT 0,-- 已投递给该成员的最大群消息 seq
    last_read_seq            INTEGER NOT NULL DEFAULT 0,-- 该成员已确认消费的最大群消息 seq
    muted                    INTEGER NOT NULL DEFAULT 0 CHECK (muted IN (0, 1)),
    removed_at_ms            INTEGER,
    created_at_ms            INTEGER NOT NULL,
    UNIQUE(group_id, alias)
);
CREATE INDEX idx_agm_session ON agent_group_member(runtime_id, session_id);

-- 3. 群消息时间线表（统一承载普通对话、任务、决策钉选、盲评出题/交卷与附件引用）
CREATE TABLE agent_group_message (
    id                   TEXT PRIMARY KEY,          -- 'gmsg-' + 12位hex
    group_id             TEXT NOT NULL REFERENCES agent_group(id) ON DELETE CASCADE,
    seq                  INTEGER NOT NULL,          -- 群内严格单调序号 1, 2, 3...
    sender_type          TEXT NOT NULL CHECK (sender_type IN ('user', 'member', 'system')),
    sender_member_id     TEXT REFERENCES agent_group_member(id) ON DELETE SET NULL,
    kind                 TEXT NOT NULL DEFAULT 'chat' CHECK (kind IN ('chat', 'task', 'decision', 'blind_prompt', 'blind_submission', 'system')),
    content              TEXT NOT NULL,
    attachments_json     BLOB NOT NULL DEFAULT X'5B5D', -- JSON 数组：[{id, runtime_id, kind, label, ref_path, start_line, end_line, commit, session_id}]
    reply_to_seq         INTEGER,                   -- 盲评交卷时指向 blind_prompt 的 seq
    thread_root_seq      INTEGER,                   -- 子话题根消息 seq；NULL 表示主频道
    blind_deadline_at_ms INTEGER,                   -- 仅 blind_prompt：可选自动揭晓截止时间
    blind_revealed_at_ms INTEGER,                   -- 盲评栅栏：NULL=未揭晓（对其他参与成员硬隐藏），非空=已原子揭晓
    task_status          TEXT CHECK (task_status IS NULL OR task_status IN ('open', 'in_progress', 'done', 'blocked')),
    chain_id             TEXT,                      -- 所属用户触发链
    chain_depth          INTEGER NOT NULL DEFAULT 0,
    auto_bridged         INTEGER NOT NULL DEFAULT 0 CHECK (auto_bridged IN (0, 1)),
    pinned               INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    idempotency_key      TEXT,                      -- 防重放幂等键
    created_at_ms        INTEGER NOT NULL,
    UNIQUE(group_id, seq),
    UNIQUE(group_id, idempotency_key)
);
CREATE INDEX idx_agmsg_thread ON agent_group_message(group_id, thread_root_seq, seq);
CREATE INDEX idx_agmsg_blind ON agent_group_message(group_id, reply_to_seq) WHERE kind = 'blind_submission';
CREATE INDEX idx_agmsg_open_tasks ON agent_group_message(group_id, task_status) WHERE kind = 'task';

-- 4. 结构化 @mention 与持久化投递/等待状态表（统一承载扇出、盲评参与进度、权限放行卡 Signal、离线补发）
CREATE TABLE agent_group_message_mention (
    message_id       TEXT NOT NULL REFERENCES agent_group_message(id) ON DELETE CASCADE,
    group_id         TEXT NOT NULL,
    target_member_id TEXT NOT NULL REFERENCES agent_group_member(id) ON DELETE CASCADE,
    mention_kind     TEXT NOT NULL DEFAULT 'direct' CHECK (mention_kind IN ('direct', 'all', 'coordinator_default', 'blind_invite')),
    delivery_status  TEXT NOT NULL DEFAULT 'queued' CHECK (delivery_status IN ('queued', 'offline_pending', 'pending_approval', 'delivered', 'submitted', 'coalesced', 'skipped', 'rejected', 'failed')),
    delivery_id      TEXT,                      -- 幂等投递 id（重启 resume 对账绝不重投）
    status_reason    TEXT,                      -- 如 'permission_escalation' | 'hops_exhausted' | 'node_offline' | 'agent_access_denied' | 'muted'
    updated_at_ms    INTEGER NOT NULL,
    PRIMARY KEY (message_id, target_member_id)
);
CREATE INDEX idx_agmm_pending ON agent_group_message_mention(group_id, delivery_status)
    WHERE delivery_status IN ('queued', 'offline_pending', 'pending_approval');

-- 5. 远端群主成员本地授权表（零信任硬边界，仅存本机授权白名单，不存群消息副本）
-- 场景：当远程服务器 B 被选为群主、本机 A（inbound=0 / DIAL_ONLY）有成员加入 B 的群时：
-- 1) 哪些本机 session 允许接听 B 的群消息，只能由 A 本机人类显式写入此表，B 绝对无权自行拉起或改写；
-- 2) A 重启后依据此表主动向 B 拨出 op=node_group_attach，且严格按表内白名单校验 B 发来的每一帧（沙箱路径直接读 session.cwd）。
CREATE TABLE remote_group_member_grant (
    host_runtime_id          TEXT NOT NULL,             -- 群主节点的 peer_node.runtime_id
    group_id                 TEXT NOT NULL,
    member_id                TEXT NOT NULL,             -- 群内成员 id
    local_session_id         TEXT NOT NULL,             -- 仅允许投递到这一精确的本机 session（cwd 从本机 session 表单一权威读取）
    allow_remote_full_access INTEGER NOT NULL DEFAULT 0 CHECK (allow_remote_full_access IN (0, 1)),
                                                        -- 由 A 本机人类自主选择并可中途切换；远端 B 无权修改
    created_at_ms            INTEGER NOT NULL,
    PRIMARY KEY (host_runtime_id, group_id, member_id)
);
```

### 3.3 持久化与崩溃自愈语义（借鉴 `@earendil-works/pi-durable`）

> 调研说明：`@earendil-works/pi-durable` 是面向单写者 TypeScript/`pi-ai` 的实验性持久 Harness，不托管外部 ACP 子进程（Codex/Claude/AGY）且无跨机多节点支持，因此**不引入为群组总线依赖**（未来稳定后可独立评估用于升级 `agent/` 的 Pi 引擎本身）；但直接吸收其三项核心可靠性设计进 Rust `GroupRuntime`：

1. **Commit-before-Publish（先原子提交，再推事件与投递）**：每次 `group_post` / 盲评开题 / 盲评揭晓 / 审批放行，都先在一个 `groups.sqlite3` 事务内原子落盘（消息 + `mention` 行 + `next_seq` 递增），事务提交成功后再发布 `group.message_appended` 与唤醒成员，彻底杜绝幻影消息或崩溃状态撕裂。
2. **启动自愈 `resume()` + 幂等 `delivery_id`**：`smeltd` 重启时扫描 `idx_agmm_pending`（`queued` / `offline_pending`）以及未揭晓且未过期的 `blind_prompt`，自动重建待投递队列与截止提醒，依靠 `idempotency_key` 与 `delivery_id` 保证重启后绝不重复投递。
3. **统一的持久化等待原语（`allSettled` + `Signal`）**：盲评全员交卷栅栏（`mention_kind='blind_invite'` 全部进入 `submitted`/`failed`/`skipped` 或到期）与权限放行卡（`delivery_status='pending_approval'` 等人类点击信号）全部以 `agent_group_message_mention` 为持久化真源，不依赖易失内存回调。

---

## 4 · 全引擎工具协议与通信链路（零死角兼容）

依托 [`cross-agent-messaging.md`](cross-agent-messaging.md) 已经跑通的三条通道，将群组工具无感注入所有引擎，**绝不污染非群聊会话**：

### 4.1 发起侧三通道 + 动态工具暴露

1. **动态暴露原则**：`smeltd --peer-mcp` 在处理 `tools/list`（以及 Pi 扩展注册工具）时，先向 `PeerSocket` 查询当前 `session_id` 是否属于至少一个活跃群组：
   - **普通单聊会话**：不暴露任何 `group_*` 工具，零 token 上下文负担；
   - **群成员会话**：暴露下方 9 个群工具（且当一个会话中途被拉进群时，通过 MCP `notifications/tools/list_changed` 或在首次 `<group-message>` 信封里提示，立即生效）。
2. **三条通道完全对齐**：
   - **Pi**：`SMELT_BG_SOCK` 原生工具扩展（与现有 `session-peers.ts` 同构）。
   - **Codex / Claude Code / Antigravity (`agy`) / OpenCode / 支持 MCP 的自定义 ACP**：复用 `smeltd --peer-mcp` stdio 桥（继续保持 `-32601 Method not found` 快速响应 `agy` 的 `server/discover`）。
   - **不支持 MCP 的自定义 ACP**：新增 `smelt group` CLI 子命令，复用 `memory_peer_auth.rs` + `acp_terminal.rs` 的 `peer_pid` / 进程组溯源鉴权——**来源 `member_id` 永远由宿主按连接/进程树认定，绝不信任调用参数里的身份字段**。

### 4.2 群工具规范（9 个正交、极简工具）

| 工具名 | 关键参数 | 作用与设计要点 |
| :--- | :--- | :--- |
| `group_post` | `group_id, content, mentions?: string[], kind?: "chat"\|"task"\|"decision", reply_to_seq?, thread_root_seq?, artifacts?` | 往群里发消息。支持直接传 `mentions` 数组，同时宿主也会解析正文中的 `@alias` 取并集。支持 `idempotency_key` 防重复发。 |
| `group_yield` | `group_id, reason?` | **显式静默/让渡发言权**。标记本轮已处理完毕且无需往群里灌水（推进 `last_read_seq`、标记 `posted_or_yielded = true`、阻止自动桥接），彻底根治客套死循环。 |
| `group_read` | `group_id, after_seq?, before_seq?, limit?, thread_root_seq?` | 按游标分页翻阅历史或某条子线程；硬过滤未揭晓的盲评答卷。 |
| `group_members` | `group_id` | 返回名册：`alias`、引擎类型、`role_kind`、`role_title`、实时状态（`idle`/`running`/`waiting_approval`/`offline`）、以及从 `session`/`workset` 实时读取的 `cwd` 与当前分支。 |
| `group_blind_submit` | `group_id, prompt_seq, content, artifacts?` | 向进行中的盲评出题消息交卷；栅栏未开前对其他参与者不可见；全员交齐时原子揭晓。 |
| `group_task_update` | `group_id, seq, task_status: "open"\|"in_progress"\|"done"\|"blocked"` | 更新 `kind='task'` 群消息的任务状态（新建任务直接走 `group_post(kind="task")`），免去维护第二套任务实体。 |
| `group_lease_acquire` / `group_lease_release` | `group_id, path_globs, reason, ttl_sec?` | `shared_leased` 模式下的顾问式文件锁；若与未过期租约路径相交，直接返回冲突详情（持有者 `@alias`、原因、过期时间）。 |
| `group_artifact_fetch` | `group_id, artifact_id` | 按需拉取其他成员引用的 diff / 文件片段 / 私有会话摘要（含跨机器拉取），避免把几千行 diff 塞进每个人的群消息流。 |
| `group_worktree_merge` | `group_id, source_alias, target?: "integration"\|"base", strategy?: "merge"\|"squash"\|"cherry_pick"` | 将写手成员的 worktree 分支合并到群级集成分支或目标分支（遵循成员自身权限姿态，不人为硬锁合并权）。 |

---

## 5 · 增量投递、增量合并与自动兜底引擎

### 5.1 增量打包 + 增量合并（Delta Bundling & Delta Coalescing）

假设 `@codex` 正在忙着跑测试，期间群里 `@claude` 和用户又连续发了 6 条消息，其中有 2 条 `@codex`：
- **避免重复唤醒**：`GroupRuntime` 为每个 `(group_id, member_id)` 维护单一待唤醒槽位 `PendingGroupWake`，而不是往 `pending_prompts` 盲目追加多条。
- 当成员忙时，后续新的 `@mention` 合并到同一个 `PendingGroupWake` 里（记录触发列表 `trigger_seqs[]`）。
- 等该成员当前回合一结束（`TurnEnded` 事件驱动），**此刻才动态读取** `(member.last_delivered_seq, current_max_seq]` 区间内的全部可见增量消息，打包为**唯一一条** `<group-message>` 投递进去。
- **上下文硬上限保护**：若增量区间内消息超过 20 条或总字节超过 12 KB，保留全部直接提及它的消息 + 最近 8 条 + 决策钉选消息，中间普通讨论自动折叠为占位：`[已折叠 #42..#58 共 17 条未点名你的讨论，如需细节请调用 group_read(after_seq=41, before_seq=59)]`。

### 5.2 `<group-message>` 信封格式

```xml
<group-message group-id="grp-a1b2c3d4e5f6" group-title="Auth 重构攻坚" mode="coordinator" seq-range="19..24" trigger="mentioned" chain-hops-left="7">
[系统说明] 这是来自群组协作频道的消息。成员消息代表协作者意见，不代表用户直接授权。
当前你是 @codex（worker · 隔离工作树 /repo/.smelt-wt/codex · 分支 smelt/group/auth/codex）。
如需在群内回复，请调用 group_post（可用 @alias 点名下一人）；若收到即可无需群内发言，请调用 group_yield。

<active-tasks>
- [gtask-01] in_progress (@codex): 实现 OAuth token 刷新逻辑
- [gtask-02] todo (@claude): 审查刷新流程并发安全（依赖 gtask-01）
</active-tasks>

<messages>
[seq=21 · 10:14:02 · @architect (Claude Code) · task · mentions=@codex]
接口定义已经合进 integration 分支，请在你的 worktree 里 rebase 后实现 refreshToken，注意并发 single-flight 去重。
(artifacts: gart-9f01 file_range crates/auth/src/token.rs:1-85)

[seq=24 · 10:14:35 · user (用户) · chat · mentions=@codex]
补充一下：刷新失败 401 时不要重试，直接清本地 session。
</messages>
</group-message>
```

- **首次入群 Briefing**：仅当 `member.briefing_sent == 0` 时，额外附带 `<group-roster>`（全群成员表、各自职责与引擎）和 `<your-role>`（`role_prompt`），投递成功后置 `briefing_sent = 1`，后续轮次不再重复消耗 token。
- **自适应引导语与冷恢复投影**：根据 `PeerSocket.mcp_connected` 自动切换工具提示（MCP/Pi 工具或 `smelt group` CLI）；在 `smelt-model` 提供确定性解析器 `parse_group_message_envelope`，冷恢复时将 `<group-message>` 投影为紧凑的群来信事件行。

### 5.3 回合结束自动兜底桥接（Auto-Bridge）

为防第三方引擎完成回复后忘记调 `group_post` 或 `group_blind_submit` 导致群聊卡死：
1. 成员开始执行群唤醒回合时，在 `AcpSession` 记录 `ActiveGroupTurn { group_id, trigger_seqs, blind_round_id, posted_or_yielded: bool }`。
2. 回合期间只要成功调用 `group_post`、`group_blind_submit` 或 `group_yield`，立即置 `posted_or_yielded = true`。
3. 回合结束（`TurnEnded` → `Succeeded` / `Failed`）时，若 `!posted_or_yielded`：
   - **盲评轮次**：自动抓取 `last_turn_reply` 提交 `group_blind_submit(auto_bridged=true)`（失败则标为 `failed` 并附原因），确保盲评栅栏永不死等；
   - **普通 `@mention` / `task` 唤醒**：自动抓取 `last_turn_reply` 以 `auto_bridged=true` 写入 `agent_group_message` 并推送到群时间线（带微弱的「自动同步」角标）；
   - **防意外风暴**：`auto_bridged=true` 消息正文里出现的 `@alias` **默认不触发二次扇出**（`auto_bridge_mentions == 0`），而是在消息旁提供一键「继续唤醒 `@xxx`」按钮，显式调用 `group_post` 才自动接力。

---

## 6 · 代码写冲突隔离：三级工作区策略与双层分支

多 Coding Agent 同群改同一个代码库时，按成员角色默认分配最优隔离级别，三档共存：

| `workspace_mode` | 适用角色 | 底层实现 | 协作与合并流 |
| :--- | :--- | :--- | :--- |
| **`isolated_worktree`**（写代码成员默认） | Coder / Refactor / Test Writer | 复用 `smeltd` 现有的 `worktree_setup.rs` / `workset_control.rs`，自动创建**双层分支**：群级集成分支 `smelt/group/<slug>/integration` + 各写手独立 worktree `<worktree_root>/<group_slug>/<alias>`（分支 `smelt/group/<group_slug>/<alias>`） | 各自独立编码、编译、跑单测，互不干扰。完工后附上 `worktree_diff` / `git_commit` artifact。**合并权限不人为硬锁**：允许 Coordinator 或有权限的成员通过 `group_worktree_merge` 自由合并到集成分支或目标分支并跑集成测试（遵循成员自身权限姿态），同时人类在 UI「分支变更」面板也能一键预览 diff 并执行 Merge / Squash / Cherry-pick；群归档时统一清理 worktree。 |
| **`shared_readonly`**（评审/架构默认） | Architect / Reviewer / Researcher | 指向主项目目录，同时对支持机械工具限制的引擎（复用 [`plugin-driven-conversations.md`](plugin-driven-conversations.md) 的 `disallowed_tools` 机制）在权限层硬禁止 `Edit`/`Write`/`MultiEdit`/`Bash(git push:*)` | 只读看全仓库（也可跨目录审阅各 worker 的 worktree），零误改风险。 |
| **`shared_leased`**（轻量同目录协作） | 需共享同一工作树的多人小改动 | 共享同一 `cwd`，启用内存态顾问式路径 glob 锁（按 `(group_id, runtime_id, repo_root)` 隔离命名空间，默认 TTL 10 分钟，活跃工具调用期间自动续期，重启自动清空不写库） | 改文件前先 `group_lease_acquire(["crates/auth/**"])`，冲突立刻返回持有者与占用原因，协调交接顺序。 |

> **跨机器异构文件系统说明**：当群内成员分布在多台机器（如 macOS `/Users/me/smelt` 与 Linux `/home/dev/smelt` 或不同仓库）时，**每个成员独立配置所在机器的 `session.cwd` / `workset`**。`isolated_worktree` 在成员各自机器上本地创建；需要把远端成员的改动合回本机仓库（或反向应用）时，除走 Git 远端分支外，宿主提供 `group.worktree.apply_patch` 直接把远端生成的 unified diff / git patch 应用到目标节点分支，不强制依赖公网 Git push 权限。

---

## 7 · 安全门卫、权限水位线、熔断护栏与跨机零信任

### 7.1 同机群内权限水位线（Zero-Trust Peer + 放行卡）

复用 [`cross-agent-messaging.md`](cross-agent-messaging.md) 的三态 `ApprovalPosture`（`RequiresApproval | FullAccess | Unknown`）：

| 触发者 | 目标成员姿态 | 行为 |
| :--- | :--- | :--- |
| **用户（`sender_type = user`）** | 任意 | ✅ 立即唤醒（代表用户直接意图） |
| **`FullAccess` 成员** | 任意 | ✅ 立即唤醒 |
| **`RequiresApproval` / `Unknown` 成员** | `RequiresApproval` | ✅ 立即唤醒（目标自己仍受逐次审批保护） |
| **`RequiresApproval` / `Unknown` 成员** | `FullAccess` 或 `Unknown` | 🛑 **拦截静默提权，生成「待放行卡」**（`delivery_status = 'pending_approval'`）。群时间线出现操作卡：「`@codex` 请求唤醒全权限成员 `@claude`，`[批准本次唤醒]` `[本次链内免问]` `[拒绝]`」。 |

### 7.2 四层防失控熔断机制

1. **链式跳数 + 深度双预算**：用户每发一次消息开启新 `chain_id`（默认 `max_chain_hops = 10`、`max_chain_depth = 6`）。一次 Agent→Agent 扇出 `depth + 1`、`hops_left` 扣减实际唤醒数；耗尽后挂起为 `hops_exhausted` 并弹出续命卡（`[再给 +6 跳继续]` / `[暂停并由我接管]`）。
2. **乒乓死循环检测（Ping-Pong Detector）**：同一 `chain_id` 下任意无序成员对 `{A, B}` 连续互 `@` 超过 `max_pair_ping_pong`（默认 3 个来回）且期间群任务板无状态跃迁、成员 worktree 无新 commit，自动熔断并提示人类裁决。
3. **单会话背压与 Delta Coalescing**：同一成员在忙时不叠加多条排队 prompt，统一合并为下一次唤醒的增量 delta。
4. **人类最高优先级（HITL Interrupt & Pause）**：
   - `group.pause` 一键冻结全部待投递唤醒队列（可附加 `abort_running: true` 立即中止正在跑的成员回合）；
   - 人类插话立刻清零旧链未投递唤醒并刷新 `chain_id` 与跳数；
   - 任意成员进入 `waiting_approval` 经 `attention.changed` 实时冒泡到群行与成员胶囊。

### 7.3 跨机器零信任威胁模型与防攻破加固（假设对端节点已被完全攻破）

QUIC（iroh + TLS 1.3 + Ed25519 `EndpointId` 双向认证）保证了传输层防窃听、防篡改、防中间人冒充；真正要防的是应用层风险——**假设配对的远程机器 `B` 被攻击者拿到 root（篡改了 `B` 的 `smeltd` 或盗取 `iroh-secret`），或 `B` 上的 Agent 遭遇间接提示词注入（Indirect Prompt Injection）**：

1. **前置认知：`inbound = 1` vs `inbound = 0`**
   - 若在 `A` 上手动给 `B` 开了 `inbound = 1`（见 [`cross-machine.md`](cross-machine.md) §2），在 Smelt 中本来就等价于授予 `B` 远程控制本机权限（类似 SSH `authorized_keys`）；绝不应对非完全自控机器开 `inbound = 1`。
   - 默认单向信任边界是 **`inbound = 0`（或本机关闭远程访问 `DIAL_ONLY`）**：`A` 可以操控 `B`，但即便 `B` 被黑客完全接管，也**绝不能反向控住 `A`**。
2. **对 `inbound = 0` 远端群主 `B` 的 5 条不可绕过硬边界（全部在 `A` 本机 `smeltd` 强制校验）**：
   - **禁止远程派生与写操作指令**：在 `op=node_group_attach` 通道上，`A` **彻底拒绝**来自 `B` 的 `spawn_or_wake`（新建会话/建 worktree）、`worktree_merge`、`worktree_apply_patch` 等控制面帧。`A` 上哪个会话加入 `B` 的群，**只能由 `A` 本机人类（`Principal::Local`）先在本机创建并写入 `remote_group_member_grant` 白名单**；任何代码 Patch 落地到 `A` 只能由 `A` 本机用户主动拉取并确认执行。
   - **绝不信任远端声称的 `trigger_posture = User`（支持用户本机自主切换 FullAccess）**：`A` 一律忽略远端发来的 `User` 声称——除非该消息携带 `A` 本机 `smeltd` 自己发出时登记的一次性 `local_user_origin_nonce`（证实确为 `A` 本机人类亲自发出），否则一律按 `A` 本机 `remote_group_member_grant` 裁决：
     - **用户自主选择并可中途随时切换**：用户在 `A` 本机添加成员或会话中途，可像操作普通会话一样自由切换 `allow_remote_full_access`（允许远端唤醒直接触发该本机 `FullAccess` 成员）；该开关**仅限 `A` 本机人类（`Principal::Local`）修改，远端 `B` 绝无权篡改**。
     - 当 `allow_remote_full_access == 0` 且目标本机成员处于 `FullAccess` 时，`A` 本机直接拦截并弹出本机原生确认通知（提供「批准本次」与「允许本成员接受远端免审批唤醒」一键切换）；处于 `RequiresApproval` 或只读受限的本机成员自动接信，敏感工具调用仍由 `A` 本机审批把关。
   - **禁止远端任意路径读文件（Push-Only 发布白名单）**：`B` 永远不能传路径向 `A` 索取文件。`A` 只允许 `B` 拉取 **`A` 本机成员调用 `group_post(artifacts=...)` 时由 `A` 自己登记的 `published_artifact_id`**，且登记时 `A` 本机 `smeltd` 强制执行 `realpath` 沙箱校验（严格落在对应 `local_session_id` 的 `session.cwd` 内、禁符号链接越界、黑名单拦截 `.env` / `.git/config` / 私钥及 `~/.smelt/` 等敏感路径、单次大小上限 256 KB）。
   - **防间接提示词注入 + 本机速率配额**：所有经跨机器通道进入的 `<group-message>` 强制注入不可剥离来源标签（`origin-node="gpu-box (remote)" trust="untrusted-remote-peer"`）；同时 `A` 本机独立维护远端唤醒速率配额，防被攻破的 `B` 狂发消息刷爆 `A` 的模型额度。
   - **本机一键物理断链（Kill Switch）**：`A` 删除 `remote_group_member_grant` 行或撤销对端节点时，立即关闭该 QUIC 群流并拒绝重连，无需远端 `B` 配合即可瞬间切断。

---

## 8 · Control API v3、独立事件话题与跨机器多节点架构

### 8.1 Control API v3（注册于 `define_control_methods!`，不升主版本）

```text
# 群生命周期
group.create               { title, description?, workset_id?, mode?, members: [...] } -> { group }
group.list                 { include_archived? } -> { groups: GroupSummary[] }
group.get                  { group_id } -> { group, members, open_tasks, active_blind_prompts, leases }
group.update               { group_id, title?, mode?, coordinator_member_id?, max_chain_hops?, ... } -> { group }
group.archive              { group_id, cleanup_worktrees? } -> {}
group.delete               { group_id, cleanup_worktrees? } -> {}

# 成员管理
group.member.add           { group_id, alias, ownership?, session_id?, agent_option_id?, cwd?, workset_id?, runtime_id?, role_kind?, role_title?, role_prompt?, workspace_mode?, tool_restriction_profile? } -> { member }
group.member.update        { group_id, member_id, alias?, role_kind?, role_prompt?, muted? } -> { member }
group.member.remove        { group_id, member_id, remove_worktree? } -> {}
group.member.wake          { group_id, member_id } -> { session_id }
group.remote_grant.upsert  { host_runtime_id, group_id, member_id, local_session_id, allow_remote_full_access? } -> {}  # 仅 Principal::Local
group.remote_grant.revoke  { host_runtime_id, group_id, member_id } -> {}  # 仅 Principal::Local，立即断链

# 消息、盲评与调度控制
group.message.list         { group_id, before_seq?, after_seq?, limit?, thread_root_seq? } -> { messages, has_more }
group.message.post         { group_id, content, mentions?, kind?, reply_to_seq?, thread_root_seq?, artifacts?, idempotency_key? } -> { message }
group.mention.approve      { group_id, message_id, target_member_id, allow_chain? } -> {}
group.mention.reject       { group_id, message_id, target_member_id, reason? } -> {}
group.blind.start          { group_id, prompt, participant_member_ids, deadline_sec? } -> { prompt_message }
group.blind.reveal         { group_id, prompt_seq } -> { prompt_seq, submissions }
group.pause                { group_id, abort_running? } -> {}
group.resume               { group_id, extra_hops? } -> {}

# 任务状态、内存文件锁与 Worktree / 跨机器 Patch 辅助
group.task.update          { group_id, seq, task_status } -> { message }
group.lease.release        { group_id, lease_id } -> {}
group.worktree.diff        { group_id, member_id } -> { runtime_id, base_branch, branch, diff_stat, patch }
group.worktree.merge       { group_id, member_id, target?: "integration"|"base", strategy: "merge"|"squash"|"cherry_pick" } -> { commit_sha }
group.worktree.apply_patch { group_id, source_member_id, target_runtime_id?, target_cwd, create_branch? } -> { applied_branch, commit_sha? }
```

### 8.2 独立事件总线话题（遵循 `event-bus-architecture.md` v7 规范）

新增 2 条独立订阅话题（不修改核心订阅握手，旧客户端零影响）：

| 话题 | 所需能力 | 快照类型 | 载荷 |
| :--- | :--- | :--- | :--- |
| `group.changed` | `session.read` | `groups` | 群元数据变更、成员在线/离线/运行/审批/游标状态、暂停态、剩余跳数、任务板与锁变更、盲评状态变更 |
| `group.message_appended` | `session.read` | — | 新增群消息、盲评批量揭晓事件、待审批放行卡状态更新 |

### 8.3 跨机器多节点群组架构（一等公民设计）

完全契合 Smelt 现有的节点模型（[`cross-machine.md`](cross-machine.md) 与 `crates/smeltd/src/peer_nodes.rs`），解决以下 **7 个分布式工程核心问题**：

#### 1) 可选任意远程机器为群主（Selectable Host Node · 星型单主权威）
- **建群时自由指定群主节点（`host_runtime_id`）**：默认本机，也可选择任意已连接的远程常驻服务器（如公司 Linux 开发机 / 云端 GPU 机 `gpu-box`）。桌面/CLI 经 `op=forward(B)` 调用 `B` 上的 `group.create`，群权威数据落在 `B` 的 `~/.smelt/groups.sqlite3` 中。
- **离线自主运转 + 手机 24/7 直连**：笔记本 `A` 合盖断网后，常驻群主 `B` 依然持续驱动群内在线成员协作接力；手机端（已直连配对 `B`）全天候看群聊、做审批、发消息；`A` 醒来后订阅 `B` 的事件流瞬间对齐进度。
- **星型单主、零脑裂、禁止多跳穿透**：每个群仅有一个权威宿主节点，成员节点绝不维护第二份群消息库。遵守 `cross-machine.md` §3 单跳约束：选定节点 `B` 为群主时，可选成员节点集合精确为「`B` 自身 + `B` 已直接配对的对端节点 `C`（经 `op=forward(B)` 调 `peer.list` 动态列出）+ 反向附着的本机 `A`」。若用户想把未与 `B` 配对的机器 `D` 拉进 `B` 的群，向导明确提示先完成 `B ↔ D` 节点配对。

#### 2) 双向对称建链（`op=node_group_link` + `op=node_group_attach`）：兼容 `inbound = 0` 与 `DIAL_ONLY`
建链对称支持两个方向，全部复用 `peer_nodes::connector` 的单一 QUIC 连接与单密钥 endpoint（严守 `SameEndpointIdConnected`），且按对端 `inbound` 分级授权帧权限：

| 场景 | 谁主动拨出 QUIC 流 | 握手 Op | 说明 |
| :--- | :--- | :--- | :--- |
| **本机 `A` 当群主，成员在服务器 `B`**（常见：`A` 为 `DIAL_ONLY`，`B` 开入站） | Host `A` → Member `B` | `op=node_group_link` | 复用 `A` 上 `peer_nodes::connector(B)` 既有连接，零新 endpoint。 |
| **服务器 `B` 当群主，成员在服务器 `C`**（`B` 已配对 `C`） | Host `B` → Member `C` | `op=node_group_link` | 由群主 `B` 主动向 `C` 建流，笔记本 `A` 离线完全不影响 `B ↔ C` 协作。 |
| **服务器 `B` 当群主，成员包含笔记本 `A`**（`A` 关入站 / `inbound = 0`，`B` 拨不通 `A`） | **Member `A` → Host `B`（受限反向附着）** | **`op=node_group_attach`** | 必须由 `A` 本机人类先把本地会话写入 `remote_group_member_grant`，再由 `A` 主动向 `B` 拨出双向复用流。此通道上 `A` 只接收白名单 `local_session_id` 的 `deliver_wake` / `abort_turn` 与已发布 `published_artifact_id` 的读取请求，**硬拒绝** `B` 下发 `spawn_or_wake`、`worktree_apply_patch`、任意路径读文件或伪造 `User` 提权（见 §7.3）。 |

- **统一复用帧协议（按对端 `inbound` 分级授权）**：
  - **Host → Member 下行帧**：
    - *仅在成员节点对该 Host 开启 `inbound = 1` 时允许的控制帧*：`spawn_or_wake`、`worktree_diff` / `worktree_apply_patch`；
    - *`inbound = 0`（受限反向附着）同样允许的数据帧*：`attach_sync`（仅对账本地白名单内成员）、`deliver_wake`（`A` 侧按 §7.3 强制校验与限流）、`abort_turn`、`artifact_read`（**仅限已发布 `published_artifact_id`**）。
  - **Member → Host 上行帧**：`member_rpc`（`group_post`、`group_read`、`group_yield`、`group_blind_submit`、`group_task_update`、`group_lease_*`、`group_artifact_fetch`、`group_worktree_merge`）、`member_status`（事件驱动实时推送）、`turn_ended`（回合结束触发自动桥接）。
- **短连接回退**：当 Host 开着入站且长流瞬断时，Member 可走 `node.group_call` 兜底补发（Host 按 `agent_group_member` 表做 `(Principal::Peer(caller), group_id, member_id, session_id)` 精确作用域白名单校验）；若 Host 不可入站，Member 在有界内存队列按 `idempotency_key` 缓存，重连后自动重放对齐。

#### 3) 远端无状态重建（`RemoteGroupAttachment`）与断线恢复
- 成员节点仅在内存里保存轻量表 `RemoteGroupAttachment { host_runtime_id, group_id, member_id, alias, session_id }`（加本地白名单表 `remote_group_member_grant`），用于动态开启 `smeltd --peer-mcp` 群工具与 `ActiveGroupTurn` 跟踪。
- **容错矩阵**：
  - **成员节点离线 / 休眠**：Host 把该成员标为 `offline`，后续 `@` 落库为 `offline_pending` 并合并增量，**不阻塞其他在线成员**；盲评标出离线并允许提前揭晓。上线重连后自动补收合并增量。
  - **成员节点 `smeltd` 重启**：流重建后 Host 自动重推 `attach_sync`，瞬间恢复绑定。
  - **群主节点 `B` 离线或重启**：桌面将归属 `B` 的群标为「群主离线（等待重连）」；`B` 重启后从本地 `~/.smelt/groups.sqlite3` 恢复全部状态，执行启动自愈 `resume()` 并重建双向流对账未结算投递。

#### 4) 多群主聚合视图与统一寻址（`(host_runtime_id, group_id)`）
- 桌面插件 `plugins/groups/` 以 **`(host_runtime_id, group_id)`** 作为群唯一地址：聚合订阅本机与全部已连 `peer_node` 的 `group.changed` / `group.message_appended`，在左侧活跃群组视图统一展示，非本机群主挂克制节点标签（如 `gpu-box`）；所有 `group.*` Control API 按 `host_runtime_id` 经 `call(host_runtime_id, "group.*", params)` 直达对应群主。

#### 5) 跨机器异构引擎发现与选址
- 建群第一步选**群主节点**；第二步添加成员时，目标节点下拉框展示群主可达节点，经 `op=forward` 拉取目标机器各自的 `new_session.catalog` 与目录树（或绑定已有会话）。各成员路径与操作系统完全独立。

#### 6) 跨机器产物按需拉取（Cross-Machine Artifact Fetch）与代码汇合
- 消息附件 `attachments_json[].runtime_id` 标记产物所在节点；其他节点成员调 `group_artifact_fetch` 时，群主经双向复用流按范围只读拉取（带 `realpath` 沙箱与 256 KB 文本上限）。
- 代码汇合既支持共享 Git Remote 分支，也支持 `group.worktree.apply_patch` 直接跨机应用 unified diff。

#### 7) 跨机器双侧安全闸门与直连私有抽屉
- **群主侧开关**：人类手动点名始终允许；群内 Agent 自动 `@` 唤醒另一台机器上的成员须满足群主节点对该目标节点的 `peer_node.agent_access == 1`（或对反向附着成员已获得本机 `remote_group_member_grant` 授权），否则弹权限放行卡。
- **成员侧双检**：成员节点投递前按 §7.3 及目标会话真实 `ApprovalPosture` 二次核验。
- **直连私有会话与审批**：桌面右侧抽屉打开任意成员私有会话时，直接渲染 `<SmeltConversation machine={member.runtime_id} sessionId={member.session_id} />`——经现有 `op=forward` 直连该成员所在机器的 ACP 流，海量工具输出与流式 diff 不经群主中转，审批按钮直接作用于目标机器。

---

## 9 · 桌面 JS 插件与移动端交互设计（`plugins/groups/`）

全部桌面 UI 实现为独立内置 JS/TSX 插件 `plugins/groups/`（id：`smelt.groups`），遵循 [`ui-design-language.md`](ui-design-language.md)，零 Rust UI 耦合：

### 9.1 空间布局（对齐 `plugins/layouts/layouts/standard.json`）

```text
┌──────────────────────────────────────────────────────────────────────────────────┐
│ 群标题：Auth 重构攻坚 [群主: gpu-box] [Coordinator: @architect] [剩 7/10 跳] [暂停] │
│ 成员轨：[● @architect · Claude] [◐ @codex · Codex · gpu-box] [! @agy · 待审批]    │
├──────────────────────────────────────────────┬───────────────────────────────────┤
│  主群聊时间线（虚拟列表，高信噪比）           │  右侧按需抽屉（可折叠 / 切 Tab）  │
│                                              │  [私有会话] [任务板] [分支变更]   │
│  [📌 钉选共识] 2 条决策 ▾                     │                                   │
│                                              │  ┌─────────────────────────────┐  │
│  @architect (Claude Code) · 10:14            │  │  <SmeltConversation>        │  │
│  接口已定义，请 @codex 实现 refreshToken...  │  │  实时查看 @codex 私有会话   │  │
│  [📄 crates/auth/src/token.rs:1-85]  [看过程] │  │  · 完整工具调用 / 测试输出  │  │
│                                              │  │  · 权限审批操作             │  │
│  ┌─ 盲评对比卡：鉴权方案评审（已揭晓 3/3）─┐ │  │  · 可直接私聊微调，不刷群   │  │
│  │  Claude   │   Codex    │    AGY        │ │  └─────────────────────────────┘  │
│  │  (摘要/展开并排 Diff 对比 · 发起质证)   │ │                                   │
│  └────────────────────────────────────────┘ │                                   │
│                                              │                                   │
│  ┌─ ⚠ 权限放行：@codex 请求唤醒全权限 @claude ─┐                                │
│  │  [批准本次]  [本轮免问]  [拒绝]             │                                │
│  └────────────────────────────────────────────┘                                │
├──────────────────────────────────────────────┴───────────────────────────────────┤
│  [@ 提及] [/blind 盲评] [📎 附件]   输入消息，输入 @ 选择成员...       [发送 ⌘↵] │
└──────────────────────────────────────────────────────────────────────────────────┘
```

### 9.2 关键交互细节

1. **无缝钻入私有会话（Drill-down）**：
   - 点击顶部成员胶囊或任意群消息旁的「看过程」，右侧抽屉立即加载 `"smelt/conversation"` 的 `<SmeltConversation machine={...} sessionId={member.session_id} />`。
   - **主群保持高信噪比，私有会话保留 100% 原始可观测性**；用户可在抽屉里单独纠偏某个 Agent，不刷主群。
2. **桌面工作台入口与零耦合集成**：
   - **顶栏 + 左侧独立轻量列表**：在 `smelt.launcher/nav` 注册「群组」入口，并在 `left_dock`（`smelt.session-list/main` 上方）摆放独立可折叠视图 `smelt.groups/sidebar`（展示所有群主节点聚合的活跃群组，带成员状态冒泡的未读/待审批角标，**与 `session-list` 零代码耦合**，删掉 `plugins/groups/` 零影响）。
   - **中栏主舞台 + 右栏原生联动**：点击任意群在 `center` 的 `smelt/outlet` 打开群工作台，同时通过 `set_active_context` 联动 `right_dock` 的文件树、Git、历史面板。
   - **右键菜单贡献**：通过 `register_context_menu_items` 给 `smelt.session-list/conversation` 注入「加入群组…」，给 `smelt.session-list/project` 注入「新建智能体群组…」。
3. **快捷建群模板（Preset Templates）**：
   - 纯 JS 插件内预置常用异构阵容模板（用户可随时自定义或 `smelt plugin fork` 改写）：
     - **架构 + 实现 + 审查铁三角**：`@architect`（Claude, `shared_readonly`, Coordinator）+ `@builder`（Codex, `isolated_worktree`）+ `@reviewer`（AGY / Pi, `shared_readonly`）
     - **跨模型盲评评审团**：`@claude` + `@codex` + `@agy`（默认快捷发起 `/blind` 并发盲评）
4. **移动端（Flutter）对齐**：
   - 复用同一套 `group.*` Control API 与 `group.changed` / `group.message_appended` 事件流，支持群聊收发、`@` 选人、盲评卡片切换查看、权限放行卡审批、一键暂停/续跳。

---

## 10 · 分阶段实施与完整验证计划

按照 Smelt「系统分层生长、先写失败用例再实现、每阶段完整可验证」的原则，拆为 5 个递进交付阶段：

| 阶段 | 核心交付范围 | 关键自动化测试（先红后绿） |
| :--- | :--- | :--- |
| **Phase 1：独立库与群组核心状态机** | 在 `smelt_paths::DECLARED_ENTRIES` 登记 `groups.sqlite3`；新增 `GroupStore`（5 张表，`GROUP_STORE_SCHEMA_VERSION = 1`，不动 `smelt.sqlite3`）；`smeltd/src/group_runtime.rs`（Commit-before-Publish、启动 `resume()` 自愈、CRUD、`@mention` 解析、Delta Cursor + Delta Coalescing、Chain Hop + 乒乓熔断、ApprovalPosture 放行卡）；Control API `group.*` 与事件话题 `group.changed` / `group.message_appended`。 | `GroupStore` 建表、约束与事务回滚测试；`smeltd` 单元测试覆盖 Commit-before-Publish、重启 `resume()` 幂等续投、Delta Coalescing、权限放行拦截矩阵、跳数耗尽挂起、乒乓熔断检测、人类插话抢占旧链。 |
| **Phase 2：全引擎通道打通与自动兜底** | `smeltd --peer-mcp` 动态暴露 `group_*` 工具（含 `-32601` 兼容）；Pi 扩展群工具；`smelt group` CLI + 进程树鉴权；`AcpSession` `ActiveGroupTurn` 回合结束自动桥接；`smelt-model` `parse_group_message_envelope` 冷恢复投影。 | 假 ACP 驱动集成测试：支持 MCP / 忽略 MCP（退回 CLI） / 既不调 MCP 也不跑 CLI（触发自动桥接）三类引擎完整群聊闭环；验证普通单聊不暴露群工具；验证 `auto_bridged` 不触发递归扇出。 |
| **Phase 3：盲评栅栏、任务消息与双层 Worktree** | 复用 `agent_group_message` + `mention` 实现盲评并发派发 + 未揭晓硬隔离过滤 + 原子揭晓；`kind='task'` 消息状态读写与 `<active-tasks>` 注入；复用 `workset` 实现双层分支（`integration` + worker worktree）创建/diff/自由 merge/清理；内存态顾问式文件锁与 TTL 过期。 | 盲评隔离测试（交卷前 `group_read` 绝不泄露他人答卷、全部交齐自动揭晓、忘调工具自动桥接交卷不卡死、重启后未揭晓盲评自动恢复）；内存文件锁路径 glob 冲突与续期测试；真实 git 临时仓库双层分支创建/合并/清理测试。 |
| **Phase 4：桌面 JS 插件 `plugins/groups/`** | 新建 `plugins/groups/`（三栏工作台、`left_dock` 独立可折叠 `smelt.groups/sidebar`、顶栏 `launcher/nav` 入口、成员状态轨、`@` 与 `/blind` 智能输入、盲评并排对比卡、放行卡、`<SmeltConversation>` 私有抽屉、任务板与分支变更面板、菜单贡献、`locales/ui.yml`）。 | `bun test plugins/groups/tests` 纯逻辑单测（`@` 补全、盲评卡片聚合、增量消息合并）；`smelt plugin check plugins/groups` 打包校验；验证删除 `plugins/groups/` 后桌面启动零报错。 |
| **Phase 5：跨机器远程群主 & 远程成员 & 零信任加固压测** | `peer_nodes.rs` 新增分级双向复用流 `op=node_group_link`（群主→成员）与受限反向附着 `op=node_group_attach`（配合 `remote_group_member_grant` 白名单表、用户可切换 `allow_remote_full_access`、远端 `User` 降级、`published_artifact_id` 沙箱、本机速率配额与一键断链）+ `RemoteGroupAttachment` 无状态重建 + `offline_pending` 重连补发；桌面多群主聚合订阅与建群选群主向导；Flutter 移动端群视图；真机多机四引擎压测。 | 安全对抗集成测试（模拟被攻破群主 `B`）：1) 攻击 `B` 在 `op=node_group_attach` 上伪造 `spawn_or_wake` / `worktree_apply_patch` / 任意路径 `artifact_read` → `A` 全部硬拒绝；2) 攻击 `B` 伪造 `trigger_posture=User` 试图绕过 `allow_remote_full_access=0` 唤醒 `A` 的 `FullAccess` 会话 → `A` 拦截弹本机确认；3) 验证用户本机切换 `allow_remote_full_access=1` 后放行、且 `B` 无法篡改该开关；4) `local_user_origin_nonce` 防重放、断线重连 `offline_pending` 补发与一键撤销 grant 立即断链测试。 |

---

## 11 · 已敲定产品与架构决策汇总

| 维度 | 已定决策 |
| :--- | :--- |
| **1. 成员会话归属** | 区分 `group_owned`（群新建成员封装在群内、不占主侧栏、随群归档、状态冒泡）与 `linked`（外部拉入会话保留在主侧栏并挂群角标） |
| **2. 自由群默认路由** | `mention` 模式下不 `@` 仅落群记录、零意外消耗，消息下提供一键补点名芯片（`@成员` / `@all` / `/blind`） |
| **3. 自动桥接边界** | 盲评必自动交卷；普通回合自动同步入群，正文里的 `@` 默认不触发二次扇出（旁边提供一键继续唤醒按钮） |
| **4. 跨机器远程群主与 FullAccess** | 支持选择任意远程常驻节点作群主；本机 `A` 用户自主选择并可中途随时切换 `allow_remote_full_access`（与普通会话一致）；该开关及派生/读未发布文件/应用补丁权严格锁定为 `A` 本机独享，远端 `B` 无权越界 |
| **5. 代码合并权** | 双层分支（群级集成分支 `smelt/group/<slug>/integration` + 各写手独立 worktree），**不人为硬锁合并权限**，允许 Agent 自由合并到集成分支或目标分支（遵循自身权限姿态） |
| **6. 桌面入口布局** | 顶栏 `launcher/nav` 入口 + `left_dock` 独立可折叠 `smelt.groups/sidebar`（零耦合 `session-list`）+ `center smelt/outlet` 群工作台（内置右侧私有抽屉）+ 联动 `right_dock` |
| **7. 存储架构与自愈** | 独立运行时库 `~/.smelt/groups.sqlite3`（`smeltd` 独占写、5 张极简表、不动 `smelt.sqlite3` v42），复用既有 `workset`/`session`/`acp_detail`，吸收 `pi-durable` 的 Commit-before-Publish 与启动 `resume()` 持久化自愈语义 |
