# Smelt NSH 插件开发参考

本文档说明如何为 Smelt（NSH A2 运行时）编写、调试并验证 TSX/JS 插件。

## 1. 目录结构与清单（`smelt-plugin.json`）

一个插件就是一个目录（用户插件位于 `~/.smelt/plugins/<id>/`）：

```
com.example.demo/
  smelt-plugin.json   # 必填：id、name、version、entry
  nsh.tsx             # TSX/JS 入口（也可命名为 main.tsx 等，与 entry 一致）
  locales/            # 可选：ui.yml / en.yml / zh-CN.yml，供 t() 和 text() 使用
  themes/             # 可选：*.json 主题文件（Smelt 主题格式）
  tsconfig.json       # 由 smelt plugin init / types 生成
  .nsh/               # 生成的 TS 声明与 tsconfig（热重载监听与导入自动忽略点目录）
```

清单字段：

```json
{
  "id": "com.example.demo",
  "name": "Demo",
  "version": "1.0.0",
  "entry": "nsh.tsx"
}
```

- `id`：只能包含 ASCII 字母、数字、`.`、`-`、`_`，且不能以 `.` 开头。同来源内（用户目录下两份、或自带目录下两份）不可重名；用户目录若与自带插件同 `id` 则构成**原地覆写**（用户版编译/主题/启动失败时自动回退到自带版本，`smelt plugin status` 可查看 `overrides_builtin` 与 `fallback_to_builtin`；用 `smelt plugin fork <自带id>` 可一键拷贝自带源码到用户目录）。
- `entry`：插件目录内的相对路径。若以 `.json` 结尾（如 `"themes/my-theme.json"`），则为**纯主题插件**，只加载主题 JSON，不启动 JS VM。
- `pages` / `layouts`（可选，可替代 `entry`）：声明该插件为**零 VM 纯布局插件**（如内置 `smelt.layouts`），直接通过清单内 `pages` 与 `layouts[].state` 引用静态布局 JSON 文件与 `locales/*.yml` 文案（`"pages": [{ "name": "workbench", "title": "workbench.title", "home": true, "index": "smelt.sessions/session" }]`，`"layouts": [{ "page": "smelt.layouts/workbench", "name": "standard", "title": "standard.title", "state": "standard.json" }]`），无需 JS 入口与 QuickJS VM。
- `module`（可选）：声明该插件为**共享 JS 模块插件**（至少两段 `/` 分隔且不占用内置保留名，如 `"smelt/conversation"`），不独立起 VM 与注册视图，而是供其他插件通过 `import { ... } from "<module>"` 直接内联打包。配套可选填 `types`（插件目录内的 `.d.ts` 相对路径，如 `"index.d.ts"`），`smelt plugin types` 会生成对应声明到 `.nsh/types/<module>.d.ts`。保存或删改用户覆写模块时，宿主会自动重新打包加载并增量重编依赖它的插件（出错自动回退自带模块并重载恢复）。
- 若使用相对模块（如 `./model.ts`、`./store.js`），必须留在当前插件目录内，禁止 `../` 越出目录、禁止导入静态资源图片文件（`import logo from "./logo.png"` 会被打包器拒绝）。

## 2. 隔离模型、可导入模块与能力边界

未覆写的内置脚本插件共驻 1 个共享内置 VM（通过作用域 SDK 隔离 `pluginId`、`dataDir`、`texts`、`store`、`onCommand` 与视图边界），每个用户插件（含同名覆写内置与新增插件）则运行在各自独立的加固 QuickJS VM（`harden: true`）中，拥有独立的堆与 CPU 预算和不可伪造的宿主通道：

- **可导入的模块**：
  - `"nsh"`：核心响应式原语（`signal`, `computed`, `effect`, `batch`, `untracked`, `cleanup`, `resource`, `context`, `split`, `ErrorBoundary`）与基础节点（`View`, `Text`, `Scroll`, `List`, `Overlay`, `Image`, `Input`, `Canvas`, `Shader`, ...）。
  - `"nsh/base"`：桌面控件（`Button`, `Dialog`, `Popover`, `Select`, `Switch`, `Checkbox`, `Radio`, `Input`, `Tabs`, `Tooltip`, `Editor`, `DiffView`, `TextView`, `Tree`, `DataTable` 等，完整清单看 `.nsh/types/nsh/base.d.ts`）。
  - `"nsh/desktop"`：桌面原语（如 `Menu`, `Window` 相关类型，见 `.nsh/types/nsh/desktop.d.ts`）。
  - `"nsh/i18n"`：国际化工具（常规插件优先直接用 `smelt/sdk` 的 `t`/`text`）。
  - `"smelt/sdk"`：Smelt 宿主桥、响应式状态、SQLite 存储、控制 RPC 与 Smelt 主题化组件。
  - `"smelt/host"`：底层纯同步 Rust 原语（`<Icon>`, `<Vector>`, `nativeModule("smelt/host")`，常规功能已被 `smelt/sdk` 包装）。
  - **共享 JS 模块插件**（如内置 `smelt.conversation` 提供的 `"smelt/conversation"`，导出 `<SmeltConversation>`）：可通过 `smelt plugin fork smelt.conversation` 在本地覆写完整对话界面（输入框、消息列表、卡片等）。
  - 同插件目录内的 `./*.ts` / `./*.tsx` / `./*.js` / `./*.json` 相对模块。
