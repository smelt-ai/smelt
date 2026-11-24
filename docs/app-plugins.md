# 整个 app 插件化

> 状态：方向定于 2026-11-09；2026-11-20 切到 **NSH A2**（`crates/smelt-nsh`）；2026-11-24 收敛为「扩展型业务功能留在 JS/TSX 插件（按需订阅状态切片与离散事件），声明式纯主题与纯布局零 VM，宿主核心 / 预首帧偏好 / 纯 OS 集成留在 `crates/smelt/src/`，对话能力拆分为纯非 UI 的 `crates/smelt-acp` 与可 `fork` 覆写的共享 JS 模块插件 `plugins/conversation/`（`"smelt/conversation"`，id `smelt.conversation`）」。
>
> 本文先写终态，再写从现在走到终态的顺序。每块拆到时，它的接口细节再写回本文。

## 目标

用户能用 JS 改 app 里的任意一个元素。

- smelt 的每一项功能都是 JS 插件。随 App 带的和用户写的是同一种东西，用同一套接口。
- 用户想改哪项功能，就改那个插件的 JS，或复制一份换个 id 再在布局里换上。
- Rust 只写宿主和能力：宿主提供机制和基础服务，能力给插件数据、动作和组件。宿主和能力都不认识任何具体功能。

上一版（2026-11-09 起）把功能从宿主搬成了 Rust 内置插件。宿主因此变小了，但 Rust 插件画的界面用户改不了：插件化只做到「能删能换」，没做到「能改」。

## 原则

1. **Rust 不认识功能。**
   - 宿主和能力的代码里不出现「会话列表」「文件树」「Git」这类功能名，也没有它们的枚举、字段或专用 action。
   - 判断标准：删掉任何一个插件，Rust 不用改一行。
2. **内置插件没有特权。** 随 App 带的插件和用户写的插件走同一套注册点和接口。内置插件需要的能力，就是开给所有插件的能力。
3. **状态归功能自己，数据只有一个权威。**
   - 一个功能的状态只存在它自己那里：插件的内存和插件自己的库。
   - 运行时的数据以 smeltd 为权威，客户端只留推送来的只读缓存。
   - 几个功能都要的派生事实（会话状态这类），由能力算好给，插件不各自再算一遍。
4. **等待一律事件驱动。** 宿主到插件的通知经 NSH 的响应式 `state()` 与离散事件监听（`onEvent` / `onCommand`）推送，不让插件轮询。
5. **搬走，不加层。** 拆一个 Rust 插件，就是把它整个分成能力和 JS 插件两部分，同一个提交删掉 Rust 版。不留两份并行的实现，不在中间加适配层。
6. **接口只开通用的。**
   - 加接口前，先看已有机制能不能统一。一个新机制和已有的平行，就是走偏的信号。
   - 不为某一个插件开专用方法。
   - 同一段逻辑第三次复制，说明缺了一个归属。要找出归属，而不是再复制一次。

## 分工：Rust 做什么，JS 做什么

一样东西放哪，先答三问：

1. 几个功能都要用吗？
2. JS 做不对吗？要原生接口、要宿主才看得见的状态、要和运行时协议用同一份规则。
3. JS 做不快吗？渲染量大、数据量大、每帧都跑。

有一问答「是」，放 Rust；都是「否」，写在 JS 插件里，宁可几个插件之间有些重复。

换个角度验证：用户把一个插件复制一份重写界面时，**不得不重新实现一遍**的东西，该在 Rust；他**会想改**的东西，该在 JS。

| 放 Rust | 放 JS 插件 |
| --- | --- |
| 数据的同步和缓存：工作区目录、会话状态、话题、机器 | 这项功能自己的数据和规则：会话列表的分组、置顶、排序、折叠，存插件自己的库 |
| 几个功能都要的派生事实：会话状态、agent 种类的名字和图标 | 视图里的一切：摆什么、怎么摆、间距、颜色、图标、文案、时间怎么写、显示哪些信息 |
| 动作：调运行时（`call`）、系统服务（通知、选路径、剪贴板、用别的应用打开） | 界面自己的临时状态：悬停、展开的菜单、输入框里的字、正在改名 |
| 重组件：终端、对话流、markdown、diff、高亮、远程桌面画面、3D 小人 | 把点击、拖动、快捷键接到动作上；右键菜单里有哪些项、怎么排 |
| 交互机制：滚动锚点、拖动、重排动画、弹层、菜单底层、输入框、焦点 | 给别的插件的约定：命令、菜单 id、插槽 |

