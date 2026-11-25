# 目录与清单

一个插件就是一个目录，目录里必须有 `smelt-plugin.json`。用户插件放在 `~/.smelt/plugins/<id>/`，目录名不要求和 id 一样，但一样最好找。

## 目录

```
com.example.counter/
  smelt-plugin.json   清单（必需）
  nsh.tsx             入口，路径由清单的 entry 定
  model.ts            同目录的模块，入口用相对路径 import
  locales/ui.yml      可选：多语言文案
  themes/*.json       可选：附带的主题
  tests/              可选：单元测试，不打包、不触发热重载
  tsconfig.json       smelt plugin init / types 生成
  .nsh/               生成的类型声明，别提交
```

加载时忽略这些：以 `.` 开头的文件和目录、`node_modules/`、`tsconfig.json`、`tests/` 目录、名字里带 `.test.` 的文件。它们改了也不触发重载。

插件目录只放源码。插件运行时的数据存进它的 [SQLite](storage.md)，库在 `~/.smelt/plugin-data/<id>/`，和插件目录分开：删掉、重装、覆写插件都不碰它。

## 清单 `smelt-plugin.json`

```json
{
  "id": "com.example.counter",
  "name": "Counter",
  "version": "0.1.0",
  "entry": "nsh.tsx"
}
```

| 字段 | 类型 | 必需 | 说明 |
| --- | --- | --- | --- |
| `id` | `string` | 是 | 插件 id。只能有 ASCII 字母、数字和 `.`、`-`、`_`，不能以 `.` 开头。视图、命令、页、布局、事件的 id 都以它开头，数据目录也按它命名 |
| `name` | `string` | 是 | 给人看的名字，插件管理页里显示 |
| `version` | `string` | 否 | 插件自己的版本号，不写是 `"0.0.0"`。只用来显示，宿主不比较它 |
| `entry` | `string` | 见下 | 入口，插件目录里的相对路径。以 `.json` 结尾时是主题插件 |
| `module` | `string` | 否 | 把这个插件开成共享模块，值是别人 import 用的名字，如 `"smelt/conversation"`。见[插件之间](plugin-interop.md#共享模块) |
| `types` | `string` | 否 | 共享模块的类型声明，插件目录里的 `.d.ts` 相对路径 |
| `requires` | `string[]` | 否 | 用到的共享模块，写 import 用的名字，如 `["smelt/conversation"]`。import 了就得列。见[插件之间](plugin-interop.md#共享模块) |
| `pages` | `PageEntry[]` | 见下 | 布局插件登记的页 |
| `layouts` | `LayoutEntry[]` | 见下 | 布局插件登记的布局 |

`entry` 和 `pages`/`layouts` 二选一：有 `entry` 就不能有 `pages`/`layouts`，反过来也一样，两样都没有是错的。

## 四种插件

| 种类 | 清单写法 | 跑不跑 JS | 例子 |
| --- | --- | --- | --- |
| 脚本插件 | `entry` 指向 `.tsx`/`.ts`/`.js` | 跑，自己一个 VM | `smelt.quota`、`smelt.files` |
| 共享模块 | `entry` + `module` | 不单独跑，打包进 import 它的插件 | `smelt.conversation`、`smelt.avatar` |
| 主题插件 | `entry` 指向 `.json` | 不跑 | `smelt.themes` |
| 布局插件 | `pages` + `layouts`，没有 `entry` | 不跑 | `smelt.layouts` |

脚本插件也可以在 `themes/` 下带主题文件。主题插件除了 `entry` 指的那份，`themes/` 下所有 `.json` 也都登记。见[主题插件](themes.md)。

### 布局插件的清单

布局插件不写代码，把页和 Dock 布局直接写在清单里，标题从 `locales/ui.yml` 取：

```json
{
  "id": "smelt.layouts",
  "name": "Layouts",
  "version": "1.0.0",
  "pages": [
    { "name": "workbench", "title_key": "page.workbench", "home": true, "index": "smelt.sessions/session" }
  ],
  "layouts": [
    {
      "page": "smelt.layouts/workbench",
      "name": "standard",
      "title_key": "layout.standard",
      "file": "layouts/standard.json",
      "materials": { "left": "sidebar" }
    }
  ]
}
```

`PageEntry`：

| 字段 | 类型 | 必需 | 说明 |
| --- | --- | --- | --- |
| `name` | `string` | 是 | 插件内的名字，不能有 `/`。页 id 是 `<插件 id>/<name>` |
| `title_key` | `string` | 是 | `locales/ui.yml` 里的键 |
| `home` | `boolean` | 否 | 是不是首页 |
| `index` | `string` | 否 | 出口里没放东西时显示的视图 id |

`LayoutEntry`：

| 字段 | 类型 | 必需 | 说明 |
| --- | --- | --- | --- |
| `page` | `string` | 是 | 布局属于哪一页，完整页 id |
| `name` | `string` | 是 | 插件内的名字，不能有 `/` |
| `title_key` | `string` | 是 | `locales/ui.yml` 里的键 |
| `file` | `string` | 是 | 布局 JSON 文件，插件目录里的相对路径 |
| `materials` | `object` | 否 | 各区的材质，键是 `left`/`right`/`bottom`/`center`，值是 `"sidebar"` 或 `"window"` |

页、布局、Dock 布局文件的格式见[页与布局](pages-and-layouts.md)。脚本插件也能用 `registerPages`、`registerLayouts` 在代码里登记。

## 插件从哪来

宿主扫两处：

1. **随 App 带的插件**：开发时读仓库的 `plugins/`，发版的 `.app` 读 `Contents/Resources/plugins/`。
2. **用户插件**：`~/.smelt/plugins/`。

同一处里两个插件 id 相同算错，报重复。用户插件和随 App 带的插件 id 相同，是**覆写**：用户那份顶替自带的，见[覆写和 fork 内置插件](plugin-interop.md#覆写和-fork-内置插件)。

## 热重载

- 盯着的目录：用户插件目录；开发构建时也盯仓库的 `plugins/`。
- 一次保存常常带来好几个文件事件，宿主等 200 ms 没有新事件才动手。
- 只重编改了的插件，只重建它自己的 VM。改了共享模块，宿主连带重编 import 它的插件。
- 删掉插件目录就卸载它；它的数据目录 `~/.smelt/plugin-data/<id>/` 留着，装回来接着用。

## 下一步

- [视图](views.md)
- [命令行](cli.md)