- **禁止导入 / 不可用**：
  - 禁止导入 `"smelt/slots"`（主壳专用，Guest 插件不得嵌套其它插件）。
  - **未向 Guest 插件开放** Node/标准库裸模块（`fs/promises`, `os`, `process`, `net`, `buffer`, `crypto`, `path`, `url`, `websocket`, `zlib` 等均不在插件 import 白名单内），也不提供全局 `Buffer` 或 `process`。
  - 不支持下载网络图片到界面上直接加载外部 `assets/`（图标用内建 `icons/*.svg`、`smelt-icons/*.svg` 或内联 SVG `<SmeltVector>`）。
- **权限说明**：虽然 VM 本身加固（冻结原型、独立堆/CPU 配额），但插件通过 `smelt/sdk` 宿主桥能发起 `call(...)`、`run(...)`、`os.launch_path(...)`、`openUrl(...)` 等拥有与宿主同等能力的调用，只安装/运行可信代码。

## 3. NSH 响应式 JSX 写法与红线

NSH **不是 React**：组件函数只在挂载时执行**一次**，之后只有读取 signal/accessor 的地方细粒度更新。

1. **禁止在组件函数顶层解构 `props`**：
   - 错误：`function Card({ title }: { title: string }) { return <Text>{title}</Text>; }`
   - 正确：`function Card(props: { title: string }) { return <Text>{props.title}</Text>; }`
   - 如果视图参数通过 `props.args` 传入，`props.args` 是由外层 accessor 传入的，在 JSX 属性、文本插值、`computed(() => ...)`、`effect(() => ...)` 中通过 `props.args` 读取才会随 `args` 变更热更新。
2. **所有可见文字必须包在 `<Text>` 里**：`<View>hello</View>` 不合法或不可见，必须 `<View><Text>hello</Text></View>`。
3. **条件与列表使用全局 `<If>` / `<Each>`（无需 import）或函数子节点**：
   - `<If when={ready()} else={<Text>Loading</Text>}>{(val) => <Text>{val()}</Text>}</If>`
   - `<Each of={items()} key={(item) => item.id}>{(item, idx) => <Text>{item().name}</Text>}</Each>`
   - 大列表用 `"nsh"` 的 `<List>`（虚拟滚动）。
4. **副作用与清理**：`effect(() => { const off = onEvent(..., ["view_opened"]); cleanup(off); })`，或配合 `onSlices(["views"], ...)` 按需监听状态切片。不要设定时轮询；等待宿主或运行时变化一律监听事件（`onEvent` 须显式传第二个参数 `types` 数组 / `onSlices` / `waitOnline`）。要逐帧推进的（拖到边上自动滚动这类）用 `requestAnimationFrame`，按两帧时间差算步长，停下就别再要帧。
5. **顶层不可阻塞主渲染**：文件/数据库/RPC 都异步放到 `bootTask` 或事件回调里，界面直接读内存 signal。
6. **主题样式不要硬编码颜色**：尽量使用语义 Tailwind class（见第 6 节）或读取 `theme()` 切片信号（`theme().palette`、`theme().radius`、`theme().type`）。

## 4. 生命周期与宿主注册

### 启动初始化与视图导出