**界面用细粒度的元素拼。**
- JS/TSX 视图用 `"nsh"`、`"nsh/base"` 的元素组件与 `"smelt/sdk"` 拼装。
- Rust 不做「一行会话」「一张自动化卡片」这种整块业务组件：做了，那块里面的东西用户又改不了。
- Rust 组件只做 JS 拼不出、或拼出来太慢的，外观能开成参数的都开成参数。

## 终态：宿主留什么

宿主只放三种东西：宿主自己要据此行动的、只有宿主看得见的、插件再小心也做不对的机制。别的给插件用的东西做成能力（下一节）。

| 类别 | 内容 |
| --- | --- |
| 窗口 | 只有主窗口：标题栏和交通灯让位、原生窗口外观（背景、透明度、窗口材质）、调试 HUD、退出流程 |
| 页面路由 | 主窗口一次一页，每页一块 `DockArea`；页的布局里可以摆一个出口（`smelt/outlet`）。进首页、按打开的视图换页或换出口里的内容、页里的「返回」回来路；装布局、存档、停靠区开合（见「摆放」） |
| 注册表 | 视图（含设置页）、页、命令和快捷键、菜单贡献、布局、主题、文件打开方式、视图角标。能力和插件登记进同一张表 |
| 命令面板 | 命令注册表上的一个通用界面 |
| 设置 | 视图注册表上的一个通用界面，单独一页、两个视图：导航（`smelt/settings-nav`）和内容（`smelt/settings`）。和命令面板一样不认识任何一页 |
| 插件运行时 | 发现、加载、卸载、热重载；按插件分开的 Policy、日志身份、`smelt-fs`；每个插件一份自己的 SQLite；随 App 带的插件放进安装包（见「插件运行时」） |
| 运行时连接 | 本机 smeltd 的启动、升级、重启；远程机器的配对、连接、中继、升级；按插件登记的话题订阅、转发运行时推送；`call` 调任意控制方法；会话状态和工作区目录的镜像（见「运行时数据只是话题」的例外） |
| 系统服务 | 系统通知和权限、菜单栏图标、Dock 角标、选路径（远程机器用宿主画的目录浏览）、用编辑器或文件管理器打开、剪贴板、对话框 |
| 应用生命周期 | 自动更新、挪到「应用程序」、退出前检查有没有活在跑、应用日志 |
| 宿主机制自己的页 | 外观、快捷键、插件管理（`crates/smelt/src/settings/`）。背后是宿主自己的机制（录快捷键、导入卸载插件），拆成插件就要给每页开一组只有它一个调用方的接口（原则 6）。宿主以来源 `smelt` 登记进同一张视图表 |

最终的 `Workspace` 只剩窗口、`DockArea`、布局、可见性登记和退出、更新这几件事。

## 终态：能力层

能力是给插件用的 Rust：数据、动作、组件。

- 放 `crates/` 根下，按能力命名，不按功能命名。
- 只依赖 `smelt-plugin-api` 和更底下的能力 crate，由应用 `smelt` 组装。
- 能力不认识任何插件：删掉一个插件，能力不用改；删掉一个能力，宿主不用改。
- 每样能力在 JS 那边都有映射（见「JS 映射」）。只有 Rust 能用的能力不算数，因为没有 Rust 插件会用它。

