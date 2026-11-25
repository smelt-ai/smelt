# Smelt 插件文档

Smelt 的界面和功能由插件组成：会话列表、对话、文件、Git、通知、主题、布局都是插件。你写的插件和这些内置插件用同一套接口、跑在同一个运行时里。

插件用 TSX 写，界面框架是 [NSH](https://nsh.dev/docs/)（Smelt 用的版本是 0.6.1）。NSH 管怎么画（元素、样式、响应式），这套文档只讲 Smelt 在 NSH 之上给了什么：宿主状态、命令、视图、运行时、存储、主题。

## 入门

| 页面 | 内容 |
| --- | --- |
| [介绍](introduction.md) | 插件能做什么，插件和宿主各管什么，插件怎么跑 |
| [第一个插件](getting-started.md) | 从 `smelt plugin init` 到在 App 里打开它 |
| [目录与清单](project-structure.md) | 插件目录、`smelt-plugin.json` 字段、四种插件、发现和热重载 |
| [命令行](cli.md) | `smelt plugin` 的子命令 |

## 指南

| 页面 | 内容 |
| --- | --- |
| [视图](views.md) | 登记视图、打开时的参数、单例和多实例、设置页、无头根 |
| [命令与菜单](commands.md) | 登记命令、处理调用、快捷键、命令行调用、应用菜单、右键菜单、菜单栏图标 |
| [页与布局](pages-and-layouts.md) | 登记页和 Dock 布局，零 VM 的布局插件 |
| [宿主状态与事件](host-state.md) | 读宿主状态、订阅变化、收宿主事件、插件间广播 |
| [调用运行时](runtime.md) | 机器、Control API、运行时话题、ACP 对话 |
| [存储](storage.md) | 插件自己的 SQLite 和数据目录 |
| [写界面](ui.md) | NSH 写法要点、Smelt 组件、图标、主题令牌 |
| [主题插件](themes.md) | 不写代码，只给 App 换配色 |
| [多语言](i18n.md) | `locales/ui.yml`、`t()`、`text()` |
| [插件之间](plugin-interop.md) | 共享模块、契约文件、互调命令、覆写和 fork 内置插件 |

## 工具

| 页面 | 内容 |
| --- | --- |
| [调试与测试](debugging.md) | 校验、看加载状态、日志、常见错误、单元测试 |

## 参考

| 页面 | 内容 |
| --- | --- |
| [`smelt/sdk`](reference/sdk.md) | SDK 全部导出，按用途分组 |
| [宿主命令](reference/host-commands.md) | `smelt.app/*` 命令、参数和结果 |
| [版本与兼容](compatibility.md) | 插件跟着 Smelt 升级时会遇到什么 |

## 相关

- 宿主这一侧怎么实现、Rust 和 JS 怎么分工：[`../app-plugins.md`](../app-plugins.md)
- 运行时 Control API：[`../control-api.md`](../control-api.md)
- 主题文件格式和设计语言：[`../ui-design-language.md`](../ui-design-language.md)
