# Smelt 插件速查

给智能体写插件时用的速查。完整文档在仓库的 `docs/plugins/`（<https://github.com/smelt-ai/smelt/tree/main/docs/plugins>）；签名以插件目录里生成的 `.nsh/types/` 为准，按符号名搜，不要猜。

## 1. 目录与清单

```
~/.smelt/plugins/com.example.demo/
  smelt-plugin.json   清单
  nsh.tsx             入口（路径由 entry 定）
  model.ts            同目录模块，相对路径 import，不能 ../ 越出目录
  locales/ui.yml      文案（_version: 2，每个键下 en / zh-CN）
  themes/*.json       可选：附带主题
  tests/              单元测试（bun test），不打包、不触发重载
  .nsh/  tsconfig.json  smelt plugin init / types 生成
```

```json
{ "id": "com.example.demo", "name": "Demo", "version": "0.1.0", "entry": "nsh.tsx" }
```

- `id`：ASCII 字母、数字、`.-_`，不以 `.` 开头。视图、命令、页、布局、事件 id 都是 `<插件 id>/<name>`。
- `version`：只用来显示，宿主不比较。
- 四种插件：脚本（`entry` 指 `.tsx`/`.ts`/`.js`）；共享模块（再加 `module: "a/b"` 和可选 `types`，打包进 import 它的插件，各插件各有一份顶层变量）；主题（`entry` 指 `.json`，不跑 JS）；布局（`pages` + `layouts`，没有 `entry`，不跑 JS）。`entry` 和 `pages`/`layouts` 二选一。
- 布局插件清单：`pages: [{ name, title_key, home?, index? }]`，`layouts: [{ page, name, title_key, file, materials? }]`，`title_key` 取 `locales/ui.yml`。
- 加载时忽略：点开头的、`node_modules/`、`tsconfig.json`、`tests/`、带 `.test.` 的。
- 和内置插件同 id 是**覆写**：失败自动回退内置版；数据是从内置版复制出来的一份（`~/.smelt/plugin-data/.overrides/<id>/`），各改各的，再 fork 一次重新复制。

## 2. 能 import 什么，能做什么

- 模块：`nsh`、`nsh/base`、`nsh/desktop`、`nsh/i18n`、`smelt/sdk`、共享模块（如 `smelt/conversation`）、同目录相对文件。`smelt/host` 是底层绑定，用 `smelt/sdk`。`smelt/slots` 不对插件开放。
- 全局只有 Web 标准的：`fetch`、`setTimeout`、`requestAnimationFrame`、`console`、`crypto`、`URL`、`TextEncoder` 等。**没有** Node 的 `fs`、`path`、`process`、`Buffer`、`require`。
- 插件**没有文件系统接口**。数据存 `store`（SQLite）；远程或本机项目文件经运行时（`call(machine, "file.open" …)`）。插件目录里的图片宿主不加载，图形用自带图标或 `SmeltVector`。
- 不按权限隔离：任何插件都能调运行时、发通知、启动本机应用。只装可信的。

## 3. NSH 写法红线

NSH 不是 React：组件函数只跑一次，读了 signal 的地方才更新。

1. 不解构 props：`function Card(props) { return <Text>{props.title}</Text>; }`。视图参数读 `props.args`。
2. 字放进 `<Text>`。
3. 条件和列表用全局 `<If when={x()} else={…}>{(v) => …}</If>`、`<Each of={items()} key={(i) => i.id}>{(i) => …}</Each>`（子函数拿到 accessor），大列表用 `nsh` 的 `<List>`。
4. 组件里订的要清理：`cleanup(onEvent(fn, ["sessions_changed"]))`。
5. 组件函数和渲染里不做 IO：放 `bootTask`、事件回调、`resource`。
6. 不轮询：用状态访问器、`onEvent`、运行时话题、`waitOnline`。逐帧的事用 `requestAnimationFrame`，停了就别再要帧。
7. 不写死颜色：用主题令牌 class 或 `theme().palette`。
8. 文案走 `t()`，两种语言一起写。

## 4. `smelt/sdk` 速查

**启动与登记**（都是整组登记，再调整组替换，空数组撤下）：

- `bootTask(async () => …)`：启动要做完的事，宿主最多等 3 秒再挂视图；抛错只记日志。
- `registerViews([{ name, export?, title, icon?, draws_top_bar?, instances?, settings? }])`。`export` 缺省同 `name`；`instances: true` 每次打开新建一份放进出口；`settings: { group: "general"|"agents"|"system"|"plugins", keywords? }` 做成设置页。
- 导出 `root()`：无头根，一直挂着（全局对话框、后台逻辑）。
- `registerCommands([{ name, title, key?, palette?, args?, cli?, when_view_shown?, when_active_session? }], groupTitle?)` + `onCommand(({ name, args }) => 结果)`：返回值是回复，抛错是失败。`args` 是认的字段名表（`[]` 不带参数），给了表外的字段宿主直接拒；`cli: true` 才能 `smelt app run`。
- `registerPages`、`registerLayouts`、`registerTopics(["automations.changed"])`、`registerMenuItems([{ slot: "app"|"help", title, command }])`、`registerContextMenuItems(menu, [{ label, icon?, command, args? }])`、`setStatusMenu(menu|null)`、`registerFileOpener(name)`、`setActiveContext(ctx)`。
- 标题用 `text("key")`（整张多语言表），界面里用 `t("key", { n })`。文案表随宿主状态来，`text()` 要在 `await initHost()` 之后。