| 能力 | 给插件的东西 | 现在在哪 |
| --- | --- | --- |
| 会话数据 | 工作区目录、会话状态，带上几个功能都要的派生事实（名字、状态、哪家 CLI、是不是自动化跑的、工作目录、摆在侧栏哪儿） | 宿主的镜像 + `smelt-model` + `smelt-plugin-api` 的 `facts`；JS 经 `"smelt/sdk"` |
| 路径 | 按运行时那台机器的系统切路径（`pathClean`、`pathJoin` 这些）、项目显示名 | `smelt-model` 的 `runtime_path`；JS 经 `"smelt/sdk"` 的 `path*`、`projectLabels` |
| 插件的库 | 自己的 SQLite：查、写、事务、按版本迁移 | `smelt-client-store` 的 `PluginStore`；JS 经 `"smelt/sdk"` 的 `store` |
| 终端 | 终端 GPU 画布、PTY 流、IME、Cmd+F 搜索与断线重连 | `crates/smelt-terminal`；JS 经 `smelt.terminal`（`SmeltNativeTerminal`） |
| 对话 | 纯非 UI ACP 连接/帧镜像/节流中心/图片缓存（`crates/smelt-acp`）+ Mermaid SVG 渲染（`smelt-ui`）+ 可 fork 共享 TSX 模块 `<SmeltConversation>`（`plugins/conversation/` -> `"smelt/conversation"`） | `crates/smelt-acp` + `plugins/conversation/` |
| 组件库与原生件 | NSH 基础件（`"nsh"`、`"nsh/base"`）+ `smelt/host` 矢量图标 + 原生能力件（`smelt.view`、`smelt.terminal`、`smelt.desktop`、`smelt.buddy`、`smelt.window-bar`、`smelt.window-drag`、`smelt.backdrop-region`） | `crates/smelt-nsh` + `crates/smelt-ui` |
| Git | 读写仓库 | `crates/smelt-git`，还没映射给 JS |
| 远程桌面 | 画面与本地转发口组件 | `crates/smelt-desktop`；JS 经 `smelt.desktop`（`SmeltNativeDesktop`） |
| 小人 | 会动的 3D 小人：零件、动作、合成、出帧节拍；演哪一出由插件给 | `crates/smelt-buddy`；JS 经 `smelt.buddy`（`SmeltNativeBuddy`） |
| 资源量测 | 进程 CPU、内存 | 宿主桥 `sys.process_usage`（`crates/smelt-nsh`） |
| 宿主内置系统模块 | 在线更新与退出守卫、通知与菜单栏/Dock、CLI 安装与清理、预首帧界面语言、终端字号与配色同步 | 宿主内建模块（`crates/smelt/src/{updater,notifications,maintenance,language,terminal_settings}`，沿用 `smelt.*` 视图/数据目录，0 VM） |

能力自己要登记视图、命令时，用自己的来源 id（如 `smelt.terminal`），和插件一样进注册表。

## 终态：插件接口

插件在 NSH A2 运行时中导入 `"nsh"`、`"nsh/base"`、`"smelt/sdk"`、`"smelt/host"`。Rust 侧的 `crates/smelt-plugin-api` 是能力 crate 登记用的接口，NSH 宿主桥接在 `crates/smelt-nsh/src/`（`host.rs`、`module.rs`、`plugins/sdk.tsx`）。只有主壳内部使用 `smelt/slots` 挂载顶层插件面板；插件不能互相嵌套其它插件。

### 读、变化、动作分开走

- **读与响应式状态**：纯同步模型函数走 `"smelt/host"`；宿主共享状态通过 `"smelt/sdk"` 的响应式 `state()` 信号读取，组件依赖的字段变化时自动重算。
- **离散变化**：通过 `onEvent` / `onCommand` 监听宿主事件与命令派发，不轮询。
- **动作**：一律是命令或宿主桥接调用，处理函数交回结果。
  - 主窗口关着时，用不到窗口的命令照样执行；本来就要把人带到主窗口的命令先开一扇窗。
  - 谁调用谁处理失败：插件调的，插件自己提示；快捷键、菜单、命令面板执行的，由宿主统一发错误通知。

### 给插件的读和事件

| 类别 | 内容 |
| --- | --- |
| 响应式状态 | `locale`、`locales`、`machines`、`connections`、`active_context`、`pages`、`views`、`commands`、`js_views`、`safe_areas`、`bar_insets`、`theme`、`texts`、`shell`（见 `SmeltState`） |
| 离散事件 | 宿主推送的运行时话题、视图打开参数变更、命令派发与生命周期事件 |
| 命令 | 通知；菜单栏和 Dock 角标；选路径；在编辑器或文件管理器里打开；`smelt.app/open-view`（打开设置页也是它）；显示主窗口；机器管理 |
| 路径与存储 | 插件数据目录（`dataDir()`）、独立 SQLite（`store.migrate` / `query` / `execute`） |

### 运行时数据只是话题

会话状态、工作区目录、智能体配置、额度、自动化，都是 smeltd 的数据：

