---
name: Smelt Mobile
description: 多台运行时的指挥台：钢是背景，纸只给需要你动手的事。
colors:
  ground: "#ECEEF0"
  shelf: "#FFFFFF"
  shelfPressed: "#F6F7F8"
  raised: "#FFFFFF"
  hairline: "#E3E6E9"
  rail: "#CCD1D5"
  paper: "#FFFDF6"
  paperEdge: "#D6D2C6"
  onPaper: "#1E1E1E"
  onPaperMuted: "#5A5650"
  paperControl: "#E6E1D3"
  ink: "#1E1E1E"
  inkMuted: "#596067"
  inkFaint: "#61686F"
  heat: "#FF9F1C"
  onHeat: "#1E1E1E"
  heatInk: "#8F5300"
  danger: "#C23B3B"
  dangerWash: "#FBE7E7"
  ok: "#1F7A4E"
  okWash: "#E3F3EA"
  link: "#2C67A8"
  control: "#E3E6E9"
  onControl: "#1E1E1E"
  controlSelected: "#FFFFFF"
  solid: "#1E1E1E"
  onSolid: "#FFFFFF"
  shadow: "#0000001F"
  scrim: "#00000066"
  ground-dark: "#121417"
  shelf-dark: "#1B1E22"
  shelfPressed-dark: "#22262B"
  raised-dark: "#22262B"
  hairline-dark: "#2A2E33"
  rail-dark: "#363B41"
  paper-dark: "#ECE7DA"
  paperEdge-dark: "#B5AF9F"
  paperControl-dark: "#DCD6C6"
  ink-dark: "#ECEEF0"
  inkMuted-dark: "#A2A9B0"
  inkFaint-dark: "#8C939A"
  heat-dark: "#FFA933"
  heatInk-dark: "#FFB547"
  danger-dark: "#F07070"
  dangerWash-dark: "#3A1B1D"
  ok-dark: "#4CC48F"
  okWash-dark: "#16291F"
  link-dark: "#86B8F0"
  control-dark: "#2A2E33"
  onControl-dark: "#ECEEF0"
  controlSelected-dark: "#454B52"
  solid-dark: "#ECEEF0"
  onSolid-dark: "#1E1E1E"
  shadow-dark: "#00000052"
  station-0: "#3B7DD8"
  station-1: "#12857F"
  station-2: "#7C5CD6"
  station-3: "#C2468A"
  station-4: "#6B7A1C"
  station-5: "#4652C8"
  station-6: "#1B7FB0"
  station-7: "#93644A"
  station-0-dark: "#5B95E6"
  station-1-dark: "#2EB8B3"
  station-2-dark: "#9A80EC"
  station-3-dark: "#E06AAA"
  station-4-dark: "#A4B83D"
  station-5-dark: "#7480EE"
  station-6-dark: "#45B6EA"
  station-7-dark: "#C08D6E"
  termBg: "#101214"
  termFg: "#E6E8EA"
typography:
  largeTitle:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "32px"
    fontWeight: 700
    lineHeight: 1.15
    letterSpacing: "-0.4px"
  title:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "20px"
    fontWeight: 600
    lineHeight: 1.25
  navTitle:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "17px"
    fontWeight: 600
    lineHeight: 1.25
  headline:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "16px"
    fontWeight: 600
    lineHeight: 1.3
  body:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "16px"
    fontWeight: 400
    lineHeight: 1.45
  bodyStrong:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "16px"
    fontWeight: 600
    lineHeight: 1.45
  callout:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: 1.4
  button:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "15px"
    fontWeight: 600
    lineHeight: 1.2
  note:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.4
  meta:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.3
    fontFeature: "\"tnum\""
  label:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "13px"
    fontWeight: 600
    lineHeight: 1.3
  caption:
    fontFamily: "system-ui, -apple-system, Roboto, sans-serif"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.3
  monoHead:
    fontFamily: "JetBrainsMonoNL, ui-monospace, monospace"
    fontSize: "12px"
    fontWeight: 500
    lineHeight: 1.3
    fontFeature: "\"tnum\""
  monoTime:
    fontFamily: "JetBrainsMonoNL, ui-monospace, monospace"
    fontSize: "13px"
    fontWeight: 500
    lineHeight: 1.3
    fontFeature: "\"tnum\""
  code:
    fontFamily: "JetBrainsMonoNL, ui-monospace, monospace"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.45
  codeRow:
    fontFamily: "JetBrainsMonoNL, ui-monospace, monospace"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: 1.35
