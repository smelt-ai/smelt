# 介绍

**插件**是一个目录，里面有一份清单 `smelt-plugin.json` 和一个 TSX 入口。Smelt 启动时找到它、打包、放进一个 JS 虚拟机里跑，它登记的视图、命令、页、布局、主题就成了 App 的一部分。

Smelt 自己的功能也是这样做的：左侧会话列表是 `smelt.session-list`，对话界面是共享模块 `smelt.conversation`，文件树是 `smelt.files`。你可以写新的插件，也可以 fork 一个内置插件改它。

## 插件能做什么

- **加视图**：一块面板、一个设置页、一个按参数开的标签页。见[视图](views.md)。
- **加命令**：进命令面板、绑快捷键、放进应用菜单或别人的右键菜单、从命令行调。见[命令与菜单](commands.md)。
- **摆整个窗口**：登记页和 Dock 布局，决定主窗口里放哪些视图。见[页与布局](pages-and-layouts.md)。
- **读运行时**：会话、项目、Git、自动化、智能体设置，本机和远程机器都行。见[调用运行时](runtime.md)。
- **存数据**：每个插件一个 SQLite。见[存储](storage.md)。
- **换配色**：不写代码，只放主题文件。见[主题插件](themes.md)。

## 插件管什么，宿主管什么

| 插件管 | 宿主（Smelt 本体）管 |
| --- | --- |
| 什么时候做、做什么、界面上写什么字、菜单里列什么 | 系统服务：通知、菜单栏、Dock 角标、文件选择框 |
| 自己的视图长什么样 | 窗口、Dock 布局的拖拽和分栏、出口路由 |
| 自己的数据和表结构 | 每个插件的 SQLite 连接和数据目录 |
| 订哪些运行时话题、调哪些运行时方法 | 到各台机器的连接、重连、在线状态 |
| 命令的实现 | 命令注册表、快捷键、命令面板 |

宿主不认识任何具体插件：删掉任意一个插件，宿主一行代码都不用改。插件需要的东西宿主没有时，是加一项通用能力，不是在宿主里写那个功能。

## 插件怎么跑

```
smelt-plugin.json + nsh.tsx + 同目录模块
        │  宿主打包（nsh-bundler）、检查 import、编成 QuickJS 字节码
        ▼
  插件 VM ──host.call──▶ 宿主（Rust）──▶ 运行时 smeltd（本机或远程）
        ◀──状态/事件/命令──
        │
        ▼
  同一棵 NSH 界面树，GPUI 原生绘制
```

- **打包在运行时做**：装、改、删插件都不用重编 App。Smelt 开着时保存文件，约 200 ms 后只重编改了的那个插件。
- **每个用户插件一个 VM**：独立的堆、CPU 预算和节点配额。死循环、内存爆了、越权操作界面节点，只停掉这一个插件，别的插件和外壳不受影响。没被覆写的内置插件共用一个 VM。
- **VM 是加固的**：原型冻结。全局只有 Web 那一套：`fetch`、`setTimeout`、`requestAnimationFrame`、`console`、`crypto`、`URL`、`TextEncoder` 等。**没有** Node 的 `fs`、`process`、`os`、`child_process`，也没有 `Buffer`。
- **SDK 由宿主提供**：`import ... from "smelt/sdk"` 拿到的是当前 Smelt 版本带的那份，插件不自带。
- **界面是原生的**：插件写的 `<View>`、`<Text>` 和 NSH 组件，由宿主用 GPUI 原生画，不是网页。

## 能 import 什么

| 模块 | 内容 |
| --- | --- |
| `nsh` | 响应式原语（`signal`、`computed`、`effect`、`resource`、`batch`、`cleanup`…）和基础元素（`View`、`Text`、`Scroll`、`List`、`Canvas`、`Image`…）。见 [NSH API](https://nsh.dev/modules/nsh) |
| `nsh/base` | 桌面控件：`Button`、`Dialog`、`Popover`、`Menu`、`Select`、`Switch`、`Tabs`、`Tooltip`、`Editor`、`DiffView`、`Tree`、`DataTable`…。见 [nsh/base](https://nsh.dev/modules/nsh-base) |
| `nsh/desktop`、`nsh/i18n` | 桌面原语、国际化工具 |
| `smelt/sdk` | Smelt 宿主的一切：状态、事件、命令、运行时、存储、Smelt 组件。见[参考](reference/sdk.md) |
| `smelt/host` | 宿主的原生矢量图标 `<Icon>`、`<Vector>`；一般用 SDK 包好的 `SmeltIcon`、`SmeltVector` |
| 共享模块 | 别的插件用清单 `module` 字段开出来的，比如 `smelt/conversation`。见[插件之间](plugin-interop.md) |
| 相对路径 | 插件目录里的 `.ts`、`.tsx`、`.js`、`.json`，不能 `../` 出目录 |

npm 包和 Node 内置模块都不行。打包后还剩不认识的 import，或者用了没声明的全局变量，`smelt plugin check` 会拦下。

## 信任模型

> [!IMPORTANT]
> 插件不按权限隔离。VM 隔离的是故障（一个插件崩了不拖垮别的），不是能力。任何插件都能经 SDK 调运行时（含 Git 提交、推送、改设置）、发通知、打开文件和链接、启动本机应用。只装你信任的插件。

## 和 NSH 文档的关系

NSH 的元素、样式、Tailwind、响应式、动画、列表、手势，以 [nsh.dev](https://nsh.dev/docs/) 为准，这里不重复。Smelt 插件只用桌面那部分：没有手机、网页、`WebView`、`Video`。

## 下一步

- [第一个插件](getting-started.md)
- [目录与清单](project-structure.md)
- NSH 的 [Reactivity](https://nsh.dev/docs/reactivity) 和 [JSX and components](https://nsh.dev/docs/jsx-and-components)
