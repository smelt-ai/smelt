# 宿主命令

宿主做的事也是命令，id 以 `smelt.app/` 开头，和插件的命令一样用 `run` 调：

```ts
await run("smelt.app/notify", { subtitle: "Build finished", body: "All tests passed" });
```

参数里的 `machine` 一律是 `null`（本机）或远程机器 id。参数缺字段、类型不对，`run` 失败。命令定义在 `crates/smelt-plugin-api/src/`（`lib.rs`、`view.rs`、`page.rs`、`connections.rs`、`quit.rs`）和 `crates/smelt/src/workspace_commands.rs`。

## 窗口与导航

| 命令 | 参数 | 结果 | 说明 |
| --- | --- | --- | --- |
| `smelt.app/show-main-window` | 无 | | 把主窗口拿到前面；关了就新开一扇。**命令行可调** |
| `smelt.app/open-view` | `{ view, args? }` | `boolean` | 打开视图，见[视图：打开视图](../views.md#打开视图)。没登记、放不下给 `false`。**命令行可调** |
| `smelt.app/open-page` | `{ page }` | `boolean` | 进一页，见[页与布局](../pages-and-layouts.md)。没有这页、这页没有布局给 `false`。**命令行可调** |
| `smelt.app/set-view-badge` | `{ view, label }` | | 设视图入口上的角标，`label: null` 清掉 |
| `smelt.app/settings` | 无 | | 打开设置。快捷键 ⌘, |
| `smelt.app/quit` | 无 | | 退出，先过退出确认。快捷键 ⌘Q |
| `smelt.app/quit-now` | 无 | | 直接退出，不确认 |
| `smelt.app/debug-hud` | 无 | | 开关性能浮层（帧耗时、CPU、内存）。快捷键 ⌘⇧F，不进命令面板 |

## 通知与角标

| 命令 | 参数 | 说明 |
| --- | --- | --- |
| `smelt.app/notify` | `{ id?, subtitle, body, on_click? }` | 发系统通知，见下 |
| `smelt.app/withdraw-notification` | `{ id }` | 撤回给了 `id` 的通知；没有也不算错 |
| `smelt.app/set-dock-badge` | `{ label }` | Dock 图标上的角标，`label: null` 清掉。只有 macOS 有 |
| `smelt.app/request-notification-permission` | 无 | 还没问过用户时请系统问一次。结果从 `notification_permission_changed` 事件来 |
| `smelt.app/open-notification-settings` | 无 | 打开系统设置里 Smelt 的通知设置。只有 macOS 有 |

`notify` 的参数：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `id` | `string?` | 给了 id 的通知由插件管：同 id 再发换掉上一条，能撤回，重启后也认得。以 `<插件 id>/` 开头。不给是一次性提示 |
| `subtitle` | `string` | 标题下面那行 |
| `body` | `string` | 正文 |
| `on_click` | `{ command, args? }?` | 点通知时执行的命令，重启后点也照样执行。不给就只是把 App 拿到前面 |

显不显示由系统的通知权限决定，见[宿主状态：通知授权](../host-state.md#通知授权)。界面里能就地报的失败不要发通知，见[写界面：文案](../ui.md#文案)。

## 文件与路径

| 命令 | 参数 | 结果 | 说明 |
| --- | --- | --- | --- |
| `smelt.app/pick-paths` | `{ machine, title, confirm, files?, multiple? }` | `string[] \| null` | 在某台机器上选路径。本机弹系统选择框；远程机器由宿主画一个浏览那台机器的选择框。`files` 为真时文件也能选，`multiple` 为真时能多选。取消给 `null` |
| `smelt.app/open-in-text-editor` | `{ path }` | | 用系统文本编辑器打开本机文件 |
| `smelt.app/open-in-file-manager` | `{ path }` | | 用访达打开本机目录 |

`title` 是选择框的标题（「选择要导入的笔记」），`confirm` 是确认按钮上的动词（「导入」）。

## 远程机器

| 命令 | 参数 | 结果 | 说明 |
| --- | --- | --- | --- |
| `smelt.app/add-machine` | `{ pairing_uri }` | `{ machine, name }` | 用配对串（`smelt+iroh://…`）加一台远程机器。走公网拨号，最长等半分钟 |
| `smelt.app/remove-machine` | `{ machine }` | | 移除远程机器，它的项目和会话从这边移走；那边的会话照常跑 |
| `smelt.app/rename-machine` | `{ machine, name }` | | 改远程机器在这台电脑上的显示名 |
| `smelt.app/reconnect-machine` | `{ machine }` | | 马上重连，不等退避 |
| `smelt.app/upgrade-machine` | `{ machine }` | | 请那台机器把运行时升到这个桌面的版本。进度看 `connections()` |
| `smelt.app/set-machine-relay` | `{ machine, relay }` | | 改连它用的中继。`relay` 空串是公共中继 |
| `smelt.app/set-machine-access` | `{ machine, agents?, inbound? }` | | 改它的权限，只改给了的。`agents`：这台电脑上的智能体能不能经本机运行时用它；`inbound`：它能不能连进来操作这台电脑（不能时只能回这边发过去的消息）。`connections()` 里这两项是 `null` 的机器还没迁到本机运行时，改不了 |
| `smelt.app/refresh-connections` | 无 | | 重读各台机器的连接情况。看连接状态的界面露出来时调一次，**不要定时调** |

机器的状态从 `machineMeta()`、`connections()` 读，见[宿主状态：机器](../host-state.md#机器)。

## 命令行

标了**命令行可调**的三条能用 `smelt app run` 从终端调，见[命令行](../cli.md#smelt-app)。