rounded:
  tag: "2px"
  ticket: "4px"
  chip: "8px"
  control: "10px"
  shelf: "12px"
  sheet: "20px"
  pill: "24px"
spacing:
  xxs: "2px"
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "20px"
  xxl: "24px"
  section: "28px"
  page: "16px"
  touch: "48px"
components:
  button-heat:
    backgroundColor: "{colors.heat}"
    textColor: "{colors.onHeat}"
    typography: "{typography.button}"
    rounded: "{rounded.control}"
    padding: "0 16px"
    height: "48px"
  button-paper:
    backgroundColor: "{colors.paperControl}"
    textColor: "{colors.onPaper}"
    typography: "{typography.button}"
    rounded: "{rounded.control}"
    padding: "0 16px"
    height: "48px"
  button-solid:
    backgroundColor: "{colors.solid}"
    textColor: "{colors.onSolid}"
    typography: "{typography.button}"
    rounded: "{rounded.control}"
    padding: "0 16px"
    height: "48px"
  button-quiet:
    backgroundColor: "{colors.control}"
    textColor: "{colors.onControl}"
    typography: "{typography.button}"
    rounded: "{rounded.control}"
    padding: "0 16px"
    height: "48px"
  button-danger:
    backgroundColor: "{colors.dangerWash}"
    textColor: "{colors.danger}"
    typography: "{typography.button}"
    rounded: "{rounded.control}"
    padding: "0 16px"
    height: "48px"
  button-disabled:
    backgroundColor: "{colors.control}"
    textColor: "{colors.inkFaint}"
    rounded: "{rounded.control}"
    height: "48px"
  ticket-paper:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.onPaper}"
    typography: "{typography.body}"
    rounded: "{rounded.ticket}"
    padding: "14px 12px 12px"
  ticket-head:
    textColor: "{colors.onPaperMuted}"
    typography: "{typography.monoHead}"
  ticket-stamp:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ok}"
    typography: "{typography.button}"
    rounded: "{rounded.ticket}"
    padding: "4px 12px"
  ticket-rail:
    backgroundColor: "{colors.rail}"
    height: "4px"
  shelf:
    backgroundColor: "{colors.shelf}"
    rounded: "{rounded.shelf}"
  shelf-header:
    textColor: "{colors.inkMuted}"
    typography: "{typography.label}"
    padding: "28px 20px 8px"
  shelf-row:
    backgroundColor: "{colors.shelf}"
    textColor: "{colors.ink}"
    typography: "{typography.headline}"
    padding: "10px 16px"
    height: "56px"
  shelf-row-pressed:
    backgroundColor: "{colors.shelfPressed}"
  shelf-row-selected:
    backgroundColor: "{colors.control}"
  station-tape:
    backgroundColor: "{colors.station-0}"
    rounded: "{rounded.tag}"
    width: "4px"
    height: "14px"
  elapsed-running:
    textColor: "{colors.ok}"
    typography: "{typography.monoTime}"
  input:
    backgroundColor: "{colors.shelf}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "12px"
  chip:
    backgroundColor: "{colors.shelf}"
    textColor: "{colors.ink}"
    typography: "{typography.label}"
    rounded: "{rounded.chip}"
  chip-selected:
    backgroundColor: "{colors.solid}"
    textColor: "{colors.onSolid}"
  segmented:
    backgroundColor: "{colors.control}"
    textColor: "{colors.inkMuted}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
  segmented-selected:
    backgroundColor: "{colors.controlSelected}"
    textColor: "{colors.ink}"
  nav-bar:
    backgroundColor: "{colors.shelf}"
    textColor: "{colors.inkMuted}"
    typography: "{typography.caption}"
    height: "64px"
  nav-bar-selected:
    backgroundColor: "{colors.control}"
    textColor: "{colors.ink}"
  nav-badge:
    backgroundColor: "{colors.heat}"
    textColor: "{colors.onHeat}"
  composer-pill:
    backgroundColor: "{colors.shelf}"
    textColor: "{colors.ink}"
    rounded: "{rounded.pill}"
    padding: "0 4px"
  sheet:
    backgroundColor: "{colors.raised}"
    rounded: "{rounded.sheet}"
  snackbar:
    backgroundColor: "{colors.solid}"
    textColor: "{colors.onSolid}"
    rounded: "{rounded.control}"
  terminal:
    backgroundColor: "{colors.termBg}"
    textColor: "{colors.termFg}"
    typography: "{typography.code}"
    padding: "4px 6px"
