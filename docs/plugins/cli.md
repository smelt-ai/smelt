# 命令行

写插件用 `smelt plugin`，在开着的 App 里调命令用 `smelt app`。两组命令都把结果作为 JSON 打到 stdout；失败时原因写到 stderr，退出码非 0。

`smelt plugin` 只在桌面版里有：`types` 和 `check` 要用和 App 一模一样的界面集、主题令牌、图标和 QuickJS 编译器。Linux 上不带界面的 `smelt` 没有这组命令。

## `smelt plugin`

| 命令 | 做什么 | 要 App 开着 |
| --- | --- | --- |
| `init <id> [--name <名字>] [--dir <目录>]` | 建一个 TSX 起步插件和类型声明 | 否 |
| `init <id> --theme [--name <名字>] [--dir <目录>]` | 建一个主题插件，带深浅两套起步主题 | 否 |
| `fork <内置 id> [--as <新 id>] [--dir <目录>]` | 把内置插件的源码复制出来改 | 否 |
| `types <目录>` | 生成或刷新 `<目录>/.nsh/types/` 和 `.nsh/tsconfig.json` | 否 |
| `check <目录>` | 离线校验插件 | 否 |
| `status [<id>]` | 看开着的 App 里插件的加载结果 | 是 |

### `init`

不给 `--dir` 时建在 `~/.smelt/plugins/<id>/`。`--name` 不给时从 id 最后一段推（`com.example.my-tool` → `My Tool`）。id 和内置插件重名会被拒绝；目录里已有清单或入口也会被拒绝，不覆盖。

建出来的插件已经能通过 `check`。结果里的 `view` 是起步视图的 id。

### `fork`

把内置插件复制到 `~/.smelt/plugins/<id>/`（或 `--dir`），跳过 `tests/`、`.test.` 文件和预编译字节码，再生成类型声明。

- 不带 `--as`：同 id，**覆写**内置插件。
- 带 `--as <新 id>`：改名，和内置的并存。

结果里的 `overrides_builtin` 说明是不是覆写，`source_path` 是复制的来源。覆写的规则和风险见[插件之间](plugin-interop.md#覆写和-fork-内置插件)。

### `types`

声明来自当前 Smelt 版本：NSH 的 `nsh`、`nsh/base`、`nsh/desktop`、`nsh/i18n`，`smelt/sdk`，`smelt/host`，能用的图标路径 `SmeltIconPath`，运行时方法名 `ControlMethodName`，以及找到的共享模块（内置的和用户覆写的）。

**升级 Smelt 后重跑一次**，声明才跟上新版本。

### `check`

做一遍 App 加载时做的检查，不改目录：

- 读清单，校验字段；
- 读 `locales/`；
- 校验 `themes/` 下的主题，整包要么全过、要么全不登记；
- 打包 TSX，查剩下的 import 是否都在白名单里、有没有用到没声明的全局变量；
- 编成 QuickJS 字节码。

过了的结果：

```json
{
  "ok": true,
  "id": "com.example.counter",
  "name": "Counter",
  "version": "0.1.0",
  "path": "/Users/you/.smelt/plugins/com.example.counter",
  "theme_only": false,
  "layout_only": false,
  "themes": [],
  "exports": ["Main"]
}
```

共享模块还会带 `module` 字段。`check` 不过就不算写完。

### `status`

读开着的 App 写的加载快照。不带 id 列出全部；带 id 只看那一个，这个插件有错误时退出码是 1。

| 字段 | 说明 |
| --- | --- |
| `id`、`name`、`version`、`path` | 清单和所在目录 |
| `source` | `"builtin"` 或 `"user"` |
| `overrides_builtin` | 用户插件顶替了同 id 的内置插件 |
| `fallback_to_builtin` | 覆写版加载失败，退回了内置版 |
| `module` | 共享模块的名字 |
| `uses_modules` | 打包进来的共享模块 |
| `views` | 登记了的视图 id |
| `error` | 加载失败的原因；加载好了是 `null` |

改完保存，等一两秒再看 `status`：`error` 是 `null`、`views` 里有你的视图，才算真的加载上了。

## `smelt app`

```sh
smelt app commands
smelt app run <命令 id> [--args '<JSON>']
```

调的是**开着的 App** 里登记的命令，只有登记时标了 `cli: true` 的才列得出、调得了。App 没开会报错。

- `commands` 列出每条命令的 `id`、`title` 和认的参数字段 `args`（空表是不带参数）。
- `run` 把 `--args` 的 JSON 交给命令，不给就是 `null`。stdout 是命令交回的值。

宿主开给命令行的命令：

```sh
smelt app run smelt.app/show-main-window
smelt app run smelt.app/open-view --args '{"view":"com.example.counter/main"}'
smelt app run smelt.app/open-page --args '{"page":"smelt.layouts/workbench"}'
```

`open-view`、`open-page` 要主窗口开着，没开先 `show-main-window`。插件怎么把命令开给命令行见[命令与菜单](commands.md#从命令行调)。
