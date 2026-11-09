# 可定制、可切换的工作台布局

状态：设计稿 v3。布局文件和存档直接用 gpui-base Dock 的官方格式 `DockAreaState`，整个工作台是一个 `DockArea`。v1 把 Dock 的左、右、下、中四个区域当格式；v2 自己写了一棵 `row` / `column` / `stack` / `block` 的树，还把组件和外观写进了布局。两版都推倒，原因见「为什么推倒 v1、v2」。对应 #260，总任务 #257；分享导入是 #259，事件订阅是 #261，看板是 #258。

## 要解决的问题

不改 Rust，就能把整扇窗口换成另一种排布：会话做成看板、工具面板挪到别处、加顶栏、状态栏，舞台分成几块。

验收（照 #260）：

- 不改 Rust 源码就能新建一套布局并切换过去。
- 重启后恢复上次的有效布局；无效布局不会让工作台不可用，默认布局随时能回去。
- 布局切换、恢复、组件不可用、加载失败都有用例。
- 拿到一张别人的工作台截图，它的结构能只靠布局文件加插件拼出来，宿主不用为它加东西。

整体架构（宿主接口、命令、组件、外观、包、信任）见 `plugin-platform.md`，本篇只管布局这一层。

## 为什么推倒 v1、v2

- **v1** 照着 Dock 的左、右、下、中四个区域写格式，每拿到一张新的截图就要加一个专用字段（顶栏 `top`、状态栏、侧栏页脚……）。
- **v2** 换成自己的通用树，但格式里掺进了组件（`accepts`、`props`、`plugin:*`）和外观（底色、边框、圆角、间距、背景图、`window_drag`），宿主还要自己摆、自己拖、自己按节点 id 另存点出来的状态。这是在重写 Dock。

gpui-shell 已经给出了答案：布局是纯数据，只管空间怎么分；面板是一个名字加它自己的不透明数据；外观归皮肤；用户改过的结果就是布局。`DockAreaState` 在源码里写明是随产品发给用户的落盘格式，序列化标签冻结不改（`state.rs` 的 `the_serde_tags_are_frozen`），gpui-shell 的插件面板本来就靠它持久化。所以 v3 直接用它，不另造字段（`AGENTS.md` 里的例外）。

### 边界

- **布局（官方格式）管：** 怎么切（`StackPanel` 横 / 竖，任意嵌套）；哪几个面板叠成一组（`TabPanel`）、选中哪个；可选的 dock 区域。每格多宽不由布局文件定，由用户拖出来，记在存档里。
- **面板管：** 自己的一切，配置写在它自己的 `info.panel` 里，布局只原样带着。舞台收哪几类东西（`accepts`）就是舞台的配置。
- **皮肤管：** 长什么样。底色、间距、圆角、标签条、分隔条，由 smelt 实现的 `DockAreaRenderer` / `TabGroupRenderer` 统一画，取主题。插件主题改颜色，不改布局。

**规则：官方格式里没有的东西，不往文件里加。** 需要的话先看 Dock 自己怎么做。

## 布局文件

### 格式

例子：顶栏、三栏、状态栏。整个窗口就是 `center` 一棵树，没写 dock，也不写尺寸。

```json
{
  "version": 1,
  "center": {
    "panel_name": "StackPanel", "info": { "stack": { "sizes": [0, 0, 0], "axis": 1 } },
    "children": [
      { "panel_name": "TabPanel", "info": { "tabs": { "active_index": 0 } },
        "children": [{ "panel_name": "shell:demo/topbar", "children": [], "info": { "panel": null } }] },
      { "panel_name": "StackPanel", "info": { "stack": { "sizes": [0, 0, 0], "axis": 0 } },
        "children": [
          { "panel_name": "TabPanel", "info": { "tabs": { "active_index": 0 } },
            "children": [{ "panel_name": "smelt.sessions", "children": [], "info": { "panel": null } }] },
          { "panel_name": "TabPanel", "info": { "tabs": { "active_index": 0 } },
            "children": [{ "panel_name": "smelt.stage", "children": [], "info": { "panel": null } }] },
          { "panel_name": "TabPanel", "info": { "tabs": { "active_index": 0 } },
            "children": [
              { "panel_name": "smelt.files", "children": [], "info": { "panel": null } },
              { "panel_name": "smelt.git", "children": [], "info": { "panel": null } }
            ] }
        ] },
      { "panel_name": "TabPanel", "info": { "tabs": { "active_index": 0 } },
        "children": [{ "panel_name": "shell:demo/statusbar", "children": [], "info": { "panel": null } }] }
    ]
  }
}
```

