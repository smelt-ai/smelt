---
name: smelt
description: 用 smelt CLI 操作本机 Smelt：产品智能体及其记忆、侧栏对话（含给别的对话发消息、取回复）、项目和任务工作树、各家 agent 的历史会话、自动化及其运行记录、MCP 服务器、默认模型、用量、agent 命令行工具和提醒钩子、外观设置、开着的 App 里插件开放的命令（开视图、开页面）；也讲怎么写 Smelt 插件（基于 NSH 的 App 界面 TSX/JS 插件）。用户说新建/修改智能体、开对话、让另一个对话去做事、整理对话、开项目、开/收工作树、翻历史会话、建定时任务、看自动化跑得怎样、加 MCP、换模型、看用量、装 agent 命令行、改主题字体、在 App 里打开某个视图或页面、写 Smelt 插件时使用。
---

# Smelt 控制面

通过 `smelt` CLI 操作本机 Smelt。stdout 是 JSON，失败时原因在 stderr、退出码非 0。控制面命令都要 `smeltd` 在跑；不要直接改 `~/.smelt/` 下的库和配置文件。`smelt --help` 有完整用法。

先 list 再 update，id 从 list 的结果里取，不要猜。

## 要先问用户的

下面这些动作直接生效，执行前先把要做的事讲清楚、等用户同意：

- 删除：`agent delete`、`automation delete`、`conversation delete`、`history delete`、删除 `~/.smelt/plugins/<id>/`（不可恢复）。
- 收工作树：`worktree finish`（删掉 worktree 目录；`--allow-unconfirmed-merge` 更要先问）。
- 给别人的对话发消息：`conversation send` 发进去的话那个 agent 会照着做；发给不是你自己开的对话前先确认。
- 全局能力，对所有智能体生效：`mcp add|enable|disable|remove`、`model default|thinking`、`pi skill|extension import|remove`。
- 改这台机器的环境：`engine install`（用 brew 或 npm 装命令行工具）、`hooks install|uninstall`（改各家 agent 的配置文件）。
- `app run` 一条会改数据的插件命令（看 `app commands` 里它的名字和参数判断；只开视图、页面、窗口的不用问）。

## 智能体

智能体是长期定义（名称、工作方式、技能、MCP、模型），不是正在跑的对话。

```sh
smelt agent list
smelt agent get <id>
smelt agent create --name "名称" --prompt "长期工作方式" --skill xxx --extension yyy --mcp figma --model anthropic/claude-opus-5-5
smelt agent update <id> --prompt "新的工作方式"
smelt agent update <id> --memory off
smelt agent update <id> --model default
smelt agent chat <id> --message "第一句话"
smelt agent memory <id>
smelt agent delete <id>
```

- `--skill` / `--extension` / `--mcp` / `--folder` / `--link` 可以重复写多个。update 时按类整份替换（技能、扩展各算一份，只写了技能就不动扩展），写 `""` 清空那一类；只改 prompt 时不要带它们。
- `--skill` / `--extension` 勾的是这个智能体能用的 Pi 技能和扩展，和下面的 Smelt App 插件无关，名字从 `smelt pi skill list`、`smelt pi extension list` 取。没有的先用 `smelt pi skill import <目录>`、`smelt pi extension import <路径>` 装进来，`remove <名字>` 卸掉。MCP 服务器名从 `smelt mcp list` 取，模型从 `smelt model list` 取；不存在的会直接报错。智能体只连 `--mcp` 列出的服务器，一个都没列就不连 MCP。
- `--model default` 跟随 Pi 的默认模型。
- `--memory on|off` 开关这个智能体的长期记忆（默认开）。关掉后不注入、不写入，已有记忆文件保留；正在进行的对话下一轮生效。
- `chat` 开一场新对话，出现在桌面「对话」栏。带 `--message` 会把第一句话发出去。返回的 `session_id` 是这场对话的 id，后面的操作都用它，不要拿智能体 id 去改标题。
- `memory` 列这个智能体的记忆和所有智能体共享的用户记忆，`pending_reason` 非空的是待用户确认的。采纳、删除只能用户在桌面设置里点，运行时拒绝从智能体会话里来的这类请求，不要想办法绕。

## 对话

```sh
smelt conversation list [--agent <智能体id>] [--archived]
smelt conversation send <id> --message "要它做的事"
smelt conversation result <id> <delivery_id>
smelt conversation rename <id> --title "新标题"
smelt conversation archive|unarchive <id>
smelt conversation delete <id>
```

