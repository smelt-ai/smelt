# 插件之间

插件之间不直接 import 对方的代码，也不共享变量。它们经宿主打交道：

| 想做 | 用 |
| --- | --- |
| 叫别的插件做件事、拿结果 | [命令](commands.md)：`run(id, args)` |
| 在别的插件的界面里放自己的视图 | [`ViewInstance`](views.md#嵌入别的视图) 或打开它：`smelt.app/open-view` |
| 往别的插件的右键菜单里加项 | [右键菜单](commands.md#右键菜单) |
| 告诉大家发生了什么 | [广播](host-state.md#插件间广播)：`emitEvent` |
| 复用一套组件、函数 | 共享模块 |

这些 id 和参数形状就是插件对外的约定，写在它的契约里。

## 契约

内置插件把对外的 id 和参数写在自己目录的 `contract.js` 里，比如 [`plugins/session-list/contract.js`](../../plugins/session-list/contract.js)：

```js
/** 项目行的右键菜单。上下文 `{ machine, path }`（path 是项目根目录）。 */
export const PROJECT_MENU = `${OWNER}/project`;
```

用户插件 import 不到别的插件目录里的文件，**照抄 id 和形状**：

```ts
// 我的插件往会话列表的项目菜单里加一项
await registerContextMenuItems("smelt.session-list/project", [
  { label: text("menu.notes"), icon: "icons/file.svg", command: "com.example.notes/open-project-notes" },
]);
```

自己的插件要给别人用，也写一份：

- 视图、命令、菜单、事件的 id，每个 id 收什么参数、给什么结果；
- 放在插件根目录的 `contract.js`，每条带注释；
- 写进去的就当已经发布：**改名、改参数就是破坏别人的插件**，用户绑的快捷键、存的布局也认 id。真要改，见[版本与兼容](compatibility.md)。

内置插件的契约：

| 插件 | 契约 |
| --- | --- |
| 会话列表 | [`plugins/session-list/contract.js`](../../plugins/session-list/contract.js) |
| 会话 | [`plugins/sessions/contract.js`](../../plugins/sessions/contract.js) |
| Git | [`plugins/git/contract.js`](../../plugins/git/contract.js) |
| 智能体 | [`plugins/agents/contract.js`](../../plugins/agents/contract.js) |
| 自动化 | [`plugins/automations/contract.js`](../../plugins/automations/contract.js) |
| 能力库 | [`plugins/capabilities/contract.js`](../../plugins/capabilities/contract.js) |
| 机器 | [`plugins/machines/contract.js`](../../plugins/machines/contract.js) |
| 通知 | [`plugins/notifications/contract.js`](../../plugins/notifications/contract.js) |
| 存档 | [`plugins/archive/contract.js`](../../plugins/archive/contract.js) |
| 桌面 | [`plugins/desktop/contract.js`](../../plugins/desktop/contract.js) |
| 启动器 | [`plugins/launcher/contract.js`](../../plugins/launcher/contract.js) |
| 额度 | [`plugins/quota/contract.js`](../../plugins/quota/contract.js) |

宿主自己的命令见[宿主命令](reference/host-commands.md)。

## 调别的插件的命令

```ts
const reply = await run("smelt.session-list/reveal-in-list", { machine, session_id });
```

- 结果是对方 `onCommand` 的返回值；对方抛错，`run` 失败。
- 对方插件没装、被删、正在重载，`run` 失败。先用 `command(id)` 看有没有这条命令（没有给 `null`），能优雅降级的就降级。
- 别的插件可能晚于你加载。启动时要用的命令，在 `bootTask` 里 `await` 之前先查 `command(id)`，没有就等 `commands_changed` 事件。

## 共享模块

想让几个插件用同一套组件或函数，做一个**共享模块**插件：清单里加 `module`，值是别人 import 用的名字。

```json
{
  "id": "com.example.widgets",
  "name": "Example Widgets",
  "module": "example/widgets",
  "entry": "index.tsx",
  "types": "index.d.ts"
}
```

```ts
// 别的插件里
import { Sparkline } from "example/widgets";
```

- `module` 至少两段，用 `/` 分开，每段的字符规则和插件 id 一样。`nsh`、`nsh/*`、`smelt/sdk`、`smelt/host`、`smelt/slots` 是保留的。两个插件声明同一个 `module` 算错。
- `types` 是给编辑器用的类型声明，`smelt plugin types` 会把找到的共享模块的声明都生成出来。
- 共享模块不自己跑：它被**打包进每个 import 它的插件**，在那个插件的 VM 里执行。所以模块里的顶层变量不在插件之间共享，每个插件各有一份。要共享状态，用命令或广播。
- 模块里可以 import `smelt/sdk`，调用算作 import 它的那个插件发的。`t()` 查的也是那个插件的 `locales/`，所以模块的文案要自己带：按 `locale()` 从模块里的一张表取，像 [`plugins/conversation/i18n.ts`](../../plugins/conversation/i18n.ts) 那样。
- 改了模块，宿主重编所有 import 它的插件。模块编不过，import 它的插件跟着失败；内置模块被覆写坏了，回退到内置版。

内置的共享模块有 `smelt/conversation`（对话界面）。它跟着内置插件变，没有单独的版本约定。

## 覆写和 fork 内置插件

内置插件的源码就在 App 里，可以复制出来改：

```sh
smelt plugin fork smelt.quota                         # 同 id：覆写内置的
smelt plugin fork smelt.quota --as com.me.quota       # 新 id：和内置的并存
```

### 覆写（同 id）

用户插件和内置插件 id 相同，用户那份顶替内置的：

- 视图 id、命令 id 都不变，布局、快捷键、别的插件对它的调用照常工作。
- **用的是同一个库**（`~/.smelt/plugin-data/<id>/`）。覆写版改了表结构，再切回内置版可能读不了。改表之前先[备份](storage.md#数据跟着插件-id-走)。
- 覆写版加载失败（编不过、启动报错），宿主自动回退到内置版，插件管理页上标出「覆写版加载失败，已自动回退到自带版本」。修好保存，宿主再试覆写版。
- 内置插件升级了，覆写版不会跟着变。升级 Smelt 后对比一下内置版的改动。

恢复内置版：在「设置 → 面板插件」里对它选「恢复默认…」，覆写目录挪进 `~/.smelt/trash/plugins/`；或者直接删掉覆写目录。

### 改名 fork（`--as`）

新 id 的插件和内置的并存，两份都在跑：

- 视图、命令 id 都换了前缀，用户要重新摆布局、绑快捷键。
- 新 id 是一个新的空库。
- 别的插件按内置 id 调它的命令，调到的还是内置版。

想改内置插件的行为又要别的东西照常接上，用覆写；想做一个独立的新插件，从 fork 起步改名。

## 相关

- [命令](commands.md)
- [宿主状态与事件：插件间广播](host-state.md#插件间广播)
- [版本与兼容](compatibility.md)
