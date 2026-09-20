# 本地状态存储

## 唯一主库

Smelt 自有的结构化本地状态统一存放在：

```text
~/.smelt/smelt.sqlite3
```

数据库使用 WAL 和 5 秒 busy timeout，数据库和 `-wal`/`-shm` sidecar 在 Unix
上均收紧为 `0600`。每个进程复用一个主库连接；SQLite 继续负责 GUI 与 daemon 之间的
跨进程协调。数据库 schema 版本使用 `user_version` 管理。

建库只发生在主文件和 sidecar 都不存在时（真正的首次安装）。`Store::open` 只打开已有库；
主文件没了但 `-wal`/`-shm` 还在时拒绝建空库。旧版本按事务迁移升级。以后改已发布结构必须：
版本号加一、写对应事务迁移，禁止直接删表重建。

非空的 `user_version=0` 数据库、形状不符的 v1 或版本过新的数据库只会被拒绝打开，
禁止删表、禁止重建空库。
旧二进制遇到未知的未来关系 schema 会在修改 journal mode 之前拒绝打开。

旧的 `~/.smelt/smelt.db` 属于已经下线的 instincts 功能，不是当前主库。启动清理会
删除它，迁移过程不会覆盖、读取或复用。

## 表结构

主库按领域拆表。会话/项目与自动化是两套关系，不共用一张大 JSON 文档：

| 表 | 责任 |
| --- | --- |
| `kv` | 有作用域的标量、集合项和兼容元数据；主键为 `(scope, key)` |
| `launch_entry` | 用户维护的有序快捷启动项；配置版本放 `kv/launch` |
| `project` | 已打开项目、顺序和折叠状态 |
| `session_group` | 侧栏中的会话组、活动叶子、标题和更新时间 |
| `session` | 实际终端或 ACP 运行会话；每个分屏叶子一行 |
| `layout_node` | 分屏递归树；split 保存方向，leaf 引用 `session` |
| `acp_detail` | `session` 的一对一 ACP 扩展；不形成第二个 session |
| `history_title` | agent 历史会话的用户标题覆盖 |
| `automation` | 自动化定义（时机/动作 JSON 列） |
| `automation_state` | 自动化调度游标；随定义级联删除 |
| `automation_run` | 每次 Run；随定义级联删除 |
| `automation_run_transcript` | Run 完整对话；随 Run 级联删除 |
| `automation_meta` | 自动化存储的 revision / store_id |
| `agent_ui_pref` | 智能体界面开关（hooks / 通知等） |
| `agent_acp_command` | 各家 ACP 启动命令；主键是历史平铺键名 |
| `agent_acp_env` | 各家 ACP 启动环境变量 |
| `agent_acp_config_memory` | 各家最近一次显式选择的模型等配置 |
| `agent_definition` | 用户定义的本地智能体 |
| `agent_profile` | 手动添加的 workspace profile |
| `remote_acp_session` | 守护进程远程 ACP 会话目录 |
| `remote_terminal_session` | 守护进程远程终端会话目录 |
| `published_session` | 守护进程已发布会话身份（崩溃恢复） |

没有独立实体的状态走 `kv(scope, key, value, value_type, updated_at_ms)`。例如外观字段用
`scope=appearance, key=theme_mode`；工作区 ACP 会话 env/config 使用 `scope=acp:<session_id>`；会话
route 的集合按索引拆成多行。`kv` 不保存整份 workspace payload。

侧栏菜单是 `project`、`session_group`、`session` 的运行时投影，不建 menu 表。
`legacy_imports` 和 `quarantine` 也不占表，分别使用 `legacy:<source>` 与 `quarantine`
作用域。

已拆成关系表的领域（自动化、启动项、外观、历史标题、工作区、智能体配置、远程会话、
守护会话目录）活路径都走类型化快照，不再经 `*.json` 文档入口。配置/缓存（远程开关、
更新设置/状态、终端配色、额度缓存、worktree 继承、自动模型名单）同样走类型化快照，
落在 `kv` 的独立作用域里，不占 `document:*.json`。侧栏菜单仍是投影，守护进程需要
重启恢复时只把发布快照写进 `kv/workspace_menu`，不建 menu 表。残留 JSON 只作一次性
导入源，写入主库后删除。EventHub / GUI 内存对象是接口投影，不是持久化。嵌套配置仍
可以 JSON 列存放。

## JSON 迁移

Smelt 自有状态的活路径只读写 `~/.smelt/smelt.sqlite3`。旧版本留下的
`~/.smelt/*.json` 不再导入，启动清理会删除这些残留文件。

历史上的一次性导入规则（仅作考古）：

1. `enable_sqlite_state()` 只打开 SQLite 主库路由，不扫描、不投影旧文件。
2. 领域加载器先读类型化快照；数据库已有值时绝不让旧文件覆盖。
3. 数据库无值时按该领域 DTO 解析旧 JSON（带 serde default 与旧格式迁移）。损坏文件
   保持原位并返回错误，不写默认值覆盖。
4. 解析成功并写入类型化快照之后才删除旧 JSON，不另留 `migration-backups`。崩溃发生
   在写库后、删文件前时，下次从快照读取并补删残留文件。
5. 普通“文件尚不存在”不写永久标记，兼容升级期间晚到的旧版本写入；只有显式删除或
   成功隔离才写 `deleted` 墓碑。
6. 后续只读写 SQLite，不进行长期双写。

无法解析的兼容数据会原子移入 `kv` 的 `quarantine` 作用域；尚未导入的坏 JSON 则在
同目录 rename 成隔离文件，成功后才写墓碑。隔离失败不改变迁移状态，修复后的文件仍可
在下一次读取时导入。

桌面 GUI、守护进程和独立 gateway 在 `main` 启动时显式启用 SQLite 路由。库调用和测试
进程默认仍使用文件语义，避免测试意外打开开发者的真实家目录。自动化是例外：
`persist=true` 时始终写同目录 `smelt.sqlite3`，不回退成 JSON 活文件。Smelt 不拥有的
外部 JSON 同样不走数据库。已经下线、当前代码不再读取的旧 JSON，以及 skill package、
workspace manifest 等外部协议，都不会被导入。

## 继续保留的文件

SQLite 只承载 Smelt 自有结构化状态。以下内容保持文件系统语义：

- 日志、socket、lock、skills、runtime 和 Git worktree；
- `remote-token`、`iroh-secret`、签名证书等非 JSON 凭据；
- `.claude/settings.json`、`.codex/hooks.json` 等由外部产品拥有的配置；
- 外部工作区内的 `context.json`，它是交付给该工作目录和 Agent 阅读的 manifest，不是全局状态库。

## 所有权

- `smeltd` 继续独占任务、已发布会话和远程会话目录的写入；
- GUI 独占 workspace 和界面设置的写入；
- SQLite 负责跨进程事务与锁，不改变既有领域 owner；
- 新功能必须通过 `smelt-store`/强类型 repository 接入，不能在业务模块直接打开数据库。