---

# Design System: Smelt Mobile

## Overview

**Creative North Star: "出菜口（The Pass）"**

手机是出菜口，用户是传菜长。每台机器是一个灶台，每个会话是一张单子。整个界面只做一件材质上的区分：**需要你动手的事是纸，其余都是钢**。钢灰底、白色搁板、墨黑字构成安静的工作背景；热敏单据纸带着锯齿顶边挂在一道平涂的钢轨上，夜里也仍是纸，是屏幕上唯一亮着的东西。保温灯琥珀只照在「需要你」上。

密度是 iOS 分组列表的密度：页边 16、行高至少 56、组间 28，正文 16 号系统字。两端用同一套 Smelt 视觉语言，iOS 与 Android 长得一样；只有返回手势、安全区、系统字号、触感这类平台**行为**各随系统。按下不出水波纹，只换底色。

这套系统明确拒绝两种结果：通用 SaaS 后台（卡片墙、仪表盘大数字），和黑客终端 cosplay（满屏等宽字、荧光绿）。等宽字只出现在单据头、实时耗时、命令、代码、diff 和终端里；终端是唯一常驻深色的区域。

**Key Characteristics:**
- 两种材质：钢（`ground` / `shelf`）承载一切常态信息，纸（`paper`）只给待你处理的单据。
- 一种热色：琥珀 `heat` 只给「需要你」，屏上琥珀的数量等于等你的事。
- 每台机器一条灶台色带（`stations`），跟着这台机器的每一条内容走。
- 数字一律表格数字；正在走的耗时与单据头是等宽字。
- 一个讲究的动作：单据挂上轨、盖章、上滑离轨；其余都是系统默认转场，减少动态效果时一律归零。
- 深浅两套都是一等公民，所有文字对比度在 `test/theme/contrast_test.dart` 里按 WCAG AA 验。

## Colors

冷灰钢配暖白纸：一套几乎无彩的中性底，一种保温灯琥珀，一组功能性状态色，外加八种灶台标签色。颜色分三层：`Primitives`（唯一允许写颜色字面量的地方，按测得的明度命名）→ `GalleyColors` 语义槽（界面只读这一层，`GalleyColors.of(context)` 缺失即 assert，不回退）→ `buildGalleyTheme` 把语义槽手写进 Material `ColorScheme`（不用 `fromSeed`）。frontmatter 里不带后缀的是浅色值，`-dark` 是深色值。方向契约里写的夜间钢底 #15171A 与失败红 #D64545 在构建中落成了 `ground-dark` 与 `danger`，以构建值为准。

### Primary
- **保温灯琥珀**（`heat` / `heat-dark`）：单据上你要按的那一颗按钮（允许、允许一次、回答、提交）、底部导航「指挥台」上的待办计数徽标。上面的字一律是墨黑 `onHeat`，琥珀本身从不做文字。
- **深琥珀字**（`heatInk` / `heatInk-dark`）：「需要你」作文字或图标时用：会话行的举手图标、平板列表顶「3 件事等你」、对话状态行的「等你批准 / 等你回答」。

