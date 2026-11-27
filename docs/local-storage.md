# 本地状态存储

Smelt 自有的结构化状态分两份 SQLite，都在 `~/.smelt/`（Windows 上是 `%USERPROFILE%\.smelt`，可用 `SMELT_HOME` 改）。不再用 JSON 文件当数据库。`~/.smelt` 顶层放什么在 `smelt_paths::DECLARED_ENTRIES` 里登记，新加的文件或目录要先登记；没登记的（旧 JSON、迁库备份 `*.bak-v*`、下线功能的目录）都是历史残留，设置「系统 → 清理残留」列出来，确认后删掉（维护插件 `crates/plugins/maintenance/src/residue.rs`）。插件的数据一律放 `plugin-data/<插件 id>/`，不另占顶层。

## 原则

- **按字段建表，不用 JSON 当库**：业务数据统一存 SQLite 按字段建表，不把一组字段塞进一个 JSON 列（嵌套配置如自动化 trigger，或整块读写、从不按局部查询且带版本号的界面状态如 `DockAreaState` 除外，读到不认识的版本或非法内容回默认值）。
- **数据单一权威**：同一份数据只存一处，其余地方只留从它推送来的只读缓存。不在客户端另存一份运行时数据再靠对账推断增删，不存可从别处算出来的派生列或镜像表。
- **改表结构先确认**：改 SQLite 表结构或给 `SCHEMA_VERSION` / `STORE_SCHEMA_VERSION` 加一前，先列出要改的表、列和理由等确认；能不动就不动，一个分支只升一次，所有改动并进同一版。

## 运行时库

```text
~/.smelt/smelt.sqlite3
```

这台机器上的会话、项目、智能体、自动化、配对设备和远程开关。schema 版本是 `user_version`，当前 `STORE_SCHEMA_VERSION = 43`（`crates/smelt-store/src/lib.rs`）。WAL，5 秒 busy timeout，库文件和 `-wal`/`-shm` 在 Unix 上收紧为 `0600`。

建库只发生在主文件和 sidecar 都不存在时。主文件没了但 sidecar 还在时拒绝建空库。非空的 `user_version = 0`、形状不符或版本过新的库只拒绝打开，不删表、不重建。以后改已发布结构必须加版本号并写迁移（完整迁移史见 `crates/smelt-store/src/lib.rs`）。

名字只存一处：认得出 provider 身份的对话（ACP 有 `history_session_id`）名字在 `history_title`，历史页、侧栏、手机看到的同一份；还认不出的写在 `session_group.custom_title`（终端分屏是 `session.custom_title`），守护确认身份时在同一事务里搬进 `history_title`。读工作区目录时按身份合起来，不另存。名字只经 `workspace.rename`（命令行按会话 id 改、历史页按身份改的也是同一套写入）写，客户端 `workspace.conversation.create` / `update` 带来的名字不算数。`acp_detail.session_title` 是 agent 报的标题，只由守护在标题变化时写。

现行表（`crates/smelt-store/src/schema.rs` 的 `CREATE_SCHEMA`）按职责：

| 范围 | 表 |
| --- | --- |
| 工作区 | `project`、`project_repository`、`project_repository_catalog`、`workset`、`workset_repository`、`session_group`、`session`、`acp_detail`、`acp_launch_env`、`acp_config_value`、`workspace_catalog` 及项目/会话行 |
| 自动化 | `automation`、`automation_state`、`automation_run`、`automation_run_transcript`、`automation_meta` |
| 智能体 | `agent_definition`、`agent_profile`（自定义 Agent）、`agent_profile_env`、`agent_host_pref`、`agent_acp_env`、`agent_acp_config_memory`、`agent_memory_usage`、`agent_memory_provenance`、`default_conversation_agent` |
| 启动与历史 | `launch_entry`、`launch_settings`、`new_session_pin`、`history_title`、`history_agent_binding` |
| 远程 | `paired_device`、`push_registration`、`remote_config`、`runtime_identity`、`published_session` |
| 群组 | `agent_group`、`agent_group_member`、`agent_group_message`、`agent_group_message_mention`（本地智能体群组、成员、消息与 `@` 投递记录，见 `docs/agent-groups.md`） |
| 配置缓存 | `memory_settings`、`terminal_theme`、`terminal_theme_palette`、`auto_model_provider`、`quota_cache_status`、`quota_cache_window`、`quota_provider_pref`（每家额度账户的开关和钥匙串拒绝状态） |