| 写法 | 含义 |
|---|---|
| `StackPanel` | 拆分。`axis` 0 横排、1 竖排；`sizes` 要么全写 0（官方的「不定宽」，平分），要么写一整套像素（按比例分给窗口，存档和 `layout.copy` 导出的就是这样）。不能混着写。 |
| `TabPanel` | 一组面板，一次显示一个，`active_index` 是选中的。只放一个就是普通的一块。 |
| 其他 `panel_name` | 面板。`info.panel` 是它自己的数据，建的时候交给它，dump 时它原样写回。 |
| `version` | 我们定的号，从 1 开始（`layout_files::VERSION`）。面板的数据结构不兼容地变了才加。 |
| `left_dock` / `right_dock` / `bottom_dock` | 可选。官方的可收起区域。宿主不为它做额外的东西，写不写由布局作者定，开合用 `layout.toggle_dock`。 |

布局文件不写定宽。实测：手写「左 120、中间不定宽、右 200」，Dock 画出来右格是 120，存下来的又是另一套数；不写尺寸时，存下来的和画出来的完全一致（`main_tests/dock_layout_roundtrip_tests.rs`）。最小宽度只有官方的 `PANEL_MIN_SIZE`（100px），我们不另加。

### 面板名

| 面板名 | 是什么 |
|---|---|
| `smelt.sessions` | 会话列表 |
| `smelt.stage` | 舞台：一个会话或一个页面（智能体页、自动化、能力、远程桌面）。可以摆好几块，见「几块舞台」 |
| `smelt.files` | 文件：文件树和打开的文件 |
| `smelt.git` | 变更，标签上带未提交数的角标 |
| `smelt.history` | 历史会话 |
| `shell:<插件 id>/<面板>` | 插件面板。gpui-shell 注册时的名字（`gpui_shell::dock::register_panel`） |

- `StackPanel`、`TabPanel` 是官方保留名。
- 内置面板用 `gpui_base::dock::register_panel` 注册，`closable` 是 false，不让关；可以拖动重排（Dock 不锁），拖坏了用 `layout.reset` 回到文件。
- 内置面板（舞台除外）在一套布局里只能有一份：同一个实体画在两处，焦点、滚动、任务都会乱。写了两份的，第二份显示成「已经摆过一份」的占位，存档时原样写回。
- 没注册的名字（插件没装、删了、加载失败）显示成「插件未安装」的占位，存档时原样写回，插件装回来就好了。插件重新加载时，上一次声明过、这一次不再声明的面板名改注册成占位；从没声明过的旧名字留到重启（gpui-base 的注册表没有注销）。
- 插件声明了、布局里没写的面板，用命令面板的「添加面板」或 `layout.add` 加进来。

### 放哪

- 用户的 `~/.smelt/layouts/`，id 就是文件名去掉 `.json`。
- 已装插件目录下的 `layouts/`，id 是 `<插件 id>/<文件名>`。导入插件时它带了布局就直接换上，删插件时当前布局是它带的就回默认，见 `plugin-platform.md` 的「布局」。
- 程序里嵌一份默认布局（`layout_files::DEFAULT_LAYOUT`），同样是这个格式：左边会话列表，中间舞台，右边文件 / 变更 / 历史一组，宽度按 260 : 820 : 340 分。`default` 留给它，用户目录里同名的文件不加载。

布局的名字不在格式里：用户目录的取文件名，插件的在 `smelt.json` 里列（第 3 步）。

### 读取

按 `DockAreaState` 解析，再 `DockArea::load`。不加自己的校验：结构由 Dock 的 normalize 收拾，未注册的面板变占位。解析失败，或 `version` 不是当前的：这份不用，退回默认布局，原因发通知、写日志。

## 怎么画

- 一个 `DockArea` 就是整个工作台（`workspace_layout.rs`），宿主不再自己摆、自己拖。放大是 Dock 自己的（`set_zoomed_in` / `set_zoomed_out`），Esc 还原。
- 皮肤只有一套（`SmeltDockRenderer` / `SmeltTabGroup`），规则对所有布局一样：
  - 组里两个以上面板画一排标签；只有一个面板时，内置的工具面板（文件、变更、历史）画一条居中的标题带，会话列表、舞台和插件面板什么都不画，自己管顶上。
  - 标签条带放大按钮，空白处拖窗口。
  - 占位面板由皮肤的 `build_placeholder` 画一行「插件未安装」。
- 不做收起。布局里没有隐藏，宿主也不提供开合按钮和快捷键（「切换侧栏」「切换工具面板」删掉）。想腾地方就拖窄，或者放大一组。实测：面板改自己的可见性来隐藏一格，再显示时被缓存的 Dock 不重画，那一格回不来；就算回来，宽度也会变。真要能收起的区域，布局作者写官方的 dock。
- 窗口变窄时 Dock 按比例缩，宿主不再先收右栏再收左栏。