### Secondary
- **单据纸**（`paper` / `paper-dark`）：只给需要你动手的单据（轨上单据、逐张处理页、对话页输入框上方的停靠单据）。深色模式下仍是纸色（略压暗），不翻成深底。
- **纸边**（`paperEdge`）：单据描边（0.6）、单据内虚线分隔、命令块左侧 1px 细线、选择题未选中描边、骨架条。
- **纸上的字**（`onPaper` / `onPaperMuted`）：纸上的正文和次要字，两种模式都是深墨，因为纸本身是浅的。
- **纸上的次要按钮底**（`paperControl`）：拒绝、跳过、总是允许等纸上非琥珀动作。

### Tertiary
- **在做绿**（`ok` / `ok-dark`）：正在跑的会话的呼吸点和走动的耗时、「已允许」章、diff 新增计数。`okWash` 是 diff 新增行浅底。
- **失败红**（`danger` / `danger-dark`）：失败图标、失败单据的等待计时、机器需要重新配对时的链路状态、「已拒绝」章。`dangerWash` 是「总是拒绝」等危险按钮底。
- **链接蓝**（`link`）：正文链接。
- **灶台标签色**（`station-0` … `station-7`，各有 `-dark`）：蓝、青、紫、粉、橄榄、靛、天蓝、棕。序号即机器登记里的 `accent`，由 `GalleyColors.station(accent)` 取（越界取模）。只作非文字图形（色带、色块），对各自底色 ≥ 3:1；读屏念色名。

### Neutral
- **钢底**（`ground` / `ground-dark`）：所有页面背景、导航栏背景、详情栏背景。
- **白色搁板**（`shelf` / `shelf-dark`）：分组列表、输入框底、底部导航与侧边导航栏底、输入胶囊。按下换成 `shelfPressed`。
- **浮层**（`raised`）：底部弹层、对话框、菜单、终端选区工具条。
- **发丝线**（`hairline`）：行间分隔、输入框描边、详情栏左边线、导航栏下滚动时出现的那道线。
- **钢轨**（`rail`）：单据轨，一道 4px 平涂结构线。
- **墨**（`ink` / `inkMuted` / `inkFaint`）：主字、副标题与分组标题、最弱的提示与 chevron。`inkFaint` 在钢底上也过 4.5:1。
- **中性控件**（`control` / `onControl` / `controlSelected`）：次要按钮底、分段控件轨道与选中块、导航选中指示、平板列表选中行底。深色模式选中块比轨道亮一档。
- **实心墨**（`solid` / `onSolid`）：非「需要你」的主要动作（扫码配对、发送、完成）、选中的筛选芯片、开关轨道、Snackbar 与 Tooltip 底。
- **终端底 / 终端字**（`termBg` / `termFg`）：终端固定深色，不跟 App 明暗走；ANSI 16 色沿用桌面 Tokyo Night 一套（见 sidecar）。

### Named Rules
**The Heat Lamp Rule（保温灯规则）。** 琥珀只给「需要你」：单据上你要按的那一颗、待办计数、等你处理的标记。普通主动作用 `solid`，次要用 `control`，终端里锁存的 Ctrl 键也反白成实心墨而不是琥珀。唯一的非「需要你」用法是文字选区：主题选区为 `heat` 35% 透明、终端选区 32% 透明，从不实心。

**The Only Paper Rule（纸只给要你动手的事）。** `paper` 与 `TicketPaper` 只出现在待处理单据上。已完成、在跑、空闲的一切都在钢和搁板上；其它任何地方不用纸色、不画单据。

**The Station Follows Rule（色带跟着机器走）。** 每条跨机器内容（单据头、分组标题、筛选芯片、对话标题下）都带这台机器的灶台色，形状是 4px 竖色带或 8px 色块。色带是独立记号，不是卡片的彩色侧边框。

**The Honest State Rule（诚实的状态）。** 状态有三条通路：图标形状、文字、颜色，颜色排最后。离线机器的缓存行画空心灰点、不呼吸、不走表，整个搁板压到 72% 不透明，分组标题写「重连中… · 显示的是 21:07 的数据」。

