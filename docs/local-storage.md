# 本地状态存储

Smelt 自有的结构化状态分两份 SQLite，都在 `~/.smelt/`（Windows 上是 `%USERPROFILE%\.smelt`，可用 `SMELT_HOME` 改）。不再用 JSON 文件当数据库。启动时会删掉已经没有读写路径的旧 JSON（`workspace.json`、`appearance.json`、`launch.json`、`collab.json`、`llm.json`、`pet.json` 等），见 `crates/smelt/src/storage_cleanup.rs`。

## 运行时库

```text
~/.smelt/smelt.sqlite3
```

这台机器上的会话、项目、智能体、自动化、配对设备和远程开关。schema 版本是 `user_version`，当前 `STORE_SCHEMA_VERSION = 28`（`crates/smelt-store/src/lib.rs`）。WAL，5 秒 busy timeout，库文件和 `-wal`/`-shm` 在 Unix 上收紧为 `0600`。

建库只发生在主文件和 sidecar 都不存在时。主文件没了但 sidecar 还在时拒绝建空库。非空的 `user_version = 0`、形状不符或版本过新的库只拒绝打开，不删表、不重建。以后改已发布结构必须加版本号并写迁移。

通用 `kv` 表在 v18 删掉。v19→v20 删掉 worktree 继承表（smelt 不管 worktree）。v20→v21 的 `layout_node` 增加 `tabs`，同一格可以叠多个终端。v21→v22 删掉 `layout_node`：对话的终端平铺成 `session.position`，分屏布局归客户端（下面的 `conversation_route.dock_layout`）。v22→v23 删掉跨 Agent 消息的 `peer_message_command` 和 `agent_host_pref.cross_agent_enabled`，以及只为它服务的持久事件表（`event_log`、`event_outbox`、`subscription_cursor`、`event_dead_letter`、`event_delivery_retry`、`command_dedupe`）。事件只在内存里投递。v23→v24 删掉 DeepSeek Harness（`dsh`）的全部数据，`auto_model_provider` 去掉 `harness` 列。v24→v25 的 `published_session` 增加 `cli_exited`。对话 id 退出后仍留着，续接与否看这一列：CLI 发出 `SessionEnded`（用户 `/exit` 回到 shell）后，守护重启、升级或崩溃恢复不再对该终端执行 `--resume`；不发 `SessionEnded` 的 provider（Antigravity、Kiro、Crush，以及只在删除会话时转发 SessionEnd 的 OpenCode）仍按最后一次对话 id 续接。v25→v26 删掉 `workspace_catalog` 的 `writer` / `writer_seq`：整份保存去掉后，目录只经单条改动写，版本号在同一事务里随改动推进，不再需要认出回执丢了的写入方。v26→v27 删掉 `session_group.project_root` 和桌面发布的侧栏菜单三张表（`workspace_menu`、`workspace_menu_project`、`workspace_menu_session`）：会话归哪个项目不存，各端按 cwd 用 `smelt_model::workspace_directory::project_root_of` 现算；手机和守护直接读工作区目录。`project_root` 带外键、`session_group` 又被下面一串表级联引用，所以 `session_group`、`session`、`acp_detail`、`acp_launch_env`、`acp_config_value` 整串改名重建：外键开着时改名会把子表的引用改到旧表上，单独重建会在删旧表时把会话级联删光。v27→v28 删掉远程会话目录 `remote_acp_session`、`remote_terminal_session`：手机、命令行、智能体、Pi 派生新建的对话由运行时起好后直接写进工作区目录，和桌面建的一样恢复和删除；自动化运行的会话不进工作区目录，看 `automation_run` 和守护里活着的会话。守护每次启动都会清空这两张表，没有要搬的数据。v28→v29 给 `workspace_catalog` 加 `epoch`：这份库的随机身份，建库、迁移时写好。目录和命令回执都带着它，版本号只在同一个 `epoch` 里比；库删了重建，版本号从头数，客户端认出是另一份，不会把新目录当成旧的扔掉，本地开着的对话发新建写回新库。

名字只存一处：认得出 provider 身份的对话（ACP 有 `history_session_id`）名字在 `history_title`，历史页、侧栏、手机看到的同一份；还认不出的写在 `session_group.custom_title`（终端分屏是 `session.custom_title`），守护确认身份时在同一事务里搬进 `history_title`。读工作区目录时按身份合起来，不另存。名字只经 `workspace.rename`（命令行按会话 id 改、历史页按身份改的也是同一套写入）写，客户端 `workspace.conversation.create` / `update` 带来的名字不算数。`acp_detail.session_title` 是 agent 报的标题，只由守护在标题变化时写。

现行表（`crates/smelt-store/src/schema.rs` 的 `CREATE_SCHEMA`）按职责：

