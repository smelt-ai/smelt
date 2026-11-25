# 命令与菜单

**命令**是插件登记的一个具名动作，id 是 `<插件 id>/<name>`。快捷键、命令面板、应用菜单、右键菜单、菜单栏图标、命令行、别的插件，调的都是命令。处理函数只有一个：`onCommand`。

```tsx
import { bootTask, onCommand, registerCommands, run, text } from "smelt/sdk";

bootTask(async () => {
  await registerCommands([
    { name: "new-note", title: text("cmd.new_note"), key: "cmd-shift-n" },
    { name: "open-note", title: "Open Note", requires_args: true, args: ["id"] },
  ]);
});

onCommand(async ({ name, args }) => {
  switch (name) {
    case "new-note":
      return createNote();
    case "open-note":
      return openNote(args.id);
  }
  throw new Error(`unknown command ${name}`);
});
```

## 登记

`registerCommands(specs, groupTitle?)` 和视图一样按插件**整组**登记：换掉这个插件上次登记的全部命令，空数组是全撤下；有一项不合法，整组不登记，原来的不动，`registerCommands` 的 Promise 失败。`groupTitle` 是快捷键设置里这组命令的标题，不给用插件 id。

### `CommandSpec`

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `name` | `string` | 必填 | 插件内的名字，不能有 `/`。命令 id 是 `<插件 id>/<name>` |
| `title` | `string \| Record<string, string>` | 同 `name` | 命令面板和快捷键设置里的名字。要多语言用 [`text("键")`](i18n.md) |
| `key` | `string` | 无 | 出厂快捷键，见[快捷键](#快捷键) |
| `context` | `string` | `"Workspace"` | 快捷键在哪生效，GPUI key context 谓词。`Workspace` 是主窗口任何地方 |
| `palette` | `boolean` | `!requires_args` | 进命令面板 |
| `requires_args` | `boolean` | `false` | 要带参数才能执行。只给代码调：不进命令面板、不能绑键、不在快捷键设置里出现 |
| `args` | `string[]` | 不查 | 认的参数字段（参数对象顶层的键）。空数组是不带参数 |
| `cli` | `boolean` | `false` | 命令行 `smelt app run` 能调。见[从命令行调](#从命令行调) |
| `when_view_shown` | `string` | 无 | 这个视图看得见时才接，否则让给下一条 |
| `when_active_session` | `boolean` | `false` | 有活动会话时才接，否则让给下一条 |

登记会被拒的情况：

- `name` 空或含 `/`，同一组里重名；
- `key` 不是单个按键，或者解析不了；
- `requires_args` 的命令给了 `key` 或开了 `palette`；
- `cli: true` 却没写 `args`（不带参数要写 `args: []`）；
- `context` 不是合法的谓词。

## 处理调用

```ts
onCommand(handler: (call: { name: string; args: any }) => any): void
```

- `name` 是插件内的名字（不带插件 id 前缀）。
- `args` 是调用方给的参数；键位、菜单、命令面板调时是 `null`。
- 返回值（可以是 Promise）交回给调用方，要能转成 JSON。没有值就返回 `undefined`。
- 抛错就是失败，错误的 `message` 是给人看的原因。键位、菜单、命令面板调时失败由宿主报给用户。
- 一个插件只有一个处理函数，后设的换掉先设的。设之前到的调用先排着，设了再处理。

### 参数检查

声明了 `args` 时，调用方给了表外的字段，宿主直接拒掉，不调你的处理函数，错误里列出你认的字段。没声明（不写 `args`）就不查。

字段的类型宿主不查，在处理函数里自己查：

```ts
onCommand(({ name, args }) => {
  if (name === "open-note") {
    if (typeof args?.id !== "number") throw new Error("open-note 要 id（数字）");
    return openNote(args.id);
  }
});
```

### 让给下一条

`when_view_shown` 和 `when_active_session` 不满足时，命令不接这次按键，按键交给下一条绑定，最后落到焦点所在视图自己的按键处理。用 `run` 调时，不满足算失败。

适合「同一个键在不同场合做不同的事」：比如只在你的面板看得见时才接 `cmd-k`。

## 调别的命令

```ts
const result = await run("smelt.app/open-view", { view: "com.example.notes/main" });
```

`run(command, args?)` 调任何一条登记过的命令，包括宿主的 `smelt.app/*` 和别的插件的。命令没登记、不适用、处理函数抛错都是 Promise 失败。宿主命令见[宿主命令参考](reference/host-commands.md)。

`command(id)` 给出一条命令的登记信息，没登记是 `null`；`commands()` 给出全部。都是响应式的，登记变了跟着变。调可选的命令前先看它在不在：

```ts
if (command("smelt.session-list/reveal-in-list")) {
  await run("smelt.session-list/reveal-in-list", { machine, session_id });
}
```

## 快捷键

`key` 用 GPUI 的写法：修饰键用 `-` 连，如 `cmd-shift-n`、`cmd-alt-k`、`f5`。

- 要带 `cmd`，或者是 F 键。不带 `cmd` 的键会抢走终端和输入框的输入。
- 只能是一个按键，不支持连按。
- 这些键留给系统和编辑，不能用：`cmd-c`、`cmd-v`、`cmd-x`、`cmd-a`、`cmd-z`、`cmd-shift-z`、`cmd-f`、`cmd-h`、`cmd-alt-h`、`cmd-m`。
- 和别的命令撞了，后来的那条不绑。

出厂键不合规的，宿主装键表时跳过，记一条日志，登记本身照样成功。用户能在设置里改任何命令的键，改过的按命令 id 存。所以**命令 id 发出去就别改**，改了用户的改键就丢了。

`context` 不是 `Workspace` 时，键只在焦点落在挂了那个 context 的视图里时生效。

## 从命令行调

```ts
{ name: "reset", title: "Reset Counter", cli: true, args: [] }
```

```sh
smelt app commands
smelt app run com.example.counter/reset
smelt app run com.example.notes/open-note --args '{"id": 42}'
```

`cli: true` 只给这样的命令：

- 参数都能写成 JSON，并且声明了 `args`；
- 不要人当场在界面上操作才有结果（不弹选择框等人选）；
- 不替人往对话、终端里打字，也不改配对和凭据。

智能体会照着 `smelt app commands` 列出来的去调，开给命令行就等于开给智能体。

## 应用菜单

macOS 顶部的「Smelt」菜单和「帮助」菜单里可以放命令：

```ts
await registerMenuItems([
  { slot: "app", command: `${pluginId()}/new-note` },
  { slot: "help", title: text("menu.guide"), command: `${pluginId()}/open-guide` },
]);
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `slot` | `"app" \| "help"` | `app` 放在版本号下面、「设置…」上面；`help` 放进「帮助」菜单 |
| `title` | `string \| Record<string, string>` | 菜单上的字，不给用命令 id |
| `command` | `string` | 点了执行的命令，不带参数 |

整组登记。菜单项旁边显示这条命令当前的快捷键。只有 macOS 有菜单栏，别的系统登记了也不显示，入口要另外放。

## 右键菜单

右键菜单归**画它的插件**：它定菜单 id 和点了交给命令的上下文，写在它的[契约](plugin-interop.md#契约)里。别的插件往这个 id 下登记项，画菜单的一方不认识登记者。

### 往别人的菜单里加项

会话列表的项目行菜单 id 是 `smelt.session-list/project`，上下文是 `{ machine, path }`：

```ts
await registerCommands([
  { name: "count-lines", title: "Count Lines", requires_args: true, args: ["machine", "path"] },
]);

await registerContextMenuItems("smelt.session-list/project", [
  { label: text("menu.count_lines"), icon: "icons/file.svg", command: `${pluginId()}/count-lines` },
]);
```

点了以后，命令收到的参数是：登记时给的 `args`，并上菜单的上下文，同名字段以上下文为准。命令声明了 `args` 时只并它认的字段。

### `ContextMenuItemSpec`

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `label` | `string \| Record<string, string>` | 菜单上的字，必填 |
| `icon` | `SmeltIconPath` | 线性图标 |
| `image` | `string` | 彩色图（比如应用自己的图标），有它就不画 `icon` |
| `machine` | `string \| null` | 只在这台机器的菜单里列，`null` 是本机；不写是哪台都列 |
| `command` | `string` | 点了执行的命令 |
| `args` | `object` | 并进参数的固定字段 |
| `submenu` | `ContextMenuItemSpec[]` | 展开一层子菜单，和 `command` 二选一 |

`registerContextMenuItems(menu, items)` 按（插件，菜单）整组登记：再登记就整组替换，空数组是撤下。第一次登记排在这个菜单的最后，之后再登记留在原来的位置。探测完、图标读到了，把整组再登记一次就行。

菜单在打开时取项。已经开着的菜单不跟着变；命令撤下了的项不列，空了的子菜单不列。

### 画自己的菜单，让别人往里加

```ts
const MENU = `${pluginId()}/note`;

async function openNoteMenu(note: Note) {
  const extra = await contextMenuItems(MENU, null, { note_id: note.id });
  // extra 是别的插件登记的、适用于本机的项，接在你自己的项后面
}
```

`contextMenuItems(menu, machine, context)` 按登记顺序给出适用于 `machine` 的项，跳过别的机器的、命令撤下了的、空了的子菜单。每一项是：

```ts
{ label: string; icon: string | null; image: string | null; command: string; args: unknown }
// 或
{ label: string; icon: string | null; image: string | null; submenu: Item[] }
```

`args` 已经并好了上下文，点了直接 `run(item.command, item.args)`。把这些项接在你自己的项后面、隔一道线，用 `SmeltContextMenu` 或 `SmeltMenu` 画出来，见[写界面](ui.md#浮层)。

把菜单 id 和上下文的形状写进你的契约，别人才知道怎么加。

## 菜单栏图标

macOS 菜单栏右上角，每个插件最多一个图标：

```ts
await setStatusMenu({
  title: "3",
  tooltip: "3 notes due",
  items: [
    { type: "action", label: "Open Notes", command: `${pluginId()}/open`, icon: { png: "icons/file.svg", color: "#888888" } },
    { type: "separator" },
    { type: "action", label: "New Note", command: `${pluginId()}/new-note` },
  ],
});

await setStatusMenu(null); // 撤掉
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `icon` | `string \| number[]` | 菜单栏上的图：资源路径或 PNG 字节。只取透明度当形状，颜色跟菜单栏深浅走。不给用 App 图标 |
| `title` | `string` | 图标右边的字，空的只显示图标 |
| `tooltip` | `string` | 鼠标停留时的提示 |
| `items` | `StatusMenuItemSpec[]` | 点开的菜单，从上往下 |

菜单项是 `{ type: "action", label, command, args?, icon? }` 或 `{ type: "separator" }`。项的 `icon` 是 `{ png, color }`：`png` 同上，按透明度涂成 `color`（`#rrggbb`）。点了执行 `command`，失败由宿主报。

和上次一样的菜单宿主什么都不做，但每次调用都要过一趟桥。内容没变就别调，像内置通知插件那样先比一下 JSON 再交。主窗口关了菜单栏图标也照常更新。只有 macOS 有。

## 打开文件

对话里的文件链接、变更视图的「打开文件」都调 `openFile`，由「打开者」决定怎么打开：

```ts
await openFile({ machine: null, path: "/Users/you/project/README.md", line: 12 });
```

想当打开者，登记一条要带参数的命令，再把它的名字交给 `registerFileOpener`：

```ts
await registerCommands([
  { name: "open-file", title: "Open File", requires_args: true, args: ["machine", "path", "line"] },
]);
await registerFileOpener("open-file");
```

命令收到 `{ machine: null | string, path: string, line: number | null }`，`line` 从 1 数。好几个插件都登记时，用最后登记、命令还在的那个。没有打开者时，本机文件交给系统文本编辑器，远程文件失败。

## 相关

- [宿主命令参考](reference/host-commands.md)
- [插件之间](plugin-interop.md)
- [视图](views.md)