**状态**（响应式，读到才订）：`machineMeta()`、`machines()`、`machineEntry(m)`、`pathStyle(m)`、`conversationFacts(m)`、`sessionAttention(m)`、`activeContext()`、`connections()`、`windowActive()`、`notificationPermission()`、`views()`、`pages()`、`commands()`、`command(id)`（没有给 `null`）、`viewShown(id)`、`viewLaidOut(id)`、`viewBadge(id)`、`safeArea(id)`、`theme()`、`locale()`。`workspaceDirectory(m)`、`sessionStates(m)` 是运行时原始结构，会变。

**事件**：`onEvent(fn, types)` 必须给 `types`。常用：`view_opened`（带 `args`）、`sessions_changed`、`attention_alert`、`machines_changed`、`project_changed`、`reconnected`、`window_activated`、`locale_changed`、`themes_changed`、`commands_changed`、`runtime_topic`、`plugin:<事件名>`。宿主先推切片再发事件。广播：`emitEvent(`${pluginId()}/name`, payload)`。

**调用**：

- `run(id, args?)`：调命令（插件的或 `smelt.app/*`）。
- `call(machine, method, params?, timeoutMs?)`：Control API，`machine` 是 `null`（本机）或远程 id；方法名见 `ControlMethodName`。要错误码用 `callDetail`（`{ ok, value }` / `{ ok: false, code, message }`）。
- `waitOnline(machine, failures?)`：等上线，机器被移除给 `false`。
- 运行时话题消息 `kind`：`event` / `snapshot`（整份换掉）/ `lagged`（自己重读）/ `unavailable`（这台机器不支持）。
- 本机：`openUrl`、`openFile({ machine, path, line? })`、`pathKind`、`revealPath`、`clipboardWrite`、`discoverApps`、`launchPath`、`processUsage`、`renderMermaidSvg`、`fetch`。
- 路径用 `pathJoin(style, …)` 这组函数，`style = pathStyle(machine)`，远程可能是 Windows。

**存储**：`store.migrate([step1, step2])`（`steps[i]` 把版本 i 迁到 i+1，只往后加，放 `bootTask` 里最先跑）、`store.query(sql, params)`、`store.execute(sql, params)`、`store.write([{ sql, params }])`（一个事务）。参数只能是 string/number/boolean/null，不存 JSON 大字段、不存 BLOB。

**宿主命令**（`run`）：`smelt.app/open-view { view, args? }`、`open-page { page }`、`set-view-badge { view, label }`、`notify { id?, subtitle, body, on_click? }`、`withdraw-notification { id }`、`set-dock-badge { label }`、`pick-paths { machine, title, confirm, files?, multiple? }`、`open-in-text-editor { path }`、`open-in-file-manager { path }`、`show-main-window`、`settings`。

**内部，不要用**：`SmeltNative*`、`nativeApi`、`acp*`、`resolve*Acp`、`agentCatalog`、`quotaFacts` 等给内置插件的规则函数、`shellState`、`jsViews`、`request`（少数 Control API 还没覆盖的才用）。

## 5. 组件、图标、令牌

- `SmeltButton`：`label`、`icon`、`trailingIcon`、`tooltip`、`onPress`、`primary`（一块界面最多一个）、`quiet`、`danger`、`dismiss`、`floating`、`circle`、`small`、`fill`、`disabled`、`loading`、`selected`、`toggled`。`command` 只用来在提示里显示快捷键，不会替你执行。
- 浮层：`SmeltPopover`、`SmeltMenu`、`SmeltContextMenu`、`SmeltSelect`、`SmeltTooltip`、`SmeltDialog`（`onOpenChange={({ open }) => …}`），样子常量 `POPUP_CLASS`、`MENU_CLASS`、`MENU_ITEM_CLASS`、`TOOLTIP_CLASS`、`INPUT_CLASS`、`DIALOG_SURFACE_CLASS`。
- 内容：`SmeltMarkdown`、`SmeltEditor`、`SmeltDiffView`、`SmeltVector`、`SmeltDisclosure`、`SmeltWindowBar`、`ViewInstance`；对话界面是共享模块 `smelt/conversation` 的 `<SmeltConversation>`。
- 图标：`<SmeltIcon path="icons/search.svg" size={14} spin? />`，只能用 `SmeltIconPath` 里列的；智能体标志 `smelt-icons/agent-<id>.svg`。
- 令牌 class：`background`/`foreground`、`card`、`popover`、`primary`、`secondary`、`muted`/`muted-foreground`、`faint`、`accent`、`row-selected`、`soft`/`soft-hover`、`border`、`input`、`ring`、`link`、`run`、`need`、`ok`、`bad`、`on-bad`、`scrim`、`sidebar*`、`chart-1`…`chart-5`；阴影 `shadow-raised`、`shadow-floating`、`shadow-overlay`。完整清单 `.nsh/types/nsh/tokens.d.ts`。

