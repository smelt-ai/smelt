# `smelt/sdk` 参考

`smelt/sdk` 是宿主注入的模块，版本跟着 Smelt 走。准确的签名看插件里生成的 `.nsh/types/smelt/sdk.d.ts`（`smelt plugin types` 生成），这一页按用途列出每个导出，细节链到各指南。

标 **内部** 的是给内置插件用的：形状跟着内置功能变，不在兼容约定里，用户插件不要依赖。见[版本与兼容](../compatibility.md)。

## 启动

| 导出 | 说明 |
| --- | --- |
| `bootTask(work)` | 登记启动时要做完的事。宿主等全部做完（最多 3 秒）再挂视图。抛错只记日志。见[第一个插件](../getting-started.md#入口长什么样) |
| `initHost(slices?)` | 提前要几个状态切片，给出第一份状态。一般不用调，读访问器时自动要。见[宿主状态](../host-state.md#切片) |
| `hostReady()` | 宿主的第一份状态到了没有 |
| `pluginId()` | 自己的插件 id |
| `dataDir()` | 自己的数据目录 `~/.smelt/plugin-data/<id>/`。见[存储](../storage.md#数据目录) |

## 登记

都是整组登记：再调一次整组替换，空数组是撤下。

| 导出 | 说明 |
| --- | --- |
| `registerViews(specs)` | 视图。见[视图](../views.md#登记) |
| `registerCommands(specs)` | 命令。见[命令](../commands.md#登记) |
| `registerPages(specs)` | 页。见[页与布局](../pages-and-layouts.md) |
| `registerLayouts(specs)` | 布局。见[页与布局](../pages-and-layouts.md) |
| `registerTopics(topics)` | 要收的运行时话题。见[运行时话题](../runtime.md#运行时话题) |
| `registerContextMenuItems(menu, items)` | 往一个右键菜单里加项。见[右键菜单](../commands.md#右键菜单) |
| `registerMenuItems(items)` | 应用菜单里的项。见[应用菜单](../commands.md#应用菜单) |
| `registerFileOpener(name)` | 认领打开文件。见[打开文件](../commands.md#打开文件) |
| `setStatusMenu(menu)` | 菜单栏图标和它的菜单，`null` 撤下。见[菜单栏图标](../commands.md#菜单栏图标) |
| `setActiveContext(ctx)` | 报当前的项目和会话。见[活动上下文](../host-state.md#活动上下文) |

## 命令与事件

| 导出 | 说明 |
| --- | --- |
| `onCommand(handler)` | 处理自己的命令。见[处理调用](../commands.md#处理调用) |
| `run(id, args?)` | 调一条命令（插件的或[宿主的](host-commands.md)），给它的结果 |
| `commands()` | 登记了的全部命令 |
| `command(id)` | 一条命令，没有给 `null` |
| `contextMenuItems(menu, machine, context)` | 别的插件往你的菜单里加的项。见[画自己的菜单](../commands.md#画自己的菜单让别人往里加) |
| `onEvent(listener, types)` | 收宿主事件、运行时话题、插件广播，给出取消函数。见[宿主事件](../host-state.md#宿主事件) |
| `emitEvent(name, payload?)` | 广播。见[插件间广播](../host-state.md#插件间广播) |
| `onSlices(slices, listener)` | 切片变了时回调。见[订切片变化](../host-state.md#订切片变化) |

## 宿主状态

都是响应式的，在组件、`computed` 里读会跟着变。见[宿主状态与事件](../host-state.md)。

| 导出 | 说明 |
| --- | --- |
| `machineMeta()`、`machineMetaEntry(machine)` | 挂着的机器：名字、在线、系统、路径风格 |
| `machines()`、`machineEntry(machine)` | 机器连同它的会话目录、状态、对话、提醒 |
| `connections()` | 远程机器的连接情况 |
| `pathStyle(machine)` | 那台机器的路径风格：`"posix"` 或 `"windows"` |
| `workspaceDirectory(machine)` | 那台机器的工作区目录：项目、会话。形状跟着运行时变 |
| `sessionStates(machine)` | 那台机器的会话状态。形状跟着运行时变 |
| `conversationFacts(machine)` | 每段对话的显示用事实：标题、智能体、状态 |
| `sessionAttention(machine)` | 每段会话的提醒 |
| `activeContext()` | 当前项目和露着的会话 |
| `windowActive()` | 主窗口在前台 |
| `notificationPermission()` | 系统通知授权 |
| `views()`、`pages()` | 登记了的视图、页 |
| `viewShown(id)`、`viewLaidOut(id)` | 视图看得见；视图摆在布局里。见[视图的状态](../views.md#视图的状态) |
| `viewBadge(id)` | 视图的角标 |
| `safeArea(viewId)` | 视图四角要让出的区域。见[自己画顶栏](../views.md#自己画顶栏) |
| `theme()`、`currentTheme` | 当前主题。见[写界面：主题](../ui.md#在代码里读主题) |
| `locale()`、`systemLocale()`、`locales()` | 语言。见[多语言](../i18n.md#当前语言) |
| `t(key, params?)`、`text(key)`、`texts()` | 文案。见[多语言](../i18n.md) |

## 调运行时

见[调用运行时](../runtime.md)。

| 导出 | 说明 |
| --- | --- |
| `call(machine, method, params?, timeoutMs?)` | 调 Control API，失败抛错 |
| `callDetail(machine, method, params?, timeoutMs?)` | 同上，给 `{ ok, value }` 或 `{ ok: false, code, message }` |
| `request(machine, payload, timeoutMs?)` | 带 `op` 的守护命令，不在版本约定里 |
| `waitOnline(machine, failures?)` | 等机器上线 |

## 存储

| 导出 | 说明 |
| --- | --- |
| `store.query`、`store.execute`、`store.write`、`store.migrate` | 插件自己的 SQLite。见[存储](../storage.md) |

## 本机能力

| 导出 | 说明 |
| --- | --- |
| `fetch` | Web 标准全局，联网用 |
| `openUrl(url)` | 用浏览器打开 |
| `openFile({ machine, path, line? })` | 交给文件打开者 |
| `revealPath(path)` | 在访达里显示 |
| `clipboardWrite(text)` | 写剪贴板 |
| `discoverApps(queries)`、`launchPath(target)` | 找本机应用，用它打开路径 |
| `processUsage()` | Smelt 自己的 CPU、内存 |
| `renderMermaidSvg(source)` | Mermaid 画成 SVG |
| `localOs()` | 本机系统：`"darwin"`、`"linux"`、`"win32"` |
| `smeltVersion()` | Smelt 的版本号 |
| `newSessionId()` | 新的 UUID v4 |
| `qrMatrix(text)` | 二维码点阵 `{ size, bits }`，`bits` 里 `'1'` 是深色块；太长给 `null` |

## 路径

运行时的路径是**那台机器上**的，远程机器可能是 Windows。第一个参数 `style` 从 `pathStyle(machine)` 拿，不要自己按 `/` 切。

| 导出 | 说明 |
| --- | --- |
| `pathClean(style, path)` | 规整：去掉多余的分隔符、`.`、`..` |
| `pathJoin(style, base, child)` | 拼接 |
| `pathFileName(style, path)` | 最后一段，没有给 `null` |
| `pathParent(style, path)` | 上一级，没有给 `null` |
| `pathExtension(style, path)` | 扩展名，没有给 `null` |
| `pathIsAbsolute(style, path)` | 是不是绝对路径 |
| `pathStartsWith(style, path, base)` | `path` 在 `base` 下（按整段比，`/a/bc` 不在 `/a/b` 下） |
| `pathStripPrefix(style, path, base)` | 去掉 `base` 后的相对路径，不在它下面给 `null` |
| `projectLabels(style, roots)` | 一组项目的显示名：末段重名的往前补父目录（`dev · smelt`） |
| `projectRootOf(style, roots, path)` | `path` 属于哪个项目根，多个都匹配取最深的，都不沾给 `null` |
| `parseFileLink(style, target, cwd?)` | 解析 Markdown 里的文件链接，给 `{ path, line, fragment }` |
| `fileUriToPath(uri)` | 拖进窗口的 `file://` 解成本机路径 |

## 组件

见[写界面](../ui.md#smelt-组件)。

| 导出 | 说明 |
| --- | --- |
| `SmeltButton` | 按钮 |
| `SmeltIcon` | 自带图标 |
| `SmeltVector` | 画 SVG 源码 |
| `SmeltPopover`、`SmeltMenu`、`SmeltContextMenu`、`SmeltSelect`、`SmeltTooltip`、`SmeltDialog` | 浮层 |
| `POPUP_CLASS`、`MENU_CLASS`、`MENU_ITEM_CLASS`、`TOOLTIP_CLASS`、`INPUT_CLASS`、`DIALOG_SURFACE_CLASS` | 浮层的样子 |
| `SmeltDisclosure`、`SmeltDisclosureChevron` | 展开收起 |
| `SmeltMarkdown` | Markdown |
| `SmeltEditor` | 代码编辑器 |
| `SmeltDiffView` | 差异 |
| `SmeltWindowBar` | 自己画顶栏时让出红绿灯 |
| `ViewInstance` | 嵌别的视图 |

## 工具

| 导出 | 说明 |
| --- | --- |
| `stableMemo(fn)` | 像 `computed`，但结果和上次 JSON 一样时给回上次的对象，下游不跟着重挂 |
| `diffLineStats(oldText, newText)` | 增删了几行，和 `SmeltDiffView` 同一种比法。新文件 `oldText` 给 `null` |

## 内部

下面这些给内置插件用，不要依赖：

| 导出 | 给谁用 |
| --- | --- |
| `nativeApi`、`smelt/host` 模块 | 宿主的原始绑定，SDK 函数建在它上面 |
| `SmeltNativeView`、`SmeltNativeWindowBar`、`SmeltNativeWindowDrag`、`SmeltBackdropRegion`、`SmeltNativeDesktop`、`SmeltNativeTerminal` | 内置插件的原生组件 |
| `shellState()`、`jsViews()` | 宿主外壳自己的状态 |
| `acpSubscribe`、`acpUnsubscribe`、`acpAction`、`acpHistory`、`acpRestart`、`acpPasteImage`、`acpClipboardImages`、`acpImageUri`、`onAcpEvent` | 对话模块。见[调用运行时：对话](../runtime.md#对话) |
| `agentCatalog`、`agentForSpace`、`launchableProfiles`、`handoffPrompt`、`resolveStartAcp`、`resolveResumeAcp`、`resolveHandoffAcp`、`resolveCliResume`、`rememberedAcpConfig`、`rememberAcpConfigValue`、`conversationAcp`、`newSessionIcon`、`daemonStateStatus`、`agentIdFromLaunch`、`isInterruptMarker` | 新建会话、对话、会话列表 |
| `quotaTarget`、`quotaFacts` | 额度 |
| `isManagedAutomationWorkspace` | 自动化 |
| `customProviderIdProblem`、`apiBaseUrlProblem`、`piMcpPresets` | 智能体设置 |

这些函数是几个功能共用、或必须和运行时同一份的规则，放在宿主里只是为了不写两份。规则跟着内置功能改。