| 范围 | 表 |
| --- | --- |
| 工作区 | `project`、`session_group`、`session`、`acp_detail`、`acp_launch_env`、`acp_config_value`、`workspace_catalog` 及项目/会话行 |
| 自动化 | `automation`、`automation_state`、`automation_run`、`automation_run_transcript`、`automation_meta` |
| 智能体 | `agent_definition`、`agent_profile`、`agent_host_pref`、`agent_acp_command`、`agent_acp_env`、`agent_acp_config_memory`、`agent_memory_usage`、`agent_memory_provenance`、`default_conversation_agent` |
| 启动与历史 | `launch_entry`、`launch_settings`、`new_session_pin`、`history_title`、`history_agent_binding` |
| 远程 | `paired_device`、`remote_config`、`runtime_identity`、`published_session` |
| 配置缓存 | `memory_settings`、`terminal_theme`、`terminal_theme_palette`、`auto_model_provider`、`quota_cache` 及额度明细 |

嵌套配置可以放在 JSON 列里（例如自动化的 trigger）。那是列，不是一份 `*.json` 活文件。

旧的 `~/.smelt/smelt.db` 属于已经下线的 instincts，启动清理会删掉，迁移不读它。

## 客户端库

```text
~/.smelt/client.sqlite3
```

只属于这台桌面，`SCHEMA_VERSION = 7`（`crates/smelt-client-store/src/schema.rs`）。v3 → v4：`runtime_connection` 增加 `smelt_home`，缓存远程机器的 `~/.smelt` 路径（`system.describe` 报上来的）。旧行先空着，连上一次就有。v4 → v5：`runtime_connection` 增加 `os`。v5 → v6：侧栏顺序从运行时搬到客户端，新建 `sidebar_project_order` / `sidebar_conversation_order`；升级后第一次按运行时目录的先后排，之后按这台桌面自己拖出来的顺序。v6 → v7：新建 `key_binding`，存用户改过的快捷键。

| 表 | 内容 |
| --- | --- |
| `appearance` | 主题、界面字号、终端字体、背景、透明度、玻璃 |
| `update_settings` / `update_state` / `staged_update` / `installer_attempt` | 这台 App 的更新 |
| `notify_prefs` | 通知开关 |
| `key_binding` | 用户改过的快捷键：动作名 → 按键，按键为空是用户清掉了。没有行的动作用出厂键位 |
| `workspace_ui` | 当前路由、侧栏宽度和开合 |
| `collapsed_project` / `pinned_project` / `collapsed_agent` | 按 `runtime_id` 记住折叠和置顶 |
| `sidebar_project_order` / `sidebar_conversation_order` | 按 `runtime_id` 记住侧栏里项目、对话从上到下的顺序。每台设备各记各的，运行时目录里只有新建的先后 |
| `conversation_route` / `conversation_route_path` | 某个运行时上某段对话的终端分屏布局（带版本号的 JSON）、当前终端、工具面板、打开的文件、Git 视图 |
| `runtime_connection` | 这台桌面连过哪些运行时（endpoint、relay、名字、`smelt_home`） |
| `runtime_migration` | 客户端数据迁出运行时库的标记 |

从旧版本迁移前，旧库整份留成 `client.sqlite3.bak-v<旧版本>`。

## 手机缓存

手机的离线缓存不在 `~/.smelt/`，在 App 数据目录的 `smelt-mobile-cache.sqlite3`（`crates/smelt-mobile/src/cache.rs`）。按 endpoint 存会话列表和最近几个对话快照。库坏了或版本不认识就整库重建，不做迁移。

## 继续留在文件系统上的

- 日志、socket（`~/.smelt/smeltd.sock`）、lock、skills、受管运行时映像（`~/.smelt/runtime/store/`）
- `~/.smelt/iroh-secret`：这台运行时的 iroh 身份，重启不变。没有 `remote-token`
- 手机上的 `client-iroh-secret`：那台设备的身份，在 App 数据目录
- `.claude/settings.json`、`.codex/hooks.json` 等外部产品自己的配置
- 工作区里交给 agent 读的 `context.json`
- 记忆正文：`~/.smelt/agents/<id>/memory/*.md` 与 `~/.smelt/memory/`，元数据在运行时库

## 谁写

- 运行时库由 `smeltd` 写。桌面、手机和 `smelt` CLI 走 Control API，不各自打开这张库改。
- 客户端库由这台桌面写。外观修改经运行时转给打开着的桌面；桌面没开时，CLI 改外观会失败。
- 新功能走 `smelt-store` 或 `smelt-client-store` 的仓储。Pi 启动器不链接 `smelt-store`，记忆元数据经 `agent.memory.*` 由主守护代写。ACP 对话的驱动在 `smeltd` 进程里，不再另起会话宿主进程。