- 使用 `bootTask(async () => { await initHost(); ... })` 注册启动任务：宿主会等待所有 `bootTask`（最多 3 秒）后再发 `smelt.plugin.ready` 并重放工作台布局中的视图插槽。
- 导出组件函数并在 `registerViews` 中声明对应名称：
  - `name`：插件内视图名；完整视图 id 为 `<插件 id>/<name>`。
  - `export`：导出的 JS 函数名，省略时默认与 `name` 相同。
  - `title`：显示标题，可用字符串或 `text("i18n.key")` 多语言对象。
  - `icon`：左侧/标签图标，如 `"icons/puzzle.svg"`。
  - `draws_top_bar`：若视图自行绘制与系统红绿灯共存的顶栏填 `true`（普通面板或设置页留 `false`），自绘时搭配 `<SmeltWindowBar view={`${pluginId()}/${name}`} />`。
  - `instances`：`true` 表示按参数开独立实例（每次 `open-view` 开到主出口）；默认 `false` 表示工作台单例视图。
  - `settings`：可选 `{ group: "general" | "agents" | "system" | "plugins", keywords?: string[] }`，把该视图同时注册为设置页右侧的一个分类页面。
- **后台无头逻辑 / 弹窗**：若插件需要始终在后台挂一个不可见根节点（比如监听事件并弹出全局 `<Dialog>`，或响应无界面命令），导出 `export function root() { ... }`，主壳会自动把它挂在隐藏层中（关闭主窗口时仍保活）。

### 注册命令、页面、布局与菜单

所有登记 API 都在 `"smelt/sdk"` 中：

- `registerCommands(specs, groupTitle?)` + `onCommand(handler)`：
  - 注册命令，完整 id 为 `<插件 id>/<name>`。
  - `key` 可给默认快捷键（如 `"cmd-shift-y"`）；`palette` 控制是否进命令面板；`cli: true` + `args: ["field"]`（空数组 `[]` 表示无参）允许通过 `smelt app run <插件 id>/<name>` 从命令行调用。
  - 用 `onCommand(async ({ name, args }) => { ... })` 分发并返回值或抛错。
- `registerPages(specs)` & `registerLayouts(specs)`：声明扩展主窗口页面和 Dock 布局。
- `registerTopics(topics)`：订阅守护运行时广播话题（如 `"automations.changed"`；会话提醒宿主已镜像，读 `sessionAttention(machine)`、收 `attention_alert` 事件），再由 `onEvent(..., ["runtime_topic"])` 接收 `{ type: "runtime_topic", machine, topic, message }`。
- `registerMenuItems(items)`：`{ slot: "app" | "help", title, command }`（仅 macOS）。
- `registerContextMenuItems(menu, items)`：向其他插件定义的右键菜单注册条目。
- `setStatusMenu(menu | null)`：设置本插件专属 macOS 菜单栏状态图标与菜单（每插件独立）。
- `registerFileOpener(name)`：登记为可被打开文件的目标插件。

## 5. 状态、RPC 与存储（`smelt/sdk`）

- **细粒度响应式状态读取**（只订阅用到的切片）：
  `locale()`、`systemLocale()`、`locales()`、`theme()` / `currentTheme()`、`machineMeta()`、`machineMetaEntry(machine)`、`machines()`、`machineEntry(machine)`、`connections()`、`activeContext()`、`views()`、`pages()`、`commands()`、`viewShown(id)`、`viewLaidOut(id)`、`safeArea(viewId)`、`windowActive()`、`notificationPermission()`、`onSlices(slices, fn)`、`initHost(initialSlices)`、`hostReady()`。
- **国际化**：在插件目录 `locales/` 放 `ui.yml`（结构同 Smelt 主库，`en` / `zh-CN`），用 `t("key", { count })` 取当前语言译文，用 `text("key")` 生成传给 `registerViews` / `registerCommands` 的多语言对象。
- **离散事件**：`onEvent((event) => { ... }, types)` 需显式传入第二个参数 `types` 数组（仅在订阅的事件到达时唤醒 VM），接收 `view_opened`（附带 `open-view` 的 `args`）、`runtime_topic`、`plugin:<name>`、`window_activated`、`machines_changed`、`sessions_changed`、`themes_changed`、`locale_changed` 等；本插件也可用 `emitEvent(`${pluginId()}/event-name`, payload)` 广播事件。
- **调用命令与守护 RPC**：
  - `run("smelt.app/open-view", { view, args })` 等触发宿主或其它插件命令。
  - `call(machine, method, params, timeoutMs?)` / `callDetail(...)`：调用本机（`machine = null`）或远程（`machine = "<id>"`）`smeltd` 的 Control API（方法名联合类型见 `.nsh/types/smelt/sdk.d.ts` 中的 `ControlMethodName`）。可用 `await waitOnline(machine)` 事件驱动等待机器上线。
  - `openFile({ machine, path, line })`、`openUrl(url)`、`clipboardWrite(text)`、`pathKind(path)`、`revealPath(path)`、`discoverApps(queries)`、`launchPath(opts)`、`processUsage()`。