## Typography

**Body Font:** 系统字（iOS SF Pro、Android Roboto，中文落到系统 CJK 字体），代码里不指定 family。
**Label/Mono Font:** JetBrains Mono NL（无连字版，随 App 打包，Regular / Medium 500 / Bold 700）。

**Character:** 系统字负责一切可读的话，安静、跟随平台字号；等宽字只出现在需要逐位对齐或逐字照抄的地方，像热敏打印机打在单据头上的那一行。

### Hierarchy
- **largeTitle**（700，32/1.15，-0.4 字距）：顶层页大标题（指挥台、会话、智能体、设置），向上滚动收成 17 号 `navTitle`。
- **title**（600，20/1.25）：弹层、对话框、详情页标题。
- **navTitle**（600，17/1.25）：内联导航栏标题；收起后下方可带一行 `caption` 导词（当前滚到的机器名）。
- **headline**（600，16/1.3）：搁板行标题、单据标题、空状态标题。
- **body / bodyStrong**（400 / 600，16/1.45）：正文、审批问题、输入框文字。对话阅读栏最宽 720。
- **callout**（400，15/1.4）：搁板里次要的行（「全部会话」、近期的安静行）、选择题选项。
- **button**（600，15/1.2）：所有按钮字、章上的字（章用 800 加表格数字）。
- **note**（400，14/1.4）：空状态正文、状态条、提示。
- **meta**（400，13/1.3，表格数字）：行副标题（agent · 项目）、相对时间「2 分钟前」、链路延迟「12 ms」。
- **label**（600，13/1.3）：搁板分组标题、芯片、分段控件、表单字段名。
- **caption**（400，12/1.3）：导航标签、导词、Tooltip。
- **monoHead**（500，12/1.3，等宽表格数字）：单据头「#1 ▮ MacBook Pro ······ 00:42」、diff 文件路径与增删计数。单据头右侧的等待计时加粗到 700。
- **monoTime**（500，13/1.3，等宽表格数字）：正在走的耗时（会话行、对话时间线，绿色）、回合耗时、运行时长、延迟等测量值。
- **code**（400，13/1.45）：命令、代码块、行内代码、diff、设备 ID 与 endpoint、配对链接输入。
- **codeRow**（400，15/1.35）：可点的等宽行（斜杠命令列表）。

样式本身不带颜色：同一字号在纸上取 `onPaper`，在钢上取 `ink`。

### Named Rules
**The Measurement Mono Rule（等宽只给测量与代码）。** 等宽字只用于单据头、正在走的耗时与其它测量值、命令、代码、diff、终端。工具名、相对时间、章上的中文都用系统字；相对时间与延迟用系统字的表格数字。

**The Tabular Rule（数字对齐）。** 所有会变的数字都开 `tabularFigures`，计时走动时不跳宽。

## Layout

**分组列表模型。** 页面是钢底上一列白色搁板：页边 `page` 16，搁板内行左右 16、上下 10，行最小高 56（`touch` 48 + 8），行间发丝线从内容起点（左缩 16）开始。分组标题在搁板上方，上留 28（`section`，第一组只留 8）、下留 8，左右比页边多 4。组内紧、组间松、标题上方比下方大。间距一律是 4 的倍数（`xxs` 2 仅用于标题与副标题之间）。

**指挥台第一屏（手机）。** 大标题「指挥台」+ 右上「+」；标题下一道钢轨横贯全宽，单据横排挂在轨下，单据宽 = 栏宽 − 2×16 − 28 的露边，夹在 240–340 之间，所以下一张总露出一截；单据间距 12。轨下按机器分组的搁板。没有单据时轨下只有一行「没有等你处理的事」。