### 看得见

一块面板「看得见」：它是所在组选中的那个，没有被另一组的放大挡住，也不在收起的 dock 里。宿主据此：

- 给插件面板调 `set_active`（Dock 自己调）；
- 内置面板只在看得见时刷新（会话列表的动画、文件和变更的后台刷新）；
- 舞台看不见时，上面的会话不算被看着，未读照常记。

别处要「打开文件」「看变更」时，找到放着这个面板的组，切到它（`select_panel`），被放大挡住就先还原。布局里没放这个面板时，这个入口什么都不做。

### 窗口外框

红黄绿按钮的位置在建窗口时就定了（`workspace_frame`）。宿主按 Dock 的树算出贴着窗口左上角、右上角的那一组（放大时就是放大的那组；横排取第一个 / 最后一个子节点，竖排取第一个），告诉它：

- 皮肤画的标签条、标题带在左上角时开头让出红黄绿按钮的宽度（全屏、Windows 下是 0）。
- 舞台贴左上角时，它的标题胶囊让开同样的宽度（`Workspace::chrome_left_guard`）。会话列表顶上本来就空着一条。
- 插件面板读宿主接口的 `layout()`（`top_left` / `top_right` / `lights`），自己让；拖窗口自己调 `start_window_move()`。

### 不接 `ShellRoot`

gpui-kit 的 `ShellRoot` 是给整窗应用的根视图，smelt 的工作台不套它：

- 它常驻一个 50ms 的提示时钟，空闲时也周期唤醒，违反「等待一律事件驱动」。
- 它给整窗铺不透明背景，透明窗口和背景图就没了。
- 它在全局绑 `tab` / `shift-tab` / `cmd-c`，和终端、编辑器、我们自己的键位冲突。
- 它在背景上按下时把焦点清掉，点舞台空白处终端会丢焦点。

## 几块舞台

`smelt.stage` 可以摆好几块。每块在自己的 `info.panel` 里写 `accepts` 说它收哪几类，不写（`null`）是全收；不认识的名字忽略：

| 值 | 是什么 |
|---|---|
| `terminal` | 终端会话 |
| `conversation` | 对话会话 |
| `page` | 智能体页（连同里面的智能体对话）、自动化、能力库、远程桌面 |

```json
{ "panel_name": "smelt.stage", "children": [], "info": { "panel": { "accepts": ["terminal"] } } }
```

- **焦点。** 有一块舞台有焦点，它显示的就是「当前」：标题、文件 / 变更 / 历史跟着的项目、上一个 / 下一个会话、通知跳转都看它。在别的舞台里按下（或者插件 `run("layout.focus", { id })`），焦点换过去，它显示的变成当前。空着的舞台不抢焦点。
- **当前只记一份。** 有焦点的那块显示的就是工作台的当前会话 / 当前页，不另记。
- **打开一样东西时放哪块：** 它已经在某块舞台上，焦点换到那块；不然有焦点的舞台收它就放这儿；不然放进收它的、最近有过焦点的那块（都没有过就按布局先后），那块原来显示的让位。没有一块收它，就什么都不做。
- **一样东西只在一块上。** 会话、页面不会同时出现在两块舞台。
- **关掉一个会话：** 那块舞台换上它收的、最近看过、别处没显示的会话；没有就交出焦点、空着。
- **换布局、拖动重排、启动恢复完：** 按 Dock 现在的树重新理一遍：舞台没了的、放错了块的、重复的清掉，空着的舞台放进它收的、最近看过的会话。
- **只在内存里。** 焦点在哪块、别的几块显示着什么不落盘：舞台在 Dock 里没有稳定的名字，存档里只有面板名和 `info.panel`。重启后焦点在第一块，当前会话照常恢复，别的几块按上一条放。
- **插件看得到，不指定。** `layout()` 的 `stages` 列出几块舞台：id（运行时给的，重启会变）、`accepts`、有没有焦点、显示着哪个页面；`sessions()` 每个会话的 `stage` 是显示着它、看得见的那块。插件只能 `layout.focus` 换焦点，不能把东西塞进指定的舞台。

标题胶囊和它底下的拖窗口带是舞台自己的一部分，浮在舞台内容上沿，布局不能关。

## 存档

**按布局存，全局一份，不跟着会话走。** 外框属于窗口，不属于某段对话。

表不改（v9 已有）：