- 插件登记要哪些话题（`register_topics`），宿主按并集订阅、原样转发，不解读，也不缓存：新登记的插件靠重订拿到快照。
- 插件之间不各自镜像一份。
- 例外：会话状态和工作区目录（`session_states`、`workspace_directory`）留作宿主接口。三问都答「是」：宿主自己据此行动（重启守护进程前看有没有会话在跑）；只有宿主看得见（多台机器的订阅合并、机器上下线的代数）；插件再小心也做不对（每个插件各自合并一遍，结果会不一样）。宿主留一份镜像（`crates/smelt/src/daemon_states.rs`），插件只读。
- 会话的显示名、状态、最后更新时间由 smeltd 算好放进推送（`smeltd/src/session_names.rs`），各端直接用，不自己拼。`conversation_name` 是整段对话的名字，`name` 是这一格的名字。
- 还没有话题的数据，由 smeltd 补上话题。

### 视图一张表

- 视图和设置页是同一种东西，按来源整组登记：`register_views(视图)`，id 是 `<来源>/<名字>`，和命令、布局、主题一样。
- 设置页就是带 `settings: {组, 搜索关键词}` 的视图。打开带 `settings` 的视图，就是打开设置、停在这一页。
- 可见性只有一套：`view_shown` 和 `shown_views_changed`。
- 视图第一次露出来时建，之后一直留着，直到登记被替换或撤下。打开时从插件自己的库里读，改了马上写回，不靠视图活着保存东西。
- 每个视图是插件脚本里导出的函数组件，登记在 `registerViews(...)` 里。名字由插件自己起；`main` 不再是保留名，会话列表沿用 `smelt.session-list/main`，布局不用改。
- 视图登记里写标题、图标、是否自己画顶栏（`draws_top_bar`）。自己画顶栏的视图顶上放 `SmeltNativeWindowBar`（`smelt.window-bar`）。
- 视图打开时的参数由宿主随视图挂载/事件传入，不靠写死全局状态。
- 视图登记里不写位置。入口放在哪，是菜单贡献和布局的事。
- 按实例建的视图（`instances`，如远程桌面 `smelt.desktop/desktop`）不进布局、没有入口：用 `smelt.app/open-view` 带参数打开，宿主主壳按每次打开建一份新实例放进出口。插件内部需要嵌终端、3D 小人、远程桌面等重画布时，直接使用对应原子能力组件（`<SmeltNativeTerminal>`、`<SmeltNativeBuddy>`、`<SmeltNativeDesktop>`），不嵌套其它 JS 插件。

### 菜单贡献

插件要在别人画的地方放一个入口（机器菜单里的「远程桌面」、项目菜单里的「用 IDE 打开」），用菜单贡献：

- 登记：`register_context_menu_items(菜单 id, 菜单项)`，按（来源，菜单）整组替换。每一项写标题、图标、要执行的命令和参数，或者一层子菜单；可以只在某台机器上列。条件变了，插件自己重新登记。
- 菜单 id 是字符串，由画菜单的那个插件定义和公布（会话列表的 `smelt.session-list/conversation`、`smelt.session-list/project`、`smelt.session-list/machine`），上下文的形状写在同一处。宿主只存这张表。
- 画菜单的插件用 `context_menu_items(菜单 id, 上下文)` 取项，自己画成菜单项，点了执行命令，参数是登记的参数并上上下文。失败挂在发起的地方（会话列表是那一行），找不到落点的才发系统通知。菜单在打开时取项，开着的不跟着登记变。
- 应用菜单（App、帮助）是另一张表：`register_menu_items(菜单项)`。
- 不为侧栏底部这类地方开「区域贡献」：一块想单独露出来的东西，就做成视图，由布局摆。

### 扩展点

插件往别处加东西的地方都是扩展点：命令、菜单、页、视图、设置页、主题、布局、文件打开者、退出守卫。每个扩展点先定它是哪一种：

- **并存的**：命令、应用菜单项、右键菜单项、页、视图、设置页、主题、布局。来几个摆几个，装上就有、删了就没。退出守卫也是并存的：退出时挨个问，有一个接手就停。
- **只放一个的**：眼下用哪个主题、哪份布局、打开文件交给谁。好几个提供的时候，用哪个**由用户选**：
  - 选择归扩展点的主人：宿主的在宿主的设置里选，插件的在那个插件自己的设置页里选、存进它自己的库。
  - 没选过用默认的（主人自己定，一般是第一个）；选的那个卸了也按没选过算，原来的选择不删，装回来接着用。
  - 不按登记顺序、谁后来去定谁优先；同来源内重名不加载、报重复。用户插件若与随 App 带的插件同 id，则视为**原地覆写自带插件**（详见「随 App 带的插件」），删掉用户插件即恢复自带版；用户覆写版加载、编译、主题校验或启动失败时自动回退到自带版本并在状态中保留错误。
  - 没有「停用某个插件」的开关去绕：不想要哪个提供的就选别的，或者删掉它。