- `<id>` 写 list 里的 `id` 或 `session_id` 都行。默认不列已归档的。终端（`terminal: true`）发不了消息。
- `send` 把话排进那段对话（它正在跑就排在这一轮后面，挂起的先唤醒），立刻返回 `delivery_id`，不等它做完。之后用 `result` 取：`pending` 是还没轮到或还在做；`finished` 带 `outcome` 和这一轮最后一段回复 `text`；`unknown` 是运行时不认识这次投递（重启过）。还在 `pending` 就先做别的，过一阵再查，不要连着刷。
- 发进去的话在桌面上看得到，用户也能插话。要一段新对话去做事，先 `agent chat` 开一场再 `send`。
- 在 Pi 对话里也可以用 Pi 自带的 `session_list` / `session_read` / `session_send` / `session_spawn` 工具。

## 项目和工作树

```sh
smelt project list
smelt project open <路径>
smelt project close <路径>
smelt worktree list [--project <路径>]
smelt worktree create <项目路径> --task <任务名> [--repo <仓库名>...] [--branch <分支>] [--prefix <分支前缀>] [--base <基线分支>]
smelt worktree inspect <id>
smelt worktree finish <id> [--allow-unconfirmed-merge]
```

- 项目就是侧栏里的一个目录。`open` 加进侧栏，`close` 从侧栏拿掉，都不动磁盘上的文件，项目下的对话也留着。
- 工作树是在项目的 git 仓库上为一个任务开的一组 worktree，放在 `<项目>/worktrees/<任务名>`，分支默认 `feat/<任务名>`，默认从远端默认分支拉出；`--base` 指定从哪个分支拉出（先找远端上的同名分支，没有再用本地分支；也可写 `origin/<分支>`），适合从大迭代分支拆小迭代。仓库认项目根目录和 `repos/<名字>` 下的，`--repo` 不写就全用；不填 `--base` 时仓库要有远端。结果里的 `path` 是任务目录，在里面干活；`setup` 列出跑过的初始化脚本（`<仓库>/.smelt/worktree-setup`）。
- 收工先 `inspect`：每个仓库 `dirty`（有没有未提交的改动）和 `merge`（分支合没合进基线）。有未提交改动或确实没合并的，`finish` 会拦下，先把情况告诉用户；判断不了合没合时才考虑 `--allow-unconfirmed-merge`，要用户点头。`finish` 后 `retained_branches` 是留下没删的分支。

## 历史会话

```sh
smelt history list --agent claude [--cwd <项目目录>]
smelt history get <path> --agent claude
smelt history delete <resume_id> --agent claude [--cwd <项目目录>]
```

- 各家 agent 自己存在本机的会话存档，按项目目录分。`--agent` 是 `claude`、`codex`、`copilot`、`grok`、`cursor`、`opencode`、`kiro`、`pi`、`antigravity`；`--cwd` 不写是当前目录；自定义 agent 的历史加 `--profile <id>`。
- `list` 最近活跃的在前，`get` 用 list 里的 `path` 读整段对话（很长时只挑要的看）。

## 自动化

```sh
smelt automation list
smelt automation get <id>
smelt automation create --name "名称" --agent <智能体id> --every-hours 1 --prompt "这一次要做什么"
smelt automation create --name "名称" --agent <智能体id> --daily 09:30 --prompt "这一次要做什么"
smelt automation create --name "名称" --agent <智能体id> --weekly mon,wed,fri@18:00 --prompt "..."
smelt automation create --name "名称" --agent <智能体id> --during 30@09:00-18:00@weekdays --prompt "..."
smelt automation create --name "名称" --agent <智能体id> --webhook
smelt automation create --name "名称" --agent <智能体id> --event topic.name --event-filter '{"branch":"main"}'
smelt automation create --name "名称" --shell "make check" --daily 08:00
smelt automation update <id> --prompt "新的单次输入"
smelt automation update <id> --daily 10:00 --notify off
smelt automation enable|disable|run <id>
smelt automation runs [<id>] [--limit N]
smelt automation run-log <run_id>
smelt automation cancel <run_id>
smelt automation delete <id>
```