嵌套配置可以放在 JSON 列里（例如自动化的 trigger）。那是列，不是一份 `*.json` 活文件。

旧的 `~/.smelt/smelt.db` 属于已经下线的 instincts，没登记，算历史残留，迁移不读它。

## 客户端库

```text
~/.smelt/client.sqlite3
```

只属于这台桌面，`SCHEMA_VERSION = 17`（完整迁移史见 `crates/smelt-client-store/src/schema.rs`）。

| 表 | 内容 |
| --- | --- |
| `appearance` | 主题、界面字号和字体、终端字体、背景图和它的不透明度、材质，选的布局（`layout_id`，空是没选过，用第一个登记的布局；选的布局暂时没登记时照样留着） |
| `key_binding` | 用户改过的快捷键：命令 id（`<来源>/<名字>`）→ 按键，按键为空是用户清掉了。没有行的命令用出厂键位；命令还没登记（插件没加载）的行照样留着 |
| `workspace_ui` | 主窗口当前出口（停在哪一页） |
| `workspace_layout_state` | 每份布局被用户拖成的样子：gpui-base 的 `DockAreaState` 原文。版本对得上、摆的视图和摞（`smelt/stack`）跟布局文件一样就用，只是拖动、换组、改大小的照用；布局文件增减了视图或改了摞，存档过时不用；要回到布局文件就删掉这份存档 |
| `runtime_connection` | 这台桌面连过哪些运行时（endpoint、relay、名字、所在系统 `os`） |
| `runtime_migration` | 客户端数据迁出运行时库的标记 |

从旧版本迁移前，旧库整份留成 `client.sqlite3.bak-v<旧版本>`。

## 插件库

```text
~/.smelt/plugin-data/<插件 id>/store.sqlite3
```

每个插件一个目录，整个归它：库在 `store.sqlite3`，表和版本由插件自己管（`crates/smelt-client-store/src/plugin_store.rs`），宿主不认识里面的表；库以外的文件也放这个目录（JS 插件用 `"smelt/sdk"` 的 `dataDir()` 取）。内置插件也一样：通知插件的开关和发过的通知在 `smelt.notifications/`，文件视图的在 `smelt.files/`。插件之间不共用目录：复制一个插件改成新 id，数据要另拷一份。

旧版本的库是 `plugin-data/<插件 id>.sqlite3`，插件第一次打开库时挪进自己的目录（先把 WAL 并回主库再改名）；别的进程还开着旧库就这次照用旧位置，下次再挪。

## 手机缓存

手机的离线缓存不在 `~/.smelt/`，在 App 数据目录的 `smelt-mobile-cache.sqlite3`（`crates/smelt-mobile/src/cache.rs`）。按 endpoint 存会话列表和最近几个对话快照。库坏了或版本不认识就整库重建，不做迁移。

## 继续留在文件系统上的

- 日志、socket（`~/.smelt/smeltd.sock`）、lock、skills
- `~/.smelt/cache/bun-x/<包@版本>`：MCP 预设经 `smelt-agent x` 跑的包，一个钉死的版本一个目录，不进用户的 `~/.bun`。同一个包别的版本一天没用过，下次启动它时在后台整份删掉（`agent/src/pi-mcp-runtime.ts`）。预设换了钉的版本，守护启动时把 `~/.pi/agent/mcp.json` 里按预设登记的条目跟着换（`smelt_core::pi_mcp::upgrade_preset_pins`）
- `~/.smelt/iroh-secret`：这台运行时的 iroh 身份，重启不变。没有 `remote-token`
- 手机上的 `client-iroh-secret`：那台设备的身份，在 App 数据目录
- `.claude/settings.json`、`.codex/hooks.json` 等外部产品自己的配置
- 工作区里交给 agent 读的 `context.json`
- 记忆正文：`~/.smelt/agents/<id>/memory/*.md` 与 `~/.smelt/memory/`，元数据在运行时库

## 谁写

- 运行时库由 `smeltd` 写。桌面、手机和 `smelt` CLI 走 Control API，不各自打开这张库改。
- 客户端库由这台桌面写。外观修改经运行时转给打开着的桌面；桌面没开时，CLI 改外观会失败。
- 新功能走 `smelt-store` 或 `smelt-client-store` 的仓储。Pi 启动器不链接 `smelt-store`，记忆元数据经 `agent.memory.*` 由主守护代写。ACP 对话的驱动在 `smeltd` 进程里，不再另起会话宿主进程。
