---
name: smelt
description: 用 smelt CLI 操作本机 Smelt：产品智能体、侧栏对话、自动化及其运行记录、MCP 服务器、插件、默认模型、用量和外观设置。用户说新建/修改智能体、开对话、整理对话、建定时任务、看自动化跑得怎样、加 MCP、导入技能、换模型、改主题字体时使用。
---

# Smelt 控制面

通过 `smelt` CLI 操作本机 Smelt。stdout 是 JSON，失败时原因在 stderr、退出码非 0。都要 smeltd 在跑。不要直接改 `~/.smelt/` 下的库和配置文件。`smelt --help` 有完整用法。

先 list 再 update，id 从 list 的结果里取，不要猜。

## 要先问用户的

下面这些动作直接生效，执行前先把要做的事讲清楚、等用户同意：

- 删除：`agent delete`、`automation delete`、`conversation delete`（不可恢复）。
- 全局能力，对所有智能体生效：`mcp add|enable|disable|remove`、`plugin import|remove`、`model default|thinking`。

## 智能体

智能体是长期定义（名称、工作方式、插件、MCP、模型），不是正在跑的对话。

```sh
smelt agent list
smelt agent get <id>
smelt agent create --name "名称" --prompt "长期工作方式" --plugin skill:xxx --mcp figma --model anthropic/claude-opus-5-5
smelt agent update <id> --prompt "新的工作方式"
smelt agent update <id> --memory off
smelt agent update <id> --model default
smelt agent chat <id> --message "第一句话"
smelt agent delete <id>
```

- `--plugin` / `--mcp` / `--folder` / `--link` 可以重复写多个。update 时是整份替换，写 `""` 清空；只改 prompt 时不要带它们。
- 插件 id 从 `smelt plugin list` 取，MCP 服务器名从 `smelt mcp list` 取，模型从 `smelt model list` 取；不存在的会直接报错。智能体只连 `--mcp` 列出的服务器，一个都没列就不连 MCP。
- `--model default` 跟随 Pi 的默认模型。
- `--memory on|off` 开关这个智能体的长期记忆（默认开）。关掉后不注入、不写入，已有记忆文件保留；正在进行的对话下一轮生效。
- `chat` 开一场新对话，出现在桌面「对话」栏。带 `--message` 会把第一句话发出去。返回的 `session_id` 是这场对话的 id，后面的操作都用它，不要拿智能体 id 去改标题。

## 对话

```sh
smelt conversation list [--agent <智能体id>] [--archived]
smelt conversation rename <id> --title "新标题"
smelt conversation archive|unarchive|pin|unpin <id>
smelt conversation delete <id>
```

`<id>` 写 list 里的 `id` 或 `session_id` 都行。默认不列已归档的。

给另一场对话发消息、读它的内容、开同项目的新 Pi 对话，用 Pi 自带的 `session_list` / `session_read` / `session_send` / `session_spawn` 工具，不走 CLI。

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
- `--notify on|off` 控制跑完是否发应用通知，不写就保持原样。
- webhook 令牌不会出现在输出里，要地址让用户去桌面的自动化页面复制。
- 智能体自动化在它的 space（`~/.smelt/agents/<id>/automations/` 下的子目录）里运行，能读到它在对话里攒下的文件和绑定的目录，但不会带上你的项目；要处理的仓库写进 `--prompt`。
- 看自动化跑得怎样：`runs` 列最近的运行（新的在前，默认 20 条），`run-log` 看一次运行的输出、报错和对话记录。

## 能力：MCP、插件、模型、用量

```sh
smelt mcp list
smelt mcp presets
smelt mcp add <预设id>
smelt mcp enable|disable|remove <服务器名>
smelt plugin list
smelt plugin import --skill <技能目录>
smelt plugin import --extension <扩展路径>
smelt plugin remove <id>
smelt model list
smelt model default <provider>/<模型id>
smelt model thinking off|minimal|low|medium|high|xhigh
smelt quota
```

- `mcp add` 只能加预设。结果里有 `next` 时（要登录、要装浏览器扩展），照原样转告用户去做；登录要开浏览器，CLI 代办不了。
- 自定义 MCP 服务器、登录模型账号、填 API key 都在桌面设置的能力库里做，CLI 不提供。
- `plugin import` 的相对路径按当前目录算。
- `model default` 只能挑 `model list` 里有的模型，推理强度保持原样。
- `quota` 看各模型账号的用量。

## 设置

```sh
smelt settings get
smelt settings set appearance.theme_mode=dark
smelt settings set appearance.theme_mode=light appearance.ui_font_px=16 appearance.font_px=13
```

允许的外观设置：

- `appearance.theme`：主题名，例如 `Default Dark`、`Ayu Light`。
- `appearance.theme_mode`：dark|light，把当前主题拨到同一族的另一侧。
- `appearance.ui_font_px`（14–26）、`appearance.ui_font_family`。
- `appearance.font_px`（9–22）、`appearance.font_family`。
- `appearance.opacity`（0.3–1.0）。

外观存在桌面客户端里：改动经 smeltd 转给打开着的桌面，立刻生效；桌面没开时 `set` 外观会报错。`settings get` 只列记忆这类存在 smeltd 里的设置。

记忆的向量检索（可选）：

- `memory.embedding_base_url`：OpenAI 兼容的 /embeddings 地址，Ollama 填 `http://localhost:11434/v1`。
- `memory.embedding_model`。
- `memory.embedding_api_key`：本地服务可不填。

填空字符串清除。没配或服务不可用时，召回退回关键词 + 模型重排。召回重排模型用 `memory.rerank_model_provider` + `memory.rerank_model_id`（Pi 里已登录的模型），都空表示自动挑。