**逐张处理。** 顶上横一道和指挥台同样的平涂轨，单据的锯齿顶边塞在轨下。一屏一张单据（`PageView`，`viewportFraction` 0.9，下一张从右边露出），自然高度，不硬撑满屏（撑出来的只会是一段空纸）：单据头 → 标题 → 印着会话最近几条（你说了什么、它回了什么、刚跑了哪些工具，取自内核随单据带回的快照末尾 6 条，`TicketContext`）→ 虚线 → 请求和按钮；第几张写在导航栏标题里。手机上全屏弹出；平板上嵌在右侧详情栏，所以平板上的轨在详情栏顶上。

**平板分栏。** 按窗口宽度判断而不是设备型号：宽度 ≥ 700 走双栏（左侧 `NavigationRail` + 列表栏 + 详情栏），宽度 ≥ 1000 列表栏 380，否则 320。详情栏有自己的导航栈，左边一道发丝线。平板上指挥台列表栏**不挂轨**，只放一行摘要（「3 件事等你」，`heatInk`，点了把右栏切到第一张）；单据挂在详情栏顶上的轨下（逐张处理），同一张单据不在一屏上出现两次。详情栏从不空白占位。

**系统适配。** 触控区统一 48；跟随系统字号，不写死行高截断；安全区内底部再留 24。

## Elevation & Depth

整体是平的，靠色阶分层：钢底 → 白搁板 → 浮层（深色模式下 `ground` 08 → `shelf` 11 → `raised` 14 依次提亮）。搁板没有投影。阴影只给两类东西：**纸**（单据离开了钢，有一点真实的投影）和**真浮层**（漂在内容上方、会随内容滚动的控件）。导航栏平时无阴影，内容滚到下面时出一道发丝线。

### Shadow Vocabulary
- **单据投影**（`canvas.drawShadow(outline, shadow, 3)`，浅色 `#0000001F`、深色 `#00000052`）：沿锯齿轮廓投下，只给 `TicketPaper`。
- **浮动胶囊**（`0 2px 12px shadow`）：对话页输入胶囊；「回到最新」胶囊用 `0 2px 10px`。
- **浮动工具条**（`0 0 16px shadow`）：终端选区操作条。
- **拖动中的行**（Material elevation 8）：设置里长按拖动排序的机器行。
- **滚动分隔**（`scrolledUnderElevation: 1`，阴影色取 `hairline`）：导航栏下有内容时的那道线。

### Named Rules
**The Flat Steel Rule（钢是平的）。** 钢只在配色里，不在质感里：轨是平涂的 4px `rail` 线，没有渐变斜面、没有拉丝纹理、没有灯光晕染。

**The Paper Lifts Rule（纸才会浮起）。** 搁板、行、芯片不加阴影。需要层次时先换色阶，浮层才加阴影。

## Shapes

形状语言是「方纸、圆钢」：纸几乎是方的，钢做的控件有柔和圆角。

- **单据**：锯齿顶边（齿高 6，齿宽约 9，等宽小三角）像从热敏打印机上撕下；下面两角 4 圆角（`ticket`），0.6 纸边描边。单据内分隔是 3 实 3 空的虚线，像打出来的。
- **章**：2px 描边、4 圆角、85% 不透明墨，逆时针歪约 7°（轨上与逐张处理页）；留在对话时间线里的小章只歪约 2°、不落下。
- **控件**：按钮、输入框、行内选择、Snackbar 都是 10（`control`）；芯片与 Tooltip 8（`chip`）；搁板与菜单 12（`shelf`）；底部弹层顶角与对话框 20（`sheet`）；输入胶囊 24（`pill`）。
- **小记号**：灶台色带与色块、骨架条、滚动条 2（`tag`）；状态点是 8 的圆。
- **图标**：只用 Lucide 线性图标，常规 20、行内小号 16，agent 身份用 LobeHub 图标（`AgentGlyph`）。

## Components

