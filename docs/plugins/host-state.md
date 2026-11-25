# 宿主状态与事件

宿主把它知道的东西镜像给插件：有哪些机器、各台的会话、界面语言、主题、登记了哪些视图和命令、哪个视图看得见。插件用 SDK 的读取函数拿，这些函数是**响应式**的：在 JSX、`computed`、`effect` 里读，变了自动跟着变。

```tsx
import { Each, Text, View } from "nsh";
import { conversationFacts, machines } from "smelt/sdk";

export function Main() {
  return (
    <View class="gap-1 p-3">
      <Each of={machines()} key={(m) => String(m.machine)}>
        {(m) => (
          <Text class="text-foreground">
            {`${m().name}: ${conversationFacts(m().machine).length} sessions`}
          </Text>
        )}
      </Each>
    </View>
  );
}
```

状态在内存里，读它不过桥、不做 IO，在渲染里随便读。

## 切片

宿主的状态分成几块**切片**。某块第一次被读时 SDK 才向宿主订它，之后它变了宿主就推新的过来。没订的切片宿主不算、不推。

| 切片 | 读取函数 | 内容 | 什么时候变 |
| --- | --- | --- | --- |
| `locale` | `locale()`、`t()` | 当前界面语言，如 `"zh-CN"` | 换语言 |
| `system_locale` | `systemLocale()` | 系统语言 | 换语言 |
| `locales` | `locales()` | App 支持的语言 `{ code, name }[]` | 不变 |
| `theme` | `theme()` | 当前主题：色板、字号、圆角，见[写界面](ui.md#主题) | 换主题、换深浅色、窗口缩放 |
| `machines_meta` | `machineMeta()`、`machineMetaEntry(m)`、`pathStyle(m)` | 挂着的机器，见下 | 加、删、改名、上下线 |
| `sessions_data` | `machines()`、`machineEntry(m)`、`conversationFacts(m)`、`sessionStates(m)`、`workspaceDirectory(m)`、`sessionAttention(m)` | 各台机器的会话，见下 | 会话、状态、提醒、目录变了 |
| `connections` | `connections()` | 远程机器的连接表 | 机器变了 |
| `active_context` | `activeContext()` | 当前项目和露着的会话，见下 | 切项目、切会话 |
| `window_active` | `windowActive()` | 主窗口在不在前台 | 窗口切前后台 |
| `notification_permission` | `notificationPermission()` | 系统通知授权，见下 | 授权变了 |
| `views` | `views()`、`viewShown(id)`、`viewLaidOut(id)`、`viewBadge(id)` | 登记的视图 | 视图登记、可见性、角标、布局变了 |
| `pages` | `pages()` | 登记的页 `{ id, title, home, index }[]` | 页登记变了、换语言 |
| `commands` | `commands()`、`command(id)` | 登记的命令 `{ id, title, key }[]` | 命令登记变了、换语言 |
| `safe_areas` | `safeArea(viewId)` | 各视图四角要让出的距离 | 布局、窗口变了 |

`initHost()` 总会先拉 `locale` 和 `theme`。别的切片想在 `bootTask` 里就有数据，就在 `initHost` 里点名，第一次一起拉：

```ts
bootTask(async () => {
  await initHost(["machines_meta", "sessions_data"]);
  // 这里 machines() 已经有数据
});
```

不点名也行：第一次读时订上，数据到了读取函数再变一次。在界面里读的，一般不用点名。

> [!NOTE]
> `shell`、`js_views` 两块是主窗口外壳自己用的，插件不要依赖。

### 机器

`machine` 在 SDK 里一律是 `null`（本机）或远程机器的 id 字符串。

`machineMeta()` 的每一项：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `machine` | `string \| null` | 机器 |
| `name` | `string` | 给人看的名字 |
| `online` | `boolean` | 此刻连得上、版本对得上。本机守护重启、升级的那一会儿也是 `false` |
| `unavailable` | `string \| null` | 不能用的原因（比如版本对不上），能用是 `null` |
| `path_style` | `"posix" \| "windows"` | 这台机器的路径写法，处理它的路径要用它，见[路径](reference/sdk.md#路径) |

本机永远在最前。

`machines()` 是 `machineMeta()` 每一项再并上这台的会话数据（`directory`、`states`、`facts`、`attention`）。

### 会话

大多数插件只要 `conversationFacts(machine)`，它是宿主替你整理好的会话列表：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `id` | `string` | 会话 id |
| `name` | `string` | 显示名 |
| `status` | `"needs_you" \| "running" \| "idle"` | 等人处理、在跑、闲着 |
| `agent` | `{ id, label, icon } \| null` | 用的智能体，`icon` 是图标资源路径 |
| `automation` | `boolean` | 是不是自动化跑出来的 |
| `cwd` | `string \| null` | 工作目录 |
| `placement` | `object` | 在会话列表里放哪：`{ kind: "project", root, workset }`、`{ kind: "conversations" }` 或 `{ kind: "hidden" }` |

`sessionAttention(machine)` 是各会话当前的提醒：

```ts
interface SessionAttention {
  session_id: string;
  action?: AttentionItem; // 要人处理的：批准、输入
  unread?: AttentionItem; // 没看的：完成、失败、响铃
}
interface AttentionItem {
  kind: string; // approval、input、success、failure、bell、notice，或比这个版本新的种类
  title: string;
  message: string;
}
```

`kind` 认不出的值当成一般提醒处理，不要报错：运行时可能比 App 新。

`workspaceDirectory(machine)` 和 `sessionStates(machine)` 是运行时原样的数据，形状跟着运行时协议走，会比上面的整理版变得多。能用 `conversationFacts` 就用它。

### 活动上下文

```ts
activeContext(): {
  project: { machine: string | null; root: string } | null;
  session: { machine: string | null; id: string; at_latest: boolean } | null;
}
```

用户眼下在哪个项目、看着哪段会话。`at_latest` 是会话滚在最底下。想跟着用户当前的项目走（文件树、Git 面板），读它。

它由显示会话的那个插件发布（`setActiveContext`），别的插件只读。

### 通知授权

`notificationPermission()` 是下面之一：

| `kind` | 意思 |
| --- | --- |
| `"checking"` | 还在查 |
| `"not_requested"` | 没请求过，可以 `run("smelt.app/request-notification-permission")` |
| `"granted"` | 给了，带 `banners`、`sound`、`badge` 三个开关 |
| `"denied"` | 拒了，只能引导去系统设置 `run("smelt.app/open-notification-settings")` |
| `"unavailable"` | 暂时不能用，带 `reason` |
| `"unsupported"` | 这个平台没有 |

## 订切片变化

响应式读取已经够用时，不需要它。要在**非响应式**的代码里对变化做事（比如变了就写库、重建缓存），用 `onSlices`：

```ts
const off = onSlices(["sessions_data"], (changed) => {
  rebuildIndex(machines());
});
```

回调拿到这一次变了的那几块切片名。返回函数用来取消。

## 宿主事件

有的变化不是状态，是一件「发生了的事」：重新连上了、项目里文件变了、有人拖了文件到 Dock 图标上。用 `onEvent` 收：

```ts
const off = onEvent(
  (event) => {
    if (event.type === "project_changed") reloadTree(event.machine, event.root);
    if (event.type === "reconnected") resubscribe(event.machine);
  },
  ["project_changed", "reconnected"],
);
```

第二个参数是要收的事件类型，**必填**。宿主只把你点名的类型发给你的插件，没人要的事件不过桥。

| 类型 | 字段 | 什么时候来 | 该做什么 |
| --- | --- | --- | --- |
| `reconnected` | `machine` | 重新连上一台机器 | 断开期间错过的要重读；替你建的监听（如 `project.watch`）要重新登记 |
| `project_changed` | `machine`、`root` | `project.watch` 过的项目里有文件变了，一批合成一次 | 重读。分不出是哪个文件 |
| `window_activated` | | 主窗口回到前台 | 监听可能漏了的地方补查一次 |
| `open_paths` | `machine`、`paths` | 拖到 Dock 图标上、Finder 里「用 Smelt 打开」 | 认得就打开。路径是那台机器上的绝对路径，可能是目录也可能是文件 |
| `view_opened` | `view`、`args` | 有人打开了你的视图，只发给视图的主人 | 见[视图](views.md#接收参数) |
| `attention_alert` | `machine`、`session`、`alert`、`title`、`message` | 一段会话刚来了一条该响一下的提醒。同一段会话同样的提醒 60 秒内只来一次；重连时的快照不补发 | 发系统通知、推送。`title`、`message` 已经按当前语言写好 |
| `runtime_topic` | `machine`、`topic`、`message` | 你登记的运行时话题推来了东西 | 见[调用运行时](runtime.md#运行时话题) |
| `plugin` | `name`、`payload` | 有插件发了事件 | 见下 |
| `sessions_changed`、`machines_changed`、`active_context_changed`、`views_changed`、`view_badges_changed`、`shown_views_changed`、`laid_out_views_changed`、`pages_changed`、`layouts_changed`、`commands_changed`、`context_menu_items_changed`、`locale_changed`、`safe_area_changed`、`themes_changed`、`overlay_changed`、`notification_permission_changed` | | 对应的东西变了 | 重读。大多数已经反映在切片里，用读取函数就行 |

先推切片再发事件：收到 `sessions_changed`、`attention_alert` 时，`machines()` 读到的已经是新的。

### 在组件里订

组件卸下时要取消，用 NSH 的 `cleanup`：

```tsx
export function Main() {
  const changed = signal(0);
  const off = onEvent(() => changed.update((n) => n + 1), ["project_changed"]);
  cleanup(off);
  // ...
}
```

模块顶层订的，跟着插件 VM 走，不用取消。

## 插件间广播

```ts
await emitEvent(`${pluginId()}/note-saved`, { id: 42 });
```

事件名必须以 `<你的插件 id>/` 开头，载荷是能转成 JSON 的值。收的一方用 `plugin:<事件名>` 点名收某一个，或者用 `plugin` 收全部插件事件：

```ts
onEvent((event) => {
  if (event.type === "plugin") refresh(event.payload);
}, ["plugin:com.example.notes/note-saved"]);
```

事件名和载荷形状写进你的[契约](plugin-interop.md#契约)。广播没有回执，也不排队：发的时候没在听的插件收不到。要结果就用命令。

## 别轮询

状态和事件已经覆盖了的东西，不要用 `setInterval` 去反复问。宿主的事件是精确的，轮询只会白白耗电、拖慢整个窗口。见[写界面：常见错误](ui.md#常见错误)。

## 相关

- [调用运行时](runtime.md)
- [SDK 参考](reference/sdk.md)