- **SQLite 持久化与数据目录**：
  - `store.migrate(steps)` / `store.query(sql, params)` / `store.execute(sql, params)` / `store.write(statements)`：读写 `~/.smelt/plugin-data/<id>/store.sqlite3`（幂等建表建议用 `CREATE TABLE IF NOT EXISTS` 或按 `migrate` 数组顺序逐版递增）。
  - `dataDir()`：返回本插件自己的可写数据目录路径，`pluginId()` 返回自身 id。

## 6. UI 组件、图标与主题令牌

- **Smelt 封装组件（优先使用，自动接入主题、字体、交互与 Markdown 链路）**：
  - `SmeltButton`：`label`、`icon`、`trailingIcon`、`tooltip`、`command`、`quiet` / `primary` / `danger` / `dismiss` / `floating`、`small`、`circle`、`fill`、`loading`、`selected`、`onPress`。
  - `SmeltIcon`：渲染内置 SVG，`<SmeltIcon path="icons/search.svg" size={14} spin={false} />`。
  - `SmeltVector`：渲染任意内联 SVG 字符串 `<SmeltVector markup="<svg ...>...</svg>" width={16} height={16} />`。
  - `SmeltMarkdown`、`SmeltEditor`、`SmeltDiffView`、`SmeltWindowBar`、`ViewInstance`（以及共享模块 `"smelt/conversation"` 导出的 `<SmeltConversation>`）。
  - `SmeltNativeTerminal`、`SmeltNativeDesktop`。
  - 共享浮层样式常量：`POPUP_CLASS`、`TOOLTIP_CLASS`、`MENU_ITEM_CLASS`、`INPUT_CLASS`、`DIALOG_SURFACE_CLASS`。
- **可用图标路径**：只能使用内置 SVG 资产路径（`icons/<name>.svg` 和 `smelt-icons/agent-<id>.svg`），精确枚举位于 `.nsh/types/smelt/sdk.d.ts` 的 `SmeltIconPath`。
- **Tailwind 语义令牌**（完整清单见 `.nsh/types/nsh/tokens.d.ts`）：
  除了标准 shadcn 令牌（`bg-background`, `bg-card`, `bg-popover`, `bg-muted`, `bg-primary`, `bg-accent`, `text-foreground`, `text-muted-foreground`, `border-border`, `border-input`），Smelt 额外提供 `faint`、`link`、`run`、`need`、`ok`、`bad`、`on-bad`、`soft-hover`、`row-selected`（可写作 `text-faint`、`text-run`、`text-need`、`text-ok`、`text-bad`、`bg-soft-hover`、`bg-row-selected`）。

## 7. 主题插件与主题 JSON 格式

如果只想改变全 App 的配色方案，不需要写 JS/TSX，直接编写**纯主题插件**（`entry` 指向 `.json`）；普通 TSX 插件也可以在目录里放置 `themes/*.json` 附带主题。

### 脚手架与验证

```sh
smelt plugin init me.sunset --theme --name "Sunset"
smelt plugin check ~/.smelt/plugins/me.sunset
smelt settings set appearance.theme="Sunset Dark"
```

- 纯主题插件的 `smelt-plugin.json` 示例：`{"id":"me.sunset","name":"Sunset","version":"0.1.0","entry":"themes/theme.json"}`。
- App 会监听目录并在 200ms 内热重载主题；若当前正启用该主题，也会同步重新着色。

### 主题文件格式（`themes/*.json`）

```json
{
  "version": 1,
  "name": "Sunset",
  "author": "Optional",
  "url": "https://example.com",
  "themes": [
    {
      "name": "Sunset Dark",
      "mode": "dark",
      "colors": {
        "bg_base": "#16171a",
        "text_base": "#e6e7eb",
        "bg_accent": "#e07a5f",
        "success.fg": "#6fc276"
      },
      "elevation": {
        "floating": [{ "y": 4, "blur": 8, "color": "#0000004d" }]
      },
      "syntax": {
        "keyword": { "color": "#f08d6c", "font_weight": 600 },
        "comment": { "color": "#7b818c", "font_style": "italic" }
      }
    }
  ]
}
```

- **严格校验与原子注册**：
  - `version` 必须为 `1`；`name`（系列名）与每个主题的 `name`、`mode`（`"light"` | `"dark"`）、`colors` 必填；`author`、`url`、`elevation`、`syntax` 可选。
  - 颜色只支持 `#rgb`、`#rgba`、`#rrggbb`、`#rrggbbaa`；任何不认识的键或非法颜色会令**整个插件的主题包原子拒绝**，保留之前可用的注册。
  - **保留名与重名限制**：不能使用内置保留名 `smelt Light` / `smelt Dark`，也不能与其它已加载插件注册的主题名重复。
