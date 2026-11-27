# 第一个插件

这一页从零做一个计数器插件：一块视图、一条命令、存进 SQLite，最后在 App 里打开它。要先装好桌面版 Smelt（`smelt plugin` 这组命令只在桌面版里有，Linux 上不带界面的 `smelt` 没有）。

## 建出来

```sh
smelt plugin init com.example.counter --name "Counter"
```

插件建在 `~/.smelt/plugins/com.example.counter/`，命令把结果打成 JSON：

```json
{
  "ok": true,
  "id": "com.example.counter",
  "name": "Counter",
  "path": "/Users/you/.smelt/plugins/com.example.counter",
  "view": "com.example.counter/main",
  "files": [".gitignore", ".nsh/tsconfig.json", ".nsh/types/", "locales/ui.yml", "nsh.tsx", "smelt-plugin.json", "tsconfig.json"]
}
```

| 文件 | 是什么 |
| --- | --- |
| `smelt-plugin.json` | 清单：`id`、`name`、`version`、`entry`。见[目录与清单](project-structure.md) |
| `nsh.tsx` | 入口：登记视图，导出视图组件 |
| `locales/ui.yml` | 文案，每个键下写 `en` 和 `zh-CN`。见[多语言](i18n.md) |
| `tsconfig.json` | 继承 `.nsh/tsconfig.json`，编辑器靠它认 `nsh`、`smelt/sdk` |
| `.nsh/types/` | 当前 Smelt 版本的类型声明：NSH、SDK、图标路径、运行时方法名 |
| `.gitignore` | 忽略 `.nsh/` |