### Buttons
干脆、实在，按下只换底色，不出水波纹。
- **Shape:** 柔和圆角（10px），高 48，左右内边距 16，`button` 字（600 / 15），可带 16 号图标。
- **Heat（`GalleyButton.heat`）：** 琥珀底墨字。只用在单据上你要按的那颗：允许 / 允许一次 / 回答 / 提交。
- **Paper（`GalleyButton.paper`）：** `paperControl` 底墨字，纸上的次要动作：拒绝、总是允许、跳过、打开链接。
- **Solid（`GalleyButton.solid`）：** 实心墨，钢上的主要动作：扫码配对、完成、复制。
- **Quiet（`GalleyButton.quiet`）：** `control` 中性底，钢上的次要动作：重新配对。
- **Danger（`GalleyButton.danger`）：** `dangerWash` 底红字：总是拒绝。
- **Disabled / Busy：** `control` 底 `inkFaint` 字。提交审批期间整组按钮置灰，快速连点不会把同一个回答发两次。
- **排列：** 轨上紧凑版是「允许 | 拒绝」两颗等宽并排（间距 8）；完整版按「允许一次 → 总是允许 → 拒绝 → 总是拒绝」流式换行。

### Chips
- **Style:** 筛选芯片 `shelf` 底、`hairline` 描边、8 圆角、`label` 字，可带灶台色块；无勾选图标。
- **State:** 选中翻成实心墨底、`onSolid` 字。单据内的选择题选项是纸上的变体：未选中纸底加纸边，选中反白成 `onPaper` 墨底纸字，单选 / 多选用圆 / 方勾选图标区分，最小高 44。

### Cards / Containers
系统里没有卡片，只有搁板和纸。
- **Shelf：** 12 圆角白色搁板，裁切内容，无阴影、无描边；行间发丝线；搁板里不再套卡片。
- **ShelfHeader：** `label` 字 `inkMuted` 色，左侧可放 14 高的灶台色带，右侧 `meta` 写链路状态（需要处理时为 `danger`）。读屏标为标题。
- **ShelfRow：** 前导（状态记号）、`headline` 标题 + `meta` 副标题（agent 图标 + 「agent · 项目」）、尾部（相对时间或绿色等宽耗时）。按下 `shelfPressed`，平板上选中行 `control` 底。
- **EmptyNote：** 居中一个 28 号淡图标、`headline` 标题、`note` 正文、可选一颗 solid 按钮。不画插图卡片。

### Inputs / Fields
- **Style:** `shelf` 底、`hairline` 1px 描边、10 圆角、内边距 12，提示字 `inkFaint`。在纸上时底色换成 `paper`。
- **Focus:** 描边换成 `ink`、1.5px。光标 `ink`，选区琥珀 35%。
- **Composer：** 对话页底部是 24 圆角的白色胶囊，`hairline` 描边加浮动胶囊阴影，左侧附图 / 命令图标按钮，下方一行用量小字。

### Navigation
- **手机：** 底部标准导航栏，`shelf` 底，高 64，四项「指挥台 / 会话 / 智能体 / 设置」，标签常显；选中项 `ink` 字 600 + `control` 指示块，未选中 `inkMuted`。指挥台图标上的待办计数是琥珀徽标。
- **平板：** 同样四项改为左侧 `NavigationRail`，配色一致。
- **顶层页：** `GalleyPage` 大标题栏（展开 116），收起为居中 `navTitle`，下方导词写当前滚到的机器；支持下拉刷新（重验连接）。
- **详情：** 页面一律经 `DetailHost.open` 打开，手机压栈、平板替换右栏。返回手势跟随系统。

### Status Mark
记号先靠形状说话：在做是绿色实心点外加一圈 1.4s 慢慢扩散的光环（减少动态效果时静止）；需要你是琥珀深色举手；完成是 `inkMuted` 对勾；失败是红色叉圈；结束是 `inkFaint` 斜杠圈；空闲与缓存里的「在做」是 `inkFaint` 空心点。每个记号都有读屏标签。