- 触发条件可以组合，至少一种：`--every-minutes N`、`--every-hours N`、`--daily HH:MM`、`--weekly <星期>@HH:MM`、`--during <分钟>@HH:MM-HH:MM[@<星期>]`（时段内每隔几分钟一次，不写星期是每天）、`--webhook`、`--event <主题>`。星期写 `mon`…`sun`、`weekdays`、`weekend`、`everyday`，逗号分隔。
- update 时按类别替换：给了任何定时就整组换掉定时，`--event` 同理；webhook 已有就保留原地址（换地址会让调用方全部失效）。`--no-schedule` / `--no-webhook` / `--no-event` 去掉那一类，但不能删光。
- 定时触发必须带 `--prompt`。`--webhook` / `--event` 可以不带，收到的内容就是这次的指令，但不带 `--prompt` 时不能手动 `run`。带了 `--prompt` 时，收到的内容附在它后面；写 `{{payload}}` 或 `{{字段名}}` 可以放到指定位置。
- `--notify on|off` 控制跑完是否发应用通知，不写就保持原样。它只管例行的「跑完提醒」。想「有事才提醒」：`--notify off`，再在 `--prompt` 里写清楚什么情况下通知用户（Pi 用 `notify_user` 工具，Shell 自动化在脚本里调 `smelt notify`）。
- webhook 令牌不会出现在输出里，要地址让用户去桌面的自动化页面复制。
- 智能体自动化在它的 space（`~/.smelt/agents/<id>/automations/` 下的子目录）里运行，能读到它在对话里攒下的文件和绑定的目录，但不会带上你的项目；要处理的仓库写进 `--prompt`。
- 看自动化跑得怎样：`runs` 列最近的运行（新的在前，默认 20 条），`run-log` 看一次运行的输出、报错和对话记录。

## 通知用户

```sh
smelt notify [--title <标题>] <内容>
```

桌面弹系统通知，并推给在 Smelt 手机端打开了「智能体消息」的手机；人在不在电脑前都推。Pi 对话里用 `notify_user` 工具（会带上会话名，点开推送直接进那段会话），CLI 给脚本和别的智能体用。只在真有事要用户知道时发，别拿来报进度。

## 能力：MCP、模型、用量

```sh
smelt mcp list
smelt mcp presets
smelt mcp add <预设id>
smelt mcp enable|disable|remove <服务器名>
smelt model list
smelt model default <provider>/<模型id>
smelt model thinking off|minimal|low|medium|high|xhigh|max
smelt quota
smelt quota refresh [<账户id>]
smelt quota settings
smelt quota enable|disable <账户id>
```

- `mcp add` 只能加预设。结果里有 `next` 时（要登录、要装浏览器扩展），照原样转告用户去做；登录要开浏览器，CLI 代办不了。
- 自定义 MCP 服务器、登录模型账号、填 API key 都在桌面设置的能力库里做，CLI 不提供。
- `model default` 只能挑 `model list` 里有的模型，推理强度保持原样。
- `quota` 看缓存里各账户的用量，`quota refresh` 现查一轮（每家一次网络请求）。账户 id 在 `quota settings` 里：`claude`、`codex` 这样，Pi 底下的是 `pi:<provider>`。点名刷新一家时 macOS 可能弹钥匙串授权框，提前告诉用户。`disable` 后不再读它的凭据、不再查。

## Agent 命令行和提醒钩子

```sh
smelt engine list
smelt engine install <id>
smelt hooks status
smelt hooks install|uninstall
```

- `engine list` 列各家 agent 命令行工具在这台机器上找没找到（`path` 为空就是没装）、版本和报错；`install` 用 brew 或 npm 装一个，id 和 `agent create --engine` 的一样。
- 提醒钩子写在各家 agent 自己的配置里，让它们在等你确认、做完时经 `smelt-notify` 提醒 Smelt。`status` 看装没装；装、卸会改那些配置文件。

## App 里的命令

```sh
smelt app commands
smelt app run smelt.app/show-main-window
smelt app run smelt.app/open-view --args '{"view":"com.example.hello/main"}'
smelt app run smelt.app/open-page --args '{"page":"<页 id>"}'
```

- 调的是本机开着的 Smelt App 里插件登记的命令，只有插件明确开给命令行的才列得出、调得了。App 没开会报错，不要替用户去开 App。
- `commands` 列出每条命令的 `id`、`title` 和认的参数字段 `args`（空表就是不带参数）；`run` 把 `--args` 的 JSON 交给它，stdout 是命令交回的值。id 和参数照 `commands` 的写，不要猜。
- `open-view` 的视图 id 是 `<插件 id>/<视图名>`（由 `registerViews` 登记的 `name` 决定，不一定是 `main`；开着的插件可用 `smelt plugin status <id>` 的 `views` 字段查）；交回 `false` 是没有这个视图。`open-view`、`open-page` 要主窗口开着，没开先 `show-main-window`。

## 设置