> [!TIP]
> id 用反向域名（`com.你的名字.功能`），只能有字母、数字和 `.`、`-`、`_`，不能以 `.` 开头，也不能和内置插件重名。想改内置插件用 [`smelt plugin fork`](plugin-interop.md#覆写和-fork-内置插件)。

## 入口长什么样

```tsx
import { signal, Text, View } from "nsh";
import { bootTask, initHost, registerViews, SmeltButton, SmeltIcon, t, text } from "smelt/sdk";

const count = signal(0);

bootTask(async () => {
  await initHost();
  await registerViews([
    {
      name: "main",
      export: "Main",
      title: text("title"),
      icon: "icons/puzzle.svg",
    },
  ]);
});

export function Main() {
  return (
    <View class="flex-1 items-center justify-center gap-3 bg-background p-6">
      <View class="flex-row items-center gap-2">
        <SmeltIcon path="icons/puzzle.svg" size={16} />
        <Text class="text-sm font-semibold text-foreground">{t("title")}</Text>
      </View>
      <Text class="text-xs text-muted-foreground">{t("count", { n: count() })}</Text>
      <SmeltButton label={t("add")} variant="primary" onPress={() => count.update((n) => n + 1)} />
    </View>
  );
}
```

`locales/ui.yml`：

```yaml
_version: 2
title:
  en: "Counter"
  zh-CN: "Counter"
count:
  en: "Count: %{n}"
  zh-CN: "计数：%{n}"
add:
  en: Add one
  zh-CN: 加一
```

- **`bootTask(work)`**：登记一件启动时要做完的事。宿主等所有启动任务做完（最多等 3 秒）才认这个插件就绪，再把布局里摆着的视图挂上去。登记视图、建表、读设置都放这里。
- **`initHost()`**：接上宿主，拉第一份状态（文案表也在里面，所以要在 `text()` 之前）。不传参数时拉默认的几块，见[宿主状态与事件](host-state.md)。
- **`registerViews([...])`**：登记视图。`name` 是插件内的名字，完整 id 是 `<插件 id>/<name>`，这里是 `com.example.counter/main`。`export` 是导出的组件名，不写就和 `name` 一样。
- **`Main`**：视图组件。NSH 的组件函数**只跑一次**，之后只有读了 signal 的地方跟着更新。`count()` 读值，`count.update(...)` 改值。`count` 放在模块顶层：视图关了再开，数还在。
- **`text("title")` 和 `t("title")`**：登记视图、命令时给 `text()`（整张多语言表，宿主按当前语言取）；界面里用 `t()`，`%{n}` 换成参数。
- **`class="..."`**：NSH 的 Tailwind。`bg-background`、`text-foreground` 这些是主题令牌，换主题时自动变色，见[写界面](ui.md#主题令牌)。

## 校验

```sh
smelt plugin check ~/.smelt/plugins/com.example.counter
```

`check` 离线做一遍 App 加载时做的事：读清单、查 `locales/`、打包 TSX、查 import、编字节码，不改目录。过了退出码是 0，并打出导出的符号；没过把原因写到 stderr，退出码 1。

## 在 App 里打开

Smelt 开着时，插件目录一有改动就自动加载。确认它加载好了：

```sh
smelt plugin status com.example.counter
```

看 `error` 是 `null`，`views` 里有 `com.example.counter/main`。然后打开它：

```sh
smelt app run smelt.app/open-view --args '{"view":"com.example.counter/main"}'
```

当前布局里没摆这个视图，它会开在主窗口的出口里（中间那块）。

## 加一条命令，存进 SQLite

把计数存下来，再加一条「清零」命令，能从命令面板和命令行调。把 `nsh.tsx` 换成：

```tsx
import { signal, Text, View } from "nsh";
import {
  bootTask,
  initHost,
  onCommand,
  registerCommands,
  registerViews,
  SmeltButton,
  store,
  t,
  text,
} from "smelt/sdk";

const count = signal(0);

async function save(next: number) {
  count.set(next);
  await store.execute("INSERT OR REPLACE INTO counter (id, value) VALUES (1, ?)", [next]);
}

bootTask(async () => {
  await initHost();
  await store.migrate([
    "CREATE TABLE counter (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);",
  ]);
  const rows = await store.query("SELECT value FROM counter WHERE id = 1");
  count.set(rows[0]?.value ?? 0);

  await registerViews([{ name: "main", export: "Main", title: text("title"), icon: "icons/puzzle.svg" }]);
  await registerCommands([{ name: "reset", title: text("reset"), cli: true, args: [] }]);
});

onCommand(async ({ name }) => {
  if (name === "reset") {
    await save(0);
    return { value: 0 };
  }
  throw new Error(`unknown command ${name}`);
});

export function Main() {
  return (
    <View class="flex-1 items-center justify-center gap-3 bg-background p-6">
      <Text class="text-2xl font-semibold text-foreground">{String(count())}</Text>
      <SmeltButton label={t("add")} variant="primary" onPress={() => void save(count() + 1)} />
    </View>
  );
}
```

- **`locales/ui.yml` 加一个键**：`reset:`，`en: Reset Counter`、`zh-CN: 计数清零`。
- **`store.migrate([...])`**：数组第 `i` 项把库从版本 `i` 迁到 `i + 1`，跑过的不再跑。以后要加列就往数组**后面**加一项，别改已有的。见[存储](storage.md)。
- **`registerCommands`**：完整 id 是 `com.example.counter/reset`。`cli: true` 加 `args: []`（不带参数）让命令行能调。
- **`onCommand`**：命令被调时跑。返回值交回给调用方，抛错就是失败。

保存后几百毫秒插件就重新加载了。从命令行试一下：

```sh
smelt app run com.example.counter/reset
```

## 出错了怎么看

- 打包、编译错：`smelt plugin check` 和 `smelt plugin status` 都会报。
- 运行时的 `console.log` 和没接住的异常：在 `~/.smelt/app.log`，来源是 `plugin`。
- 视图渲染抛错：视图位置显示「出错了」，错误也进 `app.log`。

更多见[调试与测试](debugging.md)。

## 下一步

- 视图怎么接收参数、怎么开多个实例：[视图](views.md)
- 读会话、项目、机器：[宿主状态与事件](host-state.md)、[调用运行时](runtime.md)
- 文案多语言：[多语言](i18n.md)
- 用 Smelt 的按钮、浮层、对话框、图标：[写界面](ui.md)
