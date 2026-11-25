# 视图

**视图**是插件导出的一个组件，登记后宿主可以把它摆进布局、开进出口、放进设置。一个插件可以有多个视图，每个视图的 id 是 `<插件 id>/<name>`。

```tsx
import { Text, View } from "nsh";
import { bootTask, registerViews } from "smelt/sdk";

bootTask(async () => {
  await registerViews([
    { name: "main", export: "Main", title: "Notes", icon: "icons/file.svg" },
  ]);
});

export function Main(props: { args: unknown }) {
  return (
    <View class="flex-1 bg-background p-4">
      <Text class="text-foreground">{JSON.stringify(props.args)}</Text>
    </View>
  );
}
```

## 登记

`registerViews(specs)` 按插件**整组**登记：这次的一组换掉这个插件上次登记的全部视图，传空数组就是全撤下。有一项不合法，整组不登记，原来的不动。单例视图第一次露出来时才建，之后一直用这一份；登记被替换或撤下时，建好的跟着放掉。

### `ViewSpec`

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `name` | `string` | 必填 | 插件内的名字，视图 id 是 `<插件 id>/<name>` |
| `export` | `string` | 同 `name` | 入口导出的组件名 |
| `title` | `string \| Record<string, string>` | 同 `name` | 标签、菜单、设置里显示的名字。要多语言用 [`text("键")`](i18n.md) |
| `icon` | `SmeltIconPath` | `"icons/puzzle.svg"` | 标签和侧栏上的图标，见[图标](ui.md#图标) |
| `draws_top_bar` | `boolean` | `false` | 视图自己画和红绿灯共处的顶栏。见[自己画顶栏](#自己画顶栏) |
| `instances` | `boolean` | `false` | 按参数开多个实例。见[单例和多实例](#单例和多实例) |
| `settings` | `{ group?, keywords? }` | 无 | 同时当成设置里的一页。见[设置页](#设置页) |

## 打开视图

```ts
const opened: boolean = await run("smelt.app/open-view", {
  view: "com.example.notes/main",
  args: { noteId: 42 },
});
```

宿主找有没有哪一页的布局里摆了这个视图：摆了就去那页让它露出来；都没摆就放进出口（主窗口中间那块）。设置页在设置里打开。结果是 `false` 表示没有这个视图，或者哪都放不下。

主窗口要开着。从菜单栏或后台调时，先 `run("smelt.app/show-main-window")`。

## 接收参数

打开时给的 `args` 有两条路到视图：

- **组件的 `props.args`**：响应式的，每次打开都换成新的。
- **`view_opened` 事件**：每次打开都来一次，带 `{ view, args }`。适合「每打开一次就做一件事」，比如滚到某一行。

```tsx
export function Main(props: { args: { noteId?: number } | null }) {
  const noteId = computed(() => props.args?.noteId ?? null);
  return <Text>{`note ${noteId() ?? "none"}`}</Text>;
}

onEvent((event) => {
  if (event.type === "view_opened" && event.view === "com.example.notes/main") {
    focusNote(event.args?.noteId);
  }
}, ["view_opened"]);
```

> [!IMPORTANT]
> 不要在组件参数里解构 `props`（`function Main({ args })`）。NSH 的组件只跑一次，解构拿到的是第一次的值，之后再打开就不更新了。在 JSX、`computed`、`effect` 里读 `props.args`。

参数里认不出的字段直接忽略，不要因为多了字段就报错：调用方可能比你新。

## 单例和多实例

| | 单例（默认） | 多实例（`instances: true`） |
| --- | --- | --- |
| 有几个 | 整个窗口一个 | 每份参数一个 |
| 放在哪 | 用户摆进布局的位置，或出口 | 不进布局。`open-view` 新建一个放进出口，焦点交给它；别的插件用 [`ViewInstance`](#嵌入别的视图) 建了自己摆 |
| 再打开 | 露出来，换上新 `args` | 再建一个新的 |
| 出口换了内容 | 下次打开还是这一份 | 放掉 |
| 重启后 | 跟着布局回来 | 不回来 |
| 能当设置页 | 能 | 不能 |

按参数开的东西（一段对话、一个文件）用多实例；一块常驻面板用单例。

## 视图的状态

| 函数 | 说明 |
| --- | --- |
| `viewShown(id)` | 此刻看得见：在露着的页上、没被挡住或收起。看不见时停掉昂贵的刷新 |
| `viewLaidOut(id)` | 当前布局里摆了它，不管看不看得见 |
| `viewBadge(id)` | 视图名字后面的角标字 |
| `views()` | 全部登记的视图 |

都是响应式的，在 `computed`、`effect`、JSX 里读会跟着变。设角标用宿主命令：

```ts
await run("smelt.app/set-view-badge", { view: `${pluginId()}/main`, label: "3" });
await run("smelt.app/set-view-badge", { view: `${pluginId()}/main`, label: null }); // 清掉
```

## 设置页

给 `settings` 的视图同时出现在设置里，归到 `group` 那一栏：

| `group` | 设置里的位置 |
| --- | --- |
| `"general"` | 通用 |
| `"agents"` | 智能体 |
| `"system"` | 系统 |
| `"plugins"`（默认） | 插件 |

`keywords` 是设置搜索时能匹配到这页的词。插件自己的设置写成一个设置页视图，不要另开窗口。

```ts
await registerViews([
  {
    name: "settings",
    export: "Settings",
    title: text("settings.title"),
    icon: "icons/settings.svg",
    settings: { group: "plugins", keywords: ["counter", "计数"] },
  },
]);
```

## 无头根

导出一个名叫 `root` 的组件，宿主把它挂在一个看不见的层里，插件加载着它就在。主窗口关了它也还在。

```tsx
export function root() {
  const open = signal(false);
  onCommand(({ name }) => {
    if (name === "new-note") open.set(true);
  });
  return (
    <SmeltDialog open={open()} onOpenChange={({ open: next }) => open.set(next)}>
      {/* ... */}
    </SmeltDialog>
  );
}
```

用它放命令弹出的对话框、全局浮层、需要一直跟着某个状态的后台逻辑。不需要界面的后台逻辑也可以直接写在模块顶层或 `bootTask` 里。

## 自己画顶栏

视图摆在窗口最上面时，左上角是红绿灯。默认宿主替视图画一条顶栏。`draws_top_bar: true` 表示视图自己画，这时用 `SmeltWindowBar` 占出红绿灯和拖动区：

```tsx
export function Main() {
  return (
    <View class="flex-1">
      <SmeltWindowBar view={`${pluginId()}/main`} />
      {/* ... */}
    </View>
  );
}
```

`safeArea(viewId)` 给出这个视图四个角要让出的距离和顶栏左右的留白，自己排版时用。

## 嵌入别的视图

`ViewInstance` 把任意一个登记过的视图嵌进你的视图里，包括别的插件的：

```tsx
<ViewInstance view="com.example.notes/editor" args={{ noteId: selected() }} class="flex-1" />
```

| Prop | 类型 | 说明 |
| --- | --- | --- |
| `view` | `string` | 视图 id |
| `args` | `unknown` | 交给它的参数。单例视图：参数变了原地再「打开」一次；多实例视图：参数变了整份重建 |
| `seq` | `number` | 单例视图：给了就每次变化都再「打开」一次，参数没变也算 |
| `reuseKey` | `string` | 只对多实例视图有用。同一帧里这一格卸下、别处用同一个 key 挂上时，接管原来那份实例，不丢输入草稿和滚动位置。分屏重排时用 |
| `focusSeq` | `number` | 变大时把键盘焦点交给它 |
| `onFocusIn` | `() => void` | 焦点进入它或它里面任何地方时 |
| `safeArea` | `object` | 显式给它的安全区；不给按视图 id 从主窗口取 |
| `class`、`style` | | 外层容器的样式 |

别的插件的视图 id 和参数写法以它的契约为准，见[插件之间](plugin-interop.md#契约)。那个插件没装时，这块是空的。

## 渲染出错

视图组件抛错时，那块显示「`<插件 id>` 出错了」，错误写进 `~/.smelt/app.log`。别的视图和插件不受影响。修好保存，热重载后恢复。

## 相关

- [命令与菜单](commands.md)
- [页与布局](pages-and-layouts.md)
- [宿主状态与事件](host-state.md)