```sh
smelt settings get
smelt settings set appearance.theme_mode=dark
smelt settings set appearance.theme_mode=light appearance.ui_font_px=16
```

允许的外观设置：

- `appearance.theme`：主题名，例如 `smelt Dark`、`Ayu Light`；要自配配色可按下面的「写主题插件」新增主题。
- `appearance.theme_mode`：dark|light，把当前主题拨到同一族的另一侧。
- `appearance.ui_font_px`（14–26）、`appearance.ui_font_family`。
- `appearance.font_family`：等宽字体（代码、终端）。终端字号在桌面设置的终端页里调，CLI 不提供。

外观存在桌面客户端里：改动经 smeltd 转给打开着的桌面，立刻生效；桌面没开时 `set` 外观会报错。`settings get` 只列记忆这类存在 smeltd 里的设置。

记忆的向量检索（可选）：

- `memory.embedding_base_url`：OpenAI 兼容的 /embeddings 地址，Ollama 填 `http://localhost:11434/v1`。
- `memory.embedding_model`。
- `memory.embedding_api_key`：本地服务可不填。

填空字符串清除。没配或服务不可用时，召回退回关键词 + 模型重排。召回重排模型用 `memory.rerank_model_provider` + `memory.rerank_model_id`（Pi 里已登录的模型），都空表示自动挑。

## 写 Smelt 插件（NSH TSX/JS）

Smelt 的界面插件是 NSH 上的 TSX/JS：每个用户插件跑在自己的 QuickJS VM 里，界面由 App 原生绘制，保存就热重载。写在 `~/.smelt/plugins/<id>/`。用户问「写 Smelt 插件」说的是这个，不是上面 `smelt pi` 的 Pi 技能和扩展。`smelt plugin` 只在桌面版里有，Linux 不带界面的 `smelt` 没有。

**动手前先读 [references/plugins.md](references/plugins.md)**（清单、NSH 写法红线、`smelt/sdk`、组件/图标/令牌、主题格式）；完整文档在仓库 `docs/plugins/`（<https://github.com/smelt-ai/smelt/tree/main/docs/plugins>）。

```sh
smelt plugin init <id> [--name "<名字>"]           # 起步 TSX 插件（带 locales/ 和 .nsh/types/）
smelt plugin init <id> --theme [--name "<系列名>"]  # 纯主题插件，只改配色时用这个
smelt plugin fork <自带 id> [--as <新 id>]          # 复制自带插件来改
smelt plugin types <目录>                          # 刷新 .nsh/types/（升级 Smelt 后重跑）
smelt plugin check <目录>                          # 离线校验，过了退出 0
smelt plugin status [<id>]                         # 开着的 App 里的加载结果
```

写的流程：

1. 签名查 `<插件目录>/.nsh/types/`（`smelt/sdk.d.ts`、`nsh/index.d.ts`、`nsh/base.d.ts`、`nsh/tokens.d.ts`），按符号名搜，不要猜。
2. 改完跑 `check`，不过不算写完。
3. App 开着时保存一两秒后跑 `status <id>`：`stale: false`、`error: null`、`views` 里有登记的视图才算加载上；`stale: true` 是还没按最新源码重新加载完，稍等再查。
4. 运行时日志和没接住的异常在 `~/.smelt/app.log`（`[plugin]`）。插件没有文件系统接口，数据存 `store.*`。
5. 删插件是删 `~/.smelt/plugins/<id>/`，数据在 `~/.smelt/plugin-data/<id>/`；都先问用户。

改自带插件：

- 对话界面（输入框、消息流、卡片）在共享模块 `smelt.conversation`，不在 `smelt.sessions` 里；要改它 fork `smelt.conversation`。改了共享模块，宿主重编所有用它的插件（谁在用看 `status` 的 `uses_modules`）。
- 不带 `--as` 是同 id 覆写：视图、命令 id 不变，别的插件照常接上。覆写版的数据是从自带版复制出来的一份（`plugin-data/.overrides/<id>/`），各改各的；再 fork 一次会重新复制。覆写版加载失败自动回退自带版，`status` 标 `fallback_to_builtin: true` 和 `error`。删掉覆写目录（或设置 →「面板插件」里「恢复默认…」）就回到自带版。
- `--as` 是改名并存：只改清单 id，源码里写死的 `<自带 id>/…` 要自己搜着改；新 id 是空库；带主题的插件不能 `--as`。
- 覆写版不跟着 Smelt 升级变，升级后对比 fork 结果里 `source_path` 的自带源码。