- **原子原生能力**：多个插件或高频视图直接复用的重画布（终端 `<SmeltNativeTerminal>`、远程桌面 `<SmeltNativeDesktop>`、3D 小人 `<SmeltNativeBuddy>`），留在 `crates/smelt-*` 并以原生组件暴露给所有插件直接组合；不存在「一个 JS 插件用 Slot 嵌另一个 JS 插件」。

加一个新的扩展点，先按上面定它是哪一种；只放一个的，同一次改动里把选择做进主人的设置。打开文件（`register_file_opener`）眼下只有一个登记方，选择等有第二个时再加，在那之前按最后登记的用。

### 插件之间的约定

插件之间不互相依赖，也不互相嵌套 UI，只经命令、事件、菜单 id 与运行时话题打交道：

- 一个插件对外给的视图 id、命令 id、菜单 id、插槽 id 和参数形状，权威定义放在提供方自己的 `contract.js`（JSDoc 写清参数和上下文）。调用方照抄用到的 id 和字段，注明出处。
- 接口不点名任何功能，宿主和能力不实现、也不调用这些约定。
- 提供方改约定，要在同一次改动里搜出调用方的字符串一起改。
- 删掉提供方，调用方拿到「命令没有登记」，Rust 不用改。
- 命令 id 是用户快捷键的存法，改名就丢了用户改过的键。迁移时一律沿用原 id。

### 活动上下文

- 活动上下文（当前项目、正在看的会话、是不是停在最新）是接口里的一个全局槽，由会话插件发布（`set_active_context`），宿主只转发，不解读。
- 文件、Git、历史、通知这些插件据此决定看哪个目录、把哪段会话标成已读。

### 主题

- 主题文件是 smelt 自己的带版本 JSON（格式、校验、推法见 `docs/ui-design-language.md` 第 2 节）。插件目录下的 `themes/*.json` 是主题贡献；有一份不合法，这个插件的主题一个都不装，登记报错。
- 纯主题包在 `smelt-plugin.json` 里声明主题文件，不需要脚本入口。
- 插件重载或卸载时，撤下它的主题。当前主题不在了，就回落到同明暗的 Default；原来的选择保留，重新装上后恢复。
- 主题只在启动和插件变化时读，render 里不读文件。

## 插件运行时（NSH A2）

### 单引擎 + 共享内置 VM + 每用户插件独立 QuickJS VM

- 实现位于 `crates/smelt-nsh`（基于 `smelt-ai/nsh` fork）：单引擎线程 + 单界面树（`AppView`）+ **1 个共享内置 VM（`__builtins__`，承载未被覆写的内置脚本插件）** + **每用户插件各自独立加固 QuickJS VM**（含同 `id` 覆写内置与引用了用户覆写共享模块的消费方）；主壳在 `crates/smelt-nsh/shell/MainShell.tsx`，承载各页 `nsh/base <DockArea>` 并与 `DockAreaState` 无损互转。
- 插件入口使用 TSX/JS，启动和热重载时由 `nsh-bundler` 自动打包，装改删不用重编 Rust。
- 每个插件的多个视图与设置页共享自己的模块状态，按作用域 SDK 隔离 `pluginId`、`dataDir`、`texts`、`store`、`bootTask`、`onCommand` 与事件路由；只有宿主外壳通过内部挂载机制将顶层插件视图摆进 `DockArea` / 出口 / 设置页，插件间不嵌套，重画布统一由原子原生能力组件承担。
- 加载失败的插件没有视图可登记，错误在设置的插件页、`smelt plugin status` 和 `~/.smelt/app.log` 里看。

### 随 App 带的插件