```sql
CREATE TABLE workspace_layout_state (
  layout_id TEXT PRIMARY KEY,
  -- 写这份存档时布局文件的内容哈希。文件改过就整份丢掉，以文件为准。
  file_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK (json_valid(state))
);
```

- `state` 是 `dump()` 的 JSON 原样，和布局文件同一个格式。用户拖过、切过的结果就是新的布局，不分「文件」和「改动」两层。
- Dock 发 `LayoutChanged` 时 dump，和上次写的比，一样就不写。拖分隔条只在松手时发一次。
- 放大不落盘（`load` 也会清掉放大）。
- 当前用哪套布局：`workspace_ui.layout_id`。
- 读到的存档解析不了、`version` 不对，当没有，用文件。

## 命令

命令的整体设计见 `plugin-platform.md` 的「命令」。面板都按面板名找；舞台有好几块，用 `layout.focus` 按舞台 id 找。

| 命令 | 参数 | 做什么 |
|---|---|---|
| `layout.select` | `panel` | 切到这个面板：选中它的标签，被放大挡住就先还原，在收起的 dock 里就打开 |
| `layout.zoom` | `panel` | 放大它所在的组；已经放大的是这组就还原 |
| `layout.add` | `panel`, `to`（可省） | 把布局里没有的面板加进来：给了 `to`（布局里的面板名）就进它那一组，不然在最右边另起一组。已经有了就是 `layout.select` |
| `layout.focus` | `id` | 焦点换到这块舞台（见「几块舞台」），露出来 |
| `layout.toggle_dock` | `dock`：`left` / `right` / `bottom` | 开合官方的 dock 区域。只对写了 dock 的布局有用 |
| `layout.switch` | `layout` | 换一套布局 |
| `layout.reset` | — | 丢掉这套布局的存档，重新读文件 |
| `layout.copy` | `layout`（可省，默认当前） | 把当前的 `dump()`（不是当前就用那套的文件）写到 `~/.smelt/layouts/<id>-copy.json`（已有就加序号），再切过去 |

不带参数的 `layout.reset`、`layout.copy` 也在命令面板和插件的 `commands()` 里；命令面板另有「切换布局：…」「恢复默认布局」「添加面板：…」。面板、布局不在了在工作台里提示。

## 启动和切换

1. `plugins::mount_all`（先于读存档）。
2. 读 `workspace_ui.layout_id` 指的布局，在启动事件里做，不在 render 里。找不到或不合法就用 `default`，发一条通知说原因；不改 `layout_id`，下次启动它好了就还用它。
3. 有指纹对得上的存档就 `load` 存档，不然 `load` 文件。
4. 切换：先解析新布局，失败就不动，弹提示；成功就 `load` 新的，写 `layout_id`。当前布局每次变（`LayoutChanged`）都已经存过，切换前不用再存。每次 `load` 时，新布局里的内置面板按面板名、按树的先后接着用装之前 Dock 里的实体：会话列表的滚动位置、舞台里的终端不会因为切布局而重建。实体只活在 Dock 里，不另建缓存池。
5. 插件重新加载：先 `dump`，再 `load` 回去，插件面板换成新脚本建的。

## 给智能体的配套

- **格式说明**：smelt 技能里加一节布局，指向官方的 `DockAreaState`。
- **截图命令**：见 `plugin-platform.md` 的「给智能体」。
- `smelt layout check <文件>`、JSON Schema、设置里的布局页、布局目录的文件监听：留到后面一步。

## 分享（留给 #259）

- 一套布局用到哪些插件，从文件里的 `shell:<id>/<面板>` 直接读出来，不另写依赖清单。
- 分享包是一个目录：`layouts/<id>.json` 加 `plugins/<id>/`，外加说明和预览图。导入时 id 撞了就改名，不覆盖。
- 安装前宿主列出要装的插件和它们声明的网络主机，并写明「插件可读写文件、执行命令」。

## 测试

- 布局文件（`layout_files`）：默认布局能解析、能 `load`；坏 JSON、`version` 不对退回默认；文件指纹变了存档被丢掉；存档没变不写库；未注册的面板 `load` → `dump` 后名字和 `info.panel` 原样还在。
- 存档往返：拖过分隔条以后 `dump`、新窗口 `load`，画出来一样（已有）。
- 搭建：切换布局时内置面板是同一个实体；内置面板写了两份时第二份是占位；`layout.add` 已有的面板只是切过去。
- 看得见：切走标签、别的组放大、dock 收起，都报看不见；露出来报看得见。
- 舞台：几块舞台按 `accepts` 分；拖动重排以后重新理一遍。
- 存储：`layout_id` 指向不存在的布局时回到默认，不改写存档。