## 6. 主题 JSON

```json
{
  "version": 1,
  "name": "Sunset",
  "themes": [
    {
      "name": "Sunset Dark",
      "mode": "dark",
      "colors": { "bg_base": "#16171a", "text_base": "#e6e7eb", "bg_accent": "#e07a5f" },
      "elevation": { "floating": [{ "y": 4, "blur": 8, "color": "#0000004d" }] },
      "syntax": { "keyword": { "color": "#f08d6c", "font_weight": 600 } }
    }
  ]
}
```

- 必填：`version: 1`、系列 `name`、每个主题的 `name`、`mode`（`light`/`dark`）、`colors`。`colors` 里只有 `bg_base`、`text_base` 必填且不透明，其余在 OKLab 里推出来。颜色 `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa`。
- `colors` 键：`page`、`bg_base`、`bg_deep`、`layer_01`–`layer_04`、`surface_raised`、`bg_inverse`、`bg_contrast`、`bg_button_neutral`、`bg_icon_button_contrast`、`bg_accent`；`text_*`、`icon_*`（`base`/`muted`/`faint`/`inverse`/`contrast`/`accent`）；`border_muted`/`base`/`strong`/`focus`、`hover`、`pressed`、`contrast_hover`、`contrast_pressed`、`selection`、`scrim`；`submit_highlight`/`hover`/`fg`/`icon`；`knob`、`knob_on`、`depth_top`、`depth_bottom`、`white`、`black`；`success|warning|danger|info.{bg,fg,border,icon}`；`chart.1`–`chart.5`。
- `elevation` 键：`raised`、`floating`、`overlay`、`md`、`button_neutral`、`button_contrast`、`elements`、`switch_off`、`switch_on`、`knob`、`input`，值是 `[{ x?, y?, blur?, spread?, color, inset? }]`，`[]` 是不要投影。
- `syntax`：tree-sitter 高亮名 → `{ color?, font_style?: normal|italic|underline, font_weight?: 100..900 }`。给了就整套替换内置高亮。
- 有一处不对，这个插件的主题整组不生效。不能用保留名 `smelt Light`/`smelt Dark`，不能和别的插件的主题重名（`check` 会查已装的）。

## 7. 最小示例

```tsx
import { signal, Text, View } from "nsh";
import {
  bootTask, initHost, onCommand, pluginId, registerCommands, registerViews,
  run, SmeltButton, store, t, text,
} from "smelt/sdk";

const count = signal(0);

bootTask(async () => {
  await initHost();
  await store.migrate(["CREATE TABLE counter (id INTEGER PRIMARY KEY CHECK (id = 1), n INTEGER NOT NULL);"]);
  const rows = await store.query("SELECT n FROM counter WHERE id = 1");
  count.set(Number(rows[0]?.n ?? 0));
  await registerViews([{ name: "main", export: "Main", title: text("title"), icon: "icons/plus.svg" }]);
  await registerCommands([{ name: "reset", title: text("cmd.reset"), cli: true, args: [] }]);
});

onCommand(async ({ name }) => {
  if (name !== "reset") throw new Error(`unknown command ${name}`);
  await save(0);
  return { n: 0 };
});

async function save(n: number) {
  count.set(n);
  await store.execute("INSERT INTO counter (id, n) VALUES (1, ?) ON CONFLICT(id) DO UPDATE SET n = excluded.n", [n]);
}

export function Main() {
  return (
    <View class="flex-1 gap-3 bg-background p-4">
      <Text class="text-foreground">{t("count", { n: count() })}</Text>
      <View class="flex-row gap-2">
        <SmeltButton label={t("add")} primary onPress={() => void save(count() + 1)} />
        <SmeltButton label={t("reset")} quiet onPress={() => void run(`${pluginId()}/reset`)} />
      </View>
    </View>
  );
}
```

`locales/ui.yml` 里写 `title`、`cmd.reset`、`count`（`%{n}`）、`add`、`reset` 的 `en` 和 `zh-CN`。

## 8. 闭环

```sh
smelt plugin init com.example.demo --name "Demo"     # 或 --theme；改内置用 smelt plugin fork <id> [--as <新 id>]
smelt plugin check ~/.smelt/plugins/com.example.demo # 离线校验，不过不算写完
smelt plugin status com.example.demo                 # 保存一两秒后：stale 为 false、error 为 null、views 里有视图、fallback_to_builtin 为 false
smelt app run smelt.app/open-view --args '{"view":"com.example.demo/main"}'
```

- 日志：`~/.smelt/app.log` 里 `[plugin]` 的行（`console.*`、未捕获异常、`bootTask failed`），行里不带插件 id。
- 升级 Smelt 后：`smelt plugin types <目录>` 刷新声明，再 `check`。
- 删插件先问用户；「设置 → 面板插件」里能恢复默认、看错误。