### Ticket Rail（招牌组件）
钢轨上挂着的热敏单据，是这套系统唯一的讲究动作。
- **轨：** 4px `rail` 平涂横线，画在单据上面，单据顶边塞在轨下；单据区可横向滚动，下一张露边。
- **单据（`TicketPaper` + `PassTicketCard`）：** 单据头（序号 · 灶台色块 · 机器名 ······ 等待计时，等宽）→ `headline` 标题 → 「agent 想运行 · 项目」→ 虚线 → 正文（审批问题 + 命令块 / 选择题 / 失败原因）→ 动作按钮。命令块是左侧 1px 纸边线加等宽字：轨上紧凑版只放一行、末尾省略号（不在连字符处折行），完整版照原样折行。详情未取回时是两条纸边色骨架条。
- **动作：** 新单据从右侧 40 外挂上来，绕顶边中点晃一下（幅度衰减，420ms，`easeOut`）；处理后先盖章（520ms，章由 1.4 倍缩落、淡入），再上滑 28 离轨并收起宽度（420ms，`easeInOut`），旁边的单据补位。允许时中等触感，拒绝时轻触感。
- **留痕：** 章不随单据消失：逐张处理页往回滑仍看到盖了章的单据；对话时间线里那条工具调用下留一枚静止小章。
- **减少动态效果：** 挂上与离开都直接出现 / 消失，章直接显示，呼吸点静止。

### Terminal
固定深色（`termBg` / `termFg`），ANSI 16 色与桌面一致；等宽字号只取设置里的档位，行高 1.15，四周留白 6 / 4。光标是前景色实心块，选区琥珀 32%。快捷键栏的键帽用 `shelf` 底，锁存的 Ctrl 反白成实心墨。

## Do's and Don'ts

### Do:
- **Do** 只从 `GalleyColors.of(context)` / `GalleyText.of(context)` / `Space` / `Radii` / `Strokes` / `Motion` 取值；颜色字面量只写在 `lib/theme/primitives.dart`。
- **Do** 把琥珀留给「需要你」：单据上要按的那一颗用 `GalleyButton.heat`，其它主要动作用 `GalleyButton.solid`。
- **Do** 让每条跨机器内容带上灶台色（`StationTape` / `StationLabel` / `GalleyColors.station(accent)`）。
- **Do** 状态同时用图标、文字和颜色表达（`StatusMark` 带读屏标签）；缓存数据画成静止灰色并写明是几点的数据。
- **Do** 所有动画时长经 `Motion.resolve(context, …)`，减少动态效果时归零，而不是「轻一点」。
- **Do** 会变的数字开表格数字；正在走的耗时用 `ElapsedLabel`（等宽、绿色），相对时间用 `formatAgo`（系统字）。
- **Do** 触控区至少 48，跟随系统字号，深浅两套都验 WCAG AA。
- **Do** 平板上用 `DetailHost.open` 进详情栏，详情栏总有内容；同一张单据一屏只出现一次。
- **Do** 界面文字全部走本地化，中英都写。返回手势、底部弹层、对话框、开关、分段控件用 Flutter 自带的，不另做一套。

### Don't:
- **Don't** 在待处理单据之外用纸色或画单据；已完成、在跑、空闲的东西都留在钢和搁板上。
- **Don't** 给钢轨或钢底加渐变、斜面、拉丝纹理或琥珀灯光晕染；轨是一道平涂的 `rail` 线。
- **Don't** 把等宽字当「技术感」装饰：工具名、相对时间、章上的中文、普通标签都用系统字。
- **Don't** 做卡片墙、仪表盘大数字，或在搁板里再套卡片。
- **Don't** 做满屏等宽、荧光绿的终端风界面；深色只属于终端。
- **Don't** 给卡片或单据加 1px 以上的彩色侧边框（包括失败单据的琥珀 / 红色侧条）；失败用文字、计时颜色或章来区分。
- **Don't** 加 eyebrow 小标签、渐变字或装饰性毛玻璃。
- **Don't** 用 Material `Icons.*`、emoji 或 Unicode 字符当图标；只用 Lucide 与 `AgentGlyph`。
- **Don't** 用水波纹或扩散反馈；按下只换底色。
- **Don't** 为两个平台做两套控件；只有平台行为各随系统。
- **Don't** 把这份说明里的厨房比喻写进界面。「出菜口」「灶台」是内部说法，界面上说机器、会话、指挥台。