- **`colors` 键说明**（仅 `bg_base` 与 `text_base` 必填且必须不透明，未写的颜色自动在 OKLab 空间按对比度规则推导）：
  - **背景与层级**：`page`（外层侧栏）、`bg_base`（主内容底）、`bg_deep`、`layer_01`–`layer_04`、`surface_raised`（弹层/菜单）、`bg_inverse`、`bg_contrast`（主按钮高对比底）、`bg_button_neutral`、`bg_icon_button_contrast`、`bg_accent`（强调/开关底）。
  - **文字与图标**：`text_base`、`text_muted`、`text_faint`、`text_inverse`、`text_contrast`、`text_accent`；对应的 `icon_base`、`icon_muted`、`icon_faint`、`icon_inverse`、`icon_contrast`、`icon_accent`。
  - **边框与交互层**：`border_muted`、`border_base`、`border_strong`、`border_focus`、`hover`、`pressed`、`contrast_hover`、`contrast_pressed`、`scrim`、`selection`、`knob`、`knob_on`、`submit_highlight`、`submit_hover`、`submit_fg`、`submit_icon`、`depth_top`、`depth_bottom`、`white`、`black`。
  - **状态色与图表色**：`success.{bg,fg,border,icon}`、`warning.{bg,fg,border,icon}`、`danger.{bg,fg,border,icon}`、`info.{bg,fg,border,icon}`、`chart.1`–`chart.5`。
- **`elevation` 键说明**：可选，键为 `raised`、`floating`、`overlay`、`button_neutral`、`button_contrast`、`elements`、`switch_off`、`switch_on`、`md`、`knob`、`input`。每项是阴影对象数组 `[{ x?, y?, blur?, spread?, color, inset? }]`（只有 `color` 必填，其余默认 `0`/`false`，空数组 `[]` 表示取消投影）。
- **`syntax` 键说明**：可选，支持 tree-sitter 高亮名（如 `attribute`、`boolean`、`comment`、`comment.doc`、`constant`、`constructor`、`function`、`keyword`、`label`、`number`、`operator`、`property`、`punctuation`、`string`、`string.escape`、`tag`、`text.code.span`、`text.literal`、`title`、`type`、`variable`、`variant` 等），每项 `{ color?, font_style?: "normal" | "italic" | "underline", font_weight?: 100..900（整百）}`。省略 `syntax` 时整套沿用同明暗的内置 `smelt` 代码高亮；若显式给出 `syntax`，则整套替换。

## 8. 最小完整示例

```tsx
import { computed, signal, Text, View } from "nsh";
import {
  bootTask,
  initHost,
  machineMeta,
  onCommand,
  onEvent,
  registerCommands,
  registerViews,
  run,
  SmeltButton,
  SmeltIcon,
  store,
} from "smelt/sdk";

const lastMessage = signal("Ready");

onEvent(
  (ev) => {
    if (ev.type === "view_opened") {
      const note = (ev.args as { note?: string } | null)?.note;
      if (note) lastMessage.set(String(note));
    }
  },
  ["view_opened"],
);

bootTask(async () => {
  await initHost(["machines_meta"]);
  await store.migrate([
    "CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
  ]);
  await registerViews([
    {
      name: "main",
      export: "Main",
      title: "Quick Notes",
      icon: "icons/pencil.svg",
    },
  ]);
  await registerCommands([
    {
      name: "ping",
      title: "Ping Quick Notes",
      cli: true,
      args: ["text"],
    },
  ]);
  onCommand(({ name, args }) => {
    if (name === "ping") {
      lastMessage.set(String(args?.text ?? "pong"));
      return { ok: true };
    }
    return null;
  });
});

export function Main() {
  const count = computed(() => machineMeta().length);

  return (
    <View class="flex-1 gap-3 bg-background p-4">
      <View class="flex-row items-center gap-2">
        <SmeltIcon path="icons/pencil.svg" size={15} />
        <Text class="text-sm font-semibold text-foreground">Quick Notes</Text>
        <Text class="text-xs text-faint">{`Machines: ${count()}`}</Text>
      </View>
      <Text class="text-xs text-muted-foreground">{lastMessage()}</Text>
      <SmeltButton
        label="Notify"
        small
        onPress={() => void run("smelt.app/notify", { subtitle: "Quick Notes", body: lastMessage() })}
      />
    </View>
  );
}
```
