---
name: smelt
description: 用 smelt CLI 管理本机 Smelt 产品智能体、自动化和外观。用户说新建/修改智能体、建定时任务、改主题字体时使用。
---

# Smelt 控制面

通过 `smelt` CLI 操作本机产品对象。stdout 是 JSON。不要改 `~/.smelt/smelt.sqlite3`。

智能体是长期定义（名称、工作方式、插件），不是正在跑的会话。自动化引用智能体 id，单次 `prompt` 不是定义里的工作方式。

```sh
smelt agent list
smelt agent plugins
smelt agent create --name "名称" --prompt "长期工作方式" --plugin skill:xxx
smelt agent update <id> --prompt "新的工作方式"
smelt agent get <id>
smelt agent chat <id>
smelt agent delete <id>
```

`--plugin` / `--folder` / `--link` 在 update 时是整份替换。只改 prompt 时不要带它们。`chat` 需要 smeltd 在跑，对话会出现在桌面「对话」栏。

```sh
smelt automation list
smelt automation create --name "名称" --agent <智能体id> --every-hours 1 --prompt "这一次要做什么"
smelt automation create --name "名称" --agent <智能体id> --daily 09:30
smelt automation create --name "名称" --agent <智能体id> --webhook
smelt automation create --name "名称" --agent <智能体id> --event topic.name
smelt automation update <id> --prompt "新的单次输入"
smelt automation enable|disable|run|delete <id>
```

自动化写操作需要 Smelt/smeltd 在跑。webhook 令牌不会出现在 list 里。

```sh
smelt settings get
smelt settings set appearance.theme_mode=dark
smelt settings set appearance.theme_mode=light appearance.ui_font_px=16 appearance.font_px=13
```

允许的设置：`appearance.theme_mode`（dark|light）、`appearance.ui_font_px`（14–26）、`appearance.ui_font_family`、`appearance.font_px`（9–22）、`appearance.font_family`、`appearance.opacity`（0.3–1.0）。改完后用户打开设置页会套上；正在看的窗口可能要再点一次设置。

先 list 再 update。不要和 `smelt_list_agents`（跨会话发消息）搞混。