- 放仓库根 `plugins/<name>/`，发版打包时经 `scripts/stage-plugins.sh` 将全部内置脚本插件合并预编译为顶层共享字节码（`builtins.nshc` + `builtins.meta.json`）并保留各插件可编辑源码放入安装包（macOS 是 `Contents/Resources/plugins/`，Windows 是安装目录下的 `plugins\`），发版的 App 优先加载预编译共享字节码、单内置 VM 零打包冷启（`crates/smelt/src/plugins/builtin.rs`），同时支持 `smelt plugin fork` 离线复制源码；开发用的包（调试构建和 `make dev`）直接读仓库，改了即热重载，只有改 Rust 要重编。
- 在第一帧之前加载完，布局里摆的视图第一帧就画得出来，不闪空白。
- 同来源（`~/.smelt/plugins/` 内或自带目录内）重复 id 报重复错误跳过；当 `~/.smelt/plugins/` 存在与自带插件同 id 的目录时，**用户插件优先加载进独立用户 VM 并将该插件从共享 `__builtins__` 中自动摘除**（包括首帧前 `register_initial_themes` 的主题包；若覆写的是共享模块如 `smelt.conversation`，则只将依赖该模块的消费方插件拆出独立 VM，其余内置插件继续留在 `__builtins__`）。一旦用户覆写版在主题校验、文案解析、TSX 打包或 VM 启动阶段失败，宿主会自动回退并将内置版收回共享 `__builtins__` 保证功能不断，同时在 `LoadedPluginInfo`（`overrides_builtin`、`fallback_to_builtin`、`error`）、设置页和 `smelt plugin status` 中明确标出原因；用户后续继续改写该覆写插件不会重建 `__builtins__`，删除用户目录会立即恢复纯内置单 VM。

### 画面和数据

性能基准在 `crates/smelt-nsh/bench/` 与 `crates/smelt-nsh/examples/bench.rs`，写法规则：

1. 长内容用虚拟列表，或按行拆细粒度组件/信号。
2. 持续变的条目按局部信号更新，避免变化一次整棵树重绘。
3. markdown、diff、高亮、终端用 Rust 组件（`smelt.view`）。
4. 大量数据留 Rust，JS 只拿要画的部分。

### 文案

插件的文案放 `locales/ui.yml`，格式和 Rust crate 的一样。`"smelt/sdk"` 提供 `t(键, 参数)` 按当前语言读取调用方插件自己的文案表；登记给宿主的标题（视图、命令、菜单项）用 `text(键)` 构造多语言表，宿主显示时按当前语言取，详见 `docs/i18n.md`。

## 插件的样子

**用户插件**与**随 App 带的插件**同一种目录结构，清单文件固定为 `smelt-plugin.json`：

```text
plugins/files/
  smelt-plugin.json   清单（id、name、version、entry / themes / pages / layouts 等）
  nsh.tsx             入口（复杂插件可放 nsh/main.tsx 并按职责拆分模块；纯主题或纯布局可省略 entry）
  locales/ui.yml      文案
  tests/              纯逻辑单测（用 bun test）
```

1. **只 import 自己目录里的文件和宿主模块**（`"nsh"`、`"nsh/base"`、`"smelt/sdk"`、`"smelt/host"`），不设插件间共享目录，也不开放 `smelt/slots`：要复用的进组件库或原子能力。
2. **发的是源码 + 预编译字节码**：仓库直发 TSX/JS 源码；发版安装包同时带合并预编译的 `builtins.nshc` 与各插件源码（跳过 `tests/`），兼顾单内置 VM 零打包冷启动与通过 `smelt plugin fork` 直接修改任意内置功能。
3. **逻辑和画面分开放**：数据、规则放不 import NSH 的纯 JS/TS 文件，宿主函数从参数传进来，用 `bun test` 测。
4. **迁过来的插件沿用原 id**，视图、命令、菜单、插槽的 id 都不变，同一个提交删掉 Rust 版。插件的数据目录、布局里的视图 id、用户的快捷键都不用迁。
5. **数据放 `~/.smelt/plugin-data/<id>/`**（`dataDir()`，库是里面的 `store.sqlite3`，用 `store.migrate` / `store.query` / `store.execute`），见 `docs/local-storage.md`。

改自带插件可用 `smelt plugin fork <原id>` 将源码拷贝到 `~/.smelt/plugins/<原id>/` 原地覆写（自动继承原数据目录、可自动回退并可随时删掉目录一键恢复），也可用 `--as <新id>` 另建副本。步骤写在 smelt skill（`agent/skills/smelt/SKILL.md`）里，由智能体照着做。

### 检查

- **单测**：`bun test plugins/<name>/tests`，测逻辑文件。
- **清单与打包校验**：`smelt plugin check <目录>`（`crates/smelt/src/plugins/check.rs`）离线检查 `smelt-plugin.json`、`locales`、主题文件，并运行 `nsh-bundler` 与 `oxc` 外部 import 校验。
- **对照出图**：从 Rust 插件迁过来时，同一份场景让两版各画一张图比对，删 Rust 版之前做。

## 终态：各功能怎么分

### 内置插件（`plugins/*`）与宿主内置系统模块（`crates/smelt/src/`）

- 19 个内置插件（`plugins/*`：默认由 **1 个共享内置 VM** 承载全部 `16` 个内置脚本插件，另有 `1` 个共享 JS 模块 + `1` 个 0-VM 纯主题 + `1` 个 0-VM 纯布局；用户覆写哪个插件或共享模块时，仅对应受影响插件拆出跑独立用户 VM）：`agents`、`archive`、`automations`、`capabilities`、`conversation`（共享模块插件 `smelt.conversation` -> `"smelt/conversation"`，零独立 VM，纯 TSX `<SmeltConversation>` 与对话组件，供 `sessions`、`automations` 及任意第三方插件直接导入或 `smelt plugin fork smelt.conversation` 定制）、`desktop`（画布留在 `crates/smelt-desktop`）、`files`、`git`、`history`、`ide`、`launcher`、`layouts`（零 VM 声明式布局插件，`smelt-plugin.json` 声明 `pages` / `layouts`）、`machines`、`quota`、`resources`、`session-list`（直接驱动 `crates/smelt-buddy` 3D 小人）、`sessions`（直接驱动 `crates/smelt-terminal` 终端画布，并导入 `"smelt/conversation"` 的 `<SmeltConversation>`）、`skills`、`themes`（零 VM 声明式纯主题插件）。
- 5 个宿主核心 / 纯 OS 集成与预首帧偏好模块保留在宿主 `crates/smelt/src/` 中（沿用原 `smelt.*` 视图 id 与 `~/.smelt/plugin-data/smelt.<name>/store.sqlite3` 路径，零 VM、零 IPC 开销）：`language`（预首帧恢复界面语言与系统切换）、`terminal_settings`（直接同步 `crates/smelt-terminal` 主题与全局字号）、`maintenance`（CLI 软链安装与 `~/.smelt` 残留清理）、`updater`（安装包校验/替换、移入「应用程序」与退出守卫）、`notifications`（`attention.changed` 系统通知、macOS 菜单栏与 Dock 角标）。
- 对话底层宿主能力：纯非 UI 的 `crates/smelt-acp` + `smelt-nsh` 宿主能力 `acp.*` / `mermaid.render`（经 `"smelt/sdk"` 暴露），上层 UI 全部在 `plugins/conversation/` 中，零跨 VM 嵌套。

## 会话核心

- **权威在 smeltd。** 工作区目录只有 smeltd 一个写者。客户端只发操作（新建、改名、归档、置顶、关闭、改配置），等推送回来。
- **列表只读推送的数据。** 名字、状态、时间由 smeltd 给。选中哪个、展开哪个、怎么分组、排序，是插件自己的状态，存插件自己的库。
- **会话视图是会话插件的一个视图**（`smelt.sessions/session`），是首页出口的默认内容。列表是单独的插件（`smelt.session-list/main`），选中归会话插件，列表经 `smelt.sessions/selection`、`selection-changed` 跟着。
- **会话视图用对话和终端能力**，显示选中的那段会话；分屏归会话插件。
- **别的插件怎么用会话**：跳到某段会话、开新对话、续上历史、交接、往会话里发文字，都是会话插件登记的命令。

## 摆放

插件提供视图，视图摆在哪一页、页里怎么摆是页和布局的事。

- **整个 app 是一个嵌套的路由。** 外层是页：主窗口一次显示一页，每页有自己的布局。内层是出口：页的布局里可以摆一个出口（`smelt/outlet`），出口里显示路由放进去的视图。状态在 `crates/smelt/src/router.rs`。
  - 页按来源整组登记（`register_pages`），有名字、标题、是不是首页，和出口的默认内容（`index`）。插件用 `smelt.app/open-page` 进一页。
  - 启动进首页：登记在前的那个说自己是首页的。首页没有、或没有给它的布局时，工作台直接显示明确的空态/排错提示和「面板插件」入口（不做假兜底页），等布局插件恢复或登记后自动落位。
  - 打开一个视图：哪页的布局里摆了它，就去那页露出来；哪页都没摆，就放进出口。
  - 换页记来路，页里的「返回」退回来的那页。出口里换内容不换页、不记来路。`Cmd+W` 不管路由，只关看得见的会话或分屏。
  - 开过的页留着它的 `DockArea` 和出口里放着的东西。
  - 用户把出口里的视图拖进了布局，出口放回默认内容。
- **每页是一块 `DockArea`**，布局是一棵任意的拆分树，用官方的 `DockAreaState` 存。现在的三栏只是出厂给工作台那页的布局。
- **视图里面怎么画归插件，视图之间怎么摆归布局。**
  - 布局文件是纯数据，引用视图 id。布局按来源整组登记（`register_layouts`），登记时写给哪一页。
  - 首页用哪份在外观里选，存在 `appearance.layout_id`。
- **收起是停靠区的，不是视图的。** 左右停靠区各自记着开合和宽度，外壳画开合按钮。面板不能关，只能拖动、放大，或随停靠区收起。自己画顶栏的视图单独成一组时，外壳不给它画标题带。
- **还没做的**：「把视图加进布局」的命令；下停靠区的开合按钮。

## 想扩展时选哪种

先看要改的是什么：

- **智能体怎么干活**（多一个工具、换提示、加技能、换模型）：写 Pi 的扩展或技能。smelt 只负责装、列、开关。
- **到点或有事时自动做**：用自动化。在 smelt 外面用脚本驱动 smeltd 的，走 Control API（`docs/control-api.md`）。
- **App 里多一样东西、改一样东西**：写 JS 插件，或复制随 App 带的插件改。
- **JS 做不到**：补进 Rust 能力层，同时补 JS 映射，不另开口子。

## 迁移

### 已经做完的（2026-11-09 ~ 11-19）

功能从宿主搬成了 Rust 插件（`crates/plugins/*`）：视图一张表、设置进主窗口、页面路由、命令注册表、菜单贡献、插槽、话题登记、目录只有 smeltd 写、会话名由 smeltd 给，宿主里的会话代码全部搬走。`Workspace` 只剩窗口和工作台机制。宿主源码不点名插件的 id，由 `scripts/lint-crate-deps.sh` 守着。JS 那边补齐了插件接口的映射和一批 `smelt-ui` 组件。

### 方法：一个 Rust 插件拆成能力和 JS 插件

1. **列清单。** 把它做的每样东西按「分工」三问分到 Rust 或 JS。拿不准的问用户。
2. **补能力。** Rust 那部分补进能力层，同时补 JS 映射，只补真用到的。补之前先看能不能并进已有的机制。
3. **写 JS 插件。** 沿用原 id，视图、命令、菜单、插槽的 id 都不变。行为和 Rust 版保持一致：做不到一样、或觉得旧行为该改的，先停下来问用户，改进等迁完另提。Rust 版的测试里，纯逻辑的那些改写成 JS 单测。
4. **对照。** 同一份场景让两版各画一张图比对，在真的 App 里过一遍功能清单，测性能（每步耗时、空闲有没有周期唤醒）。
5. **切换。** 同一个提交删掉 Rust 插件和应用 `smelt` 里那行 `register`。开发途中只在本地注释掉 Rust 版的登记，不提交两份同 id 的插件。
6. **自查。** Rust 里还有没有它的名字，有没有给它开专用接口。

### 剩余迁移顺序

1. **小插件**：`ide`、`maintenance`。
2. **中等的**：`history`、`notifications`、`skills`、`resources`、`desktop`、`updater`。
3. **大的**：`agents`、`automations`、`capabilities`、`files`、`git`。
4. **会话与对话**：`sessions`、`terminal` 外围、对话页（见「待定」）。

## 待定，要确认后再动

- **对话页拆到哪一层**：对话流里消息怎么排、工具调用卡片长什么样，用户可能想改，按目标该进 JS；但它渲染量最大，组件粒度要按实测定。放到最后另议。
- **内置插件能不能单独关掉**：没做。真要开关得存「关了哪些」，客户端库加表，要先确认。

## 已知缺口

- 绕过 smeltd 的修改（手改 Pi 配置文件、直接用 `pi` 命令行）现在收不到，要 smeltd 去监视文件才能补上。
- 手机端是 Flutter，跑不了 NSH。插件只影响桌面，手机直接用 smeltd 协议。

## 参考

- 社区做法：
  - 设置页：VS Code、Raycast、Zed 是声明式的配置项清单，由宿主画表单；JetBrains、Obsidian、浏览器扩展是插件自己画页。这里选后者。
  - 菜单贡献：参照 VS Code 的 `contributes.menus`。菜单 id 由画菜单的一方定义，宿主只存这张表。
