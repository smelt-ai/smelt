# 移动端交互重构 · 把 IA 拉回「指挥台」

> 当时的施工手记。文中「五态」色是那一轮的取色口径；`AgentStatus` 后来收成
> 要你 / 运行中 / 空闲三态，以 `crates/smelt-core/src/agent_status.rs` 为准。

配套：[`product-roadmap.md §6`](product-roadmap.md)（目标体验）·
[`remote-ops-roadmap.md`](remote-ops-roadmap.md)（A/B/C 三种客户端形态）·
[`ui-design-language.md`](ui-design-language.md)（桌面视觉语言，**未覆盖移动端**）

**设计稿：[`assets/mobile-ux-redesign-v1.html`](assets/mobile-ux-redesign-v1.html)**
——浏览器直接打开，自包含无外部依赖。6 屏：指挥台 / 审批详情 / 项目 / 会话详情 /
终端 / 设置，外加 IA 现状与目标的对比。配色直接取自
`crates/smelt-ui/src/ui_theme.rs` 的 `DARK` 色板，表面层级沿用桌面同一条规则
（主内容区最亮，越往边缘越暗），不在手机上另起一套视觉。

---

## 结论先说

**不需要推倒重来，需要重做信息架构。**

组件层（工具调用卡片、diff 渲染、终端快捷键栏、图片查看器）质量是够的，
留着。问题出在**上面一层**：当前 `mobile/lib/main.dart` 的 IA 是从桌面平移过来的
——首屏是"项目树 → 展开 → 会话"，这是坐在 Mac 前浏览工作区的组织方式。

而 `product-roadmap.md §6` 早就写死了移动端定位：

> **手机是指挥台，不是第二块小终端。**
> 人不在电脑前，也能知道 agent 卡在哪、一点允许/拒绝、必要时短回复。

`remote-ops-roadmap.md` 进一步把客户端形态拆成 A/B/C 三种，并明确
「手机**默认 B 操作台**，终端模式作二级入口」。

**当前实现漂移到了 A（终端/聊天流优先），操作台这个形态根本没被建出来。**
所以这不是"要不要重新设计"的问题——目标已经定了，是当前实现没落到目标上。

---

## 漂移对照

| roadmap §6 要求 | 当前实现 | 位置 |
|---|---|---|
| 会话列表：相位 · 角标 · 一键进详情 | 首屏默认是**项目 ExpansionTile 树**，会话在二级；相位是一个灰底 Chip | `_buildProjectSessionList` |
| **操作台为默认形态**（大状态 + 问题文案 + 主按钮） | 没有这个页面。审批是聊天页顶部一条 orange banner，必须先进对的那个会话才看得到 | `_buildPermissionBanner` |
| PTY 会话：**操作台为主**，完整终端二级 | 点终端会话直接进全屏 xterm，没有中间层 | `_openSession` |
| 推送通知（等批准 / 等输入 / 失败） | 未做，`pubspec.yaml` 无任何通知依赖；只有前台 SnackBar，且被 `clearSnackBars()` 互相顶掉 | `_attentionSubscription` |
| 深链：通知一点进对应会话 | 未做 | — |
| 弱网：断线提示 · 自动重连 · 失败可重试 | 已做（`CachedConnectionBar` + 重试） | ✅ |

还有一条不在 roadmap 里、但属于"没有设计系统"的症状：
移动端从未被纳入 `ui-design-language.md`。主题写死 `Brightness.dark`，
状态色是散落各处的 `Colors.green.shade700` / `Colors.orange` / `Colors.red.shade300`，
没有 token、没有浅色、没有 i18n、没有 `Semantics`。桌面那套三级表面 + 语义状态色
的思路一条也没过来。

---

## 目标 IA

```
底部导航（取代现在的 SegmentedButton 筛选器）
├── 指挥台 Console（默认首屏）
│   ├── 等我处理    ← 大卡片，问题文案 + 允许/拒绝/回复，就地决策
│   ├── 正在跑      ← 相位 + 已耗时 + 停止
│   └── 刚完成      ← 结论摘要 + 进详情
├── 项目 Projects  ← 现在的树，降为"浏览/新建"入口
└── 设置 Settings  ← 桌面管理 · 主题 · 字号 · 连接诊断（顶部那条状态栏收进来）
```

三条设计约束：

1. **「等我处理」必须能在不进入会话的情况下完成决策。** 这是指挥台和聊天客户端的
   分界线。现在必须"进会话 → 滚到顶 → 找 banner"，等于把决策藏在浏览动作后面。
2. **全局待办在任何页面都可见可达。** 进了会话 A 就看不到会话 B 在等我，是当前
   最伤的一处：用户没有理由相信"没看到就是没有"。
3. **终端是二级入口。** 从会话卡片里点"打开完整终端"，而不是点会话就掉进 xterm。

### 为什么是底部导航而不是继续用筛选器

现在的 `SegmentedButton`（Action / Running / All）**伪装成筛选器，实际是导航**：
切到 All 是两级树，切到 Action/Running 是平铺列表——同一个控件切出来的是两种
信息结构。用户切一下整个布局形态就变了，这是控件语义和实际行为的错配。

---

## 落地分片

按"每片独立可验证、做完就能看到效果"排，不一次性重做（沿用
`ui-design-language.md` 结尾的同一条原则）。

| # | 分片 | 设计稿 | 依赖 | 备注 |
|---|---|---|---|---|
| 1 | **全局待办徽标**：任何页面可见的待办计数 + 一键回到待办列表 | D / E 的 AppBar | 无（`SessionSummary.attention` 已有） | ✅ 已落地 `widgets/pending_action_badge.dart`；首页的徽标已随分片 4 挪到底部导航的 Console 角标上 |
| 2 | **推送通知 + 深链** | F「通知」分组 | **推送架构需产品拍板** | ⏸ 暂缓（2026-08 产品决定先不做）。roadmap 虽有要求，但本地通知做不到——见下方「分片 2 卡在哪」 |
| 3 | **操作台卡片**：不进会话就能批 | A / B | 无，全部是现成协议 | ✅ 已落地 `services/pending_actions_controller.dart` + `widgets/approval_card.dart`；见下方「为什么不做多会话订阅」 |
| 4 | **底部导航 + 首屏换成指挥台**，项目树降级 | A / C 底部导航 | 1、3 | ✅ 已落地 `pages/console_page.dart`；退役了 `SessionFilterBar`，设置页先承接设计稿 F 的「连接」组 |
| 5 | **移动端设计 token**：语义状态色 · `ThemeMode.system` · 字号 · `Semantics` | F「外观」· E 的 Aa | 无 | ✅ 已落地 `theme/smelt_theme.dart` + `services/terminal_prefs_store.dart`；见下方「移动端设计 token 是怎么定的」 |
| 6 | **i18n** | 设计稿全中文仅为示意 | 5 | 目前全英文 + 零星中文，无 `localizationsDelegates` |

### 移动端设计 token 是怎么定的

色值不是新调的，是照抄 `crates/smelt-ui/src/ui_theme.rs` 的 `DARK` / `LIGHT` 两张表；
五态语义照抄 `crates/smelt-core/src/agent_status.rs`（等审批红 > 需处理黄 > 运行蓝 >
完成绿 > 空闲灰）。桌面那边已经因为「0xef4444/0xf59e0b 之类散落各处」收敛过一次
（见 `agent_status_color` 上方注释），移动端只是补上同样的一步。

改造前移动端的实际状况，两个问题：

1. **同一个概念两种颜色。** `_getStatusChip` 把「等审批」画成红，
   `_buildPhaseIndicator` 画成橙——同一个会话在列表里是红的，点进去变成橙的。
2. **用的是 Material 原生色。** `Colors.orange` / `Colors.green` 跟桌面的黄和绿
   不是一个颜色，两端并排看像两个产品。

顺带纠正了一处语义：`AcpPhaseEnded` 原本画成红。查 `acp_session.rs` 的 `finish_turn`
才确认，正常结束一轮走的是 `Idle`，`Ended` 只来自 `Fatal` / `RestoreFailed`——所以它
**确实是错误终态**，红色是对的，保留（用 `danger` token 而非 `waitingApproval`，
两者同色但语义不同）。

**表面色也不交给 `ColorScheme.fromSeed`。** 起初只把状态色接了过来，表面色仍由 seed
算法生成——真机上一看就露馅：浅色整屏泛淡紫，深色压到接近纯黑，跟设计稿完全不是
一个东西。现在 `surface` / container 梯子 / 顶栏 / 底栏 / 描边全部取自 `ui_theme.rs`。
注意**深色层级跟 Discord 那版相反**（舞台 `#070707` 最暗，顶栏底栏 `#111111`
略抬起），这是 Grok Bot 那套「窗口底和舞台同色近黑、卡片上抬」，`ui_theme.rs` 里 `DARK`
上方专门写了「别顺手改回去」。「顶栏和舞台错开一档」这层关系 Material 的 container
梯子表达不了，所以顶栏/底部导航单独指定。

**再上一次模拟器又抓到一层：容器梯子只保证「一级比一级亮」，保证不了「哪些东西该是
凹的」。** 命令块 / 代码块在设计稿里取的是最暗的 `bg_rail`，是**下沉**的；实现里图省事
接了 `surfaceContainerHighest`，结果深色下它成了整张卡里最亮的一块，方向正好反了。
这类错误单测和 `flutter analyze` 一个都发现不了。现在加了 `sunken` token —— 它是少数
**在深浅两色下方向不一致**的语义（深色取 `bg_rail`，浅色取 `bg_row_hover`），正好说明
为什么这层必须显式命名、不能靠梯子推。同时补了两条测试：`sunken` 必须暗于卡片面、
容器梯子在两套主题里都严格单调（浅色原先 High 和 Container 撞了同一个色）。
输入区同理，原来跟消息区同为 `surface`、只靠一条描边分隔，现在落到 `bar` 面。

**为什么做成 `ThemeExtension` 而不是全局常量**：常量没法随 `Brightness` 切换，而状态色
在浅色底上必须整体压深才读得出来。有了它，`ThemeMode.system` 才有地方落。

**浅色模式的风险比想象中低**：终端配色本来就由那台设备下发（`terminalReady.theme`），
不受手机主题影响；QR 页的黑白是压在相机预览的遮罩上，本来就该是深色。真正需要改的
只有 `Colors.grey[800]` 这种——它在浅色底上会炸出一条突兀的黑线，换成 `outlineVariant`。

**终端字号**（设计稿 E 的 Aa）给的是**档位而不是滑杆**：终端是等宽网格，字号变化会连带
重算 cols/rows 并向 PTY 发 resize，连续拖动等于对着远端狂发 resize。默认档保持改造前
写死的 13pt，老用户升级后看到的字号不变。

**`Semantics`**：只补在「颜色/图标是唯一信息载体」的地方——终端连接指示灯、会话 phase
指示器。这两处同时也是色盲不可用的点，补文字标签一次解决两个问题。

### 设计稿对照：第一次自查漏掉了什么

分片 1 落地后曾判断「按设计实现完了」，实际拿到手机上一看，**除了底部三个 tab，
其余跟改造前几乎一样**。回头逐屏对了一遍设计稿，漏的是这些：

| 设计稿 | 当时状态 | 现在 |
| --- | --- | --- |
| A 指挥台「刚完成」段 | **整段没做** | ✅ 补上 |
| A 审批卡 meta 行（项目色块 / agent / 时间） | 只有一个红点 + 项目名 | ✅ 四件齐了 |
| A 正在跑行（脉冲点 / 项目·动作 / 时间） | 一个转圈菊花 | ✅ 重做 |
| C 项目色块头像 + 状态点 | 没做，还是 ExpansionTile 默认样式 | ✅ 补上 |
| C 「N 个会话 · M 个等你」 | 只有「N sessions」 | ✅ 补上 |
| C 会话行状态点 + 状态短语 | 右侧一个状态 Chip | ✅ 换成点 + 短语 |
| B 审批详情 sheet | 没做 | ✅ 补上 |
| D 会话内运行状态条 / 上下文 chip | 已有，但配色跑偏 | ✅ 对齐 |

**「刚完成」这段为什么最要命**：指挥台原来只画「等我处理」和「正在跑」，剩下的会话
一律不显示。结果是一台刚跑完一堆活儿的机器，在指挥台上看起来跟没开机一样——首屏
永远是「Nothing needs you」。

而且它**不需要我编时间窗**：客户端用 `unread && phase=='succeeded'` 自算「刚完成
（Recently ran）」——`status` 只发 `needs_you` / `running` / `idle` 三态，**没有 `done`**，
见 `crates/smelt-remote-gateway/src/lib.rs` 的 `mobile_status_name` 与
`crates/smelt-core/src/agent_status.rs`。未读直接读摘要上的 `unread`；看过一眼 `unread`
变 false，这一段就自己清空。

**项目身份色只在移动端。** 六色取自桌面语义色（accent / green / blue / purple /
yellow / red），哈希是写死的 FNV-1a，只保证手机内部稳定。色块用来在这一屏区分
彼此；桌面侧栏不画项目色块。协议若以后下发颜色，只换 `projectAccent`。

**审批详情 sheet（B）落地时的两个决定**：

1. **设计稿上的「影响范围」没有对应字段就不画。** `ApprovalDetailsCommand` 只有
   command / cwd / reason，`file_change` 才有 `grant_root`。与其编一句「只读构建 ·
   不写入工作区」放上去，不如让那一行不存在——审批界面上编造的信息是有害的。
2. **三个动作纵向排列、且不是按重要性排的。** 仅此一次在最上、总是允许居中做成
   幽灵按钮、拒绝在最下。拒绝放最后**不是因为它次要**，而是因为它最可撤销——拒错
   了再点一次就是了，允许错了命令已经跑完。

**D 屏基本是已有的**（状态条、计划面板、上下文用量 chip 都在），但状态条把运行色
写成了 `colorScheme.primary`（蓝紫），而指挥台的运行点用的是 `running` token（蓝）——
同一个会话换个页面就换个颜色，跟当初「列表里红 chip、详情里橙图标」是同一类漂移。
已统一到 token，底色也从 7% 提到设计稿的 12%（原来在深色底上几乎看不出是一条带子）。

### ACP 会话图标：跟桌面用同一套

移动端原来所有 ACP 会话都是一个通用的对话气泡，看不出是哪家 agent。桌面早就不是
这么画的，见 `crates/smelt/src/session_list.rs` 的 `provider_icon`，它上面那句注释
就是整个做法的依据：

> agent 身份图标统一走本地单色 SVG，**颜色由会话状态驱动**：图标同时回答
> 「这是哪家的会话」和「当前处于什么状态」。

移动端照搬这个模型：形状是 who，颜色是 state。**所以会话行上原来那个独立的状态
圆点被去掉了**——图标已经带着状态色，再画一个点是同一条信息画两遍，还把 leading
挤到 30pt（正是上一轮那条 overflow 的来源）。状态短语保留，那是给色盲和读屏的另
一半。

几个决定：

- **用 SVG 不用 PNG。** 仓库里 `assets/agent-*.png` 只有 32×32，放到 3x 屏上的
  20pt 图标（需要 60px）会糊；SVG 是 `fill="currentColor"` 的单色图，正好能按状态
  染色。代价是引入 `flutter_svg`——这是本轮唯一新增的依赖。
- **图标文件是复制过来的一份副本。** Flutter 的 asset 声明不能指到 package 目录外，
  所以 `mobile/assets/agent-icons/` 是 `crates/smelt/assets/icons/` 的拷贝
  （连同 `THIRD_PARTY_NOTICES.md`，LobeHub Icons / MIT）。桌面换图标时这里要跟着换，
  pubspec 里写了提醒。
- **匹配用「包含」而不是「相等」。** 服务端那边是 `c.contains(k.id())`
  （`agent_kind.rs` 的 `from_command_loose`），自定义 agent 的 id 常常是
  `claude-quant` 这种带后缀的形态。精确匹配会让它们全掉进兜底图标。
- **认不出的 ACP 会话退到机器人图标，不退到终端图标。** 后者会把一个 ACP 会话画成
  终端——那不是「信息少了」，是「信息错了」。

映射本体是 `agentIconAssets` 这一张 map，新增一家 agent 等于加一行，不必改 widget。

**另一处同样的问题**：新建会话时的 agent 选择器，原来每一家都是同一个通用机器人
图标——那恰恰是最需要区分身份的界面，等于让用户在一列一模一样的行里挑。已换成
各家图标（`AgentGlyph`）。这里传的是 `kind` 不是 `id`：自定义 agent 的 id 是
`claude-quant`，`kind` 才是 `claude`。

### 新建会话：跟桌面共用同一份目录

原来手机上「新建」只有两条死路径：`New conversation` 列 ACP agent，`New terminal`
固定开一个空白 shell。桌面的「+」弹层则早就是完整目录——各家 agent 的终端启动项、
自定义启动项、空白终端、各家对话，外加用户 Pin 出来的「常用」。两边各写一份，
结果就是**手机根本开不出 agent 终端**。

所以把「有哪些动作、叫什么、怎么分组、Pin 存哪」整块抽到
`crates/smelt-core/src/new_session.rs`，桌面与网关共用同一份真相源：

- **「常用」不再单独归类。** 桌面弹层里常用是顶部区块，手机上则把
  「常用 / 终端 / 对话」三段平铺进同一个底部选择器 —— 手机没有 hover、没有二级
  展开的余地，多一层就多一次点。分组标题、顺序、Pin 状态全由电脑那边算好下发。
- **Pin 在手机上是只读的。** 桌面把偏好缓存在 gpui 全局里，守护进程再写同一份
  SQLite 会双写覆盖且不刷新。手机只展示电脑的 Pin 顺序，不给改 Pin 的入口。
- **手机只回传 `launchKey`，命令永远在电脑侧解析**（`resolve_launch_action`）。
  配对设备不该决定电脑上跑什么进程——要是让手机传命令行，配对本身就变成了
  远程执行任意命令的凭证。
- **旧网关不下发 `launchActions` 时不做兜底。** 空目录直接提示去升级电脑端，
  而不是退回旧的「只能开空白终端」——半残的目录比明确的空状态更难排查。

CLI 探测（逐个跑 `--version`）在网关侧带 120s TTL 缓存，不能每次 `listWorkspace`
都来一轮；但 `resolve_launch_action` **故意不带探测**，否则缓存过期会把用户刚点下的
那一行判成「未知启动项」。

### 又一轮「只有模拟器才看得见」

这一轮抓到三个，静态检查全部沉默：

1. **`updated_at` 是秒不是毫秒。** 按毫秒解，当下的时间戳会算成 1970-01-21，
   界面上齐刷刷显示「1/21」——看着像一批很旧的会话，**不像故障**，这是最危险的
   一类 bug。源头是 `now_unix()` 用的 `Duration::as_secs()`。已写死单位并配测试。
2. **leading 写死 24pt，内容 30pt。** 每一行都挂着一条 `OVERFLOWED BY 6.0 PIXELS`
   警示带。`flutter analyze` 和单测一声不吭。
3. **`ExpansionTile` 展开时会把 trailing 图标染成主色**，导致项目行的 ⋮ 是蓝的、
   会话行的 ⋯ 是灰的。已钉死中性色。

还有一个是**测试自己抓出来的**：详情 sheet 里 `ApprovalDetailsView` 已经渲染过
工作目录和理由，事实表又画了一遍，同一句话出现两次。给 `ApprovalDetailsView` 加了
`showInlineFacts` 开关分工——卡片上要（那是它唯一的展示位），sheet 里交给事实表。

**顺带一条工具教训**：`dart format` 会把长参数拆成多行，之后再用 `sed` 按单行原文
回改就会静默不命中。第一次「把 bug 放回去验证测试」因此假阳性通过了——看着像测试
写好了，其实根本没改到代码。回退验证之后必须确认改动真的落到了文件里。

### 顺手根治的两处旧缺陷

分片 5 过程中发现并修掉的，都不是新功能，是原来就不对的地方：

1. **审批卡在两个地方长得不一样。** 指挥台用的是新 `ApprovalCard`（Allow 描边、
   Always 降级为次要按钮、长命令换行、带工作目录），会话页里还是旧的绿色实心 Allow。
   同一个审批两种渲染，等于安全性修正只做了一半。已让会话页复用 `ApprovalCard`。
2. **Elicitation 的 Submit 会被滚出视野。** 整张卡（含按钮）都在
   `SingleChildScrollView` 里、外面套死 `maxHeight: 360`，字段一多按钮就滚没了，
   而且看不出下面还有个必须点的东西。改成只有问题和字段滚、按钮钉在卡片底部，
   高度上限改成按屏幕比例给。为了能测（会话页依赖全局 `gatewayService` 单例，
   测试替换不掉），把它抽成了 `widgets/elicitation_card.dart`。

### 分片 2 卡在哪：本地通知兑现不了「出门也能收到」

`flutter_local_notifications` 这个包本身没问题——纯 Dart + 平台插件，跟当前的
Flutter/Dart SDK 也不冲突。**问题是它做不到分片 2 要的事。**

本地通知的语义是「让**本进程**弹一条系统通知」。进程没在跑，就没有任何代码去调它。
而当前 App 进后台就会被挂起：

| 证据 | 位置 | 含义 |
|---|---|---|
| 没有 `UIBackgroundModes` | `ios/Runner/Info.plist` | iOS 切后台数秒内挂起，WebSocket 断 |
| 没有前台服务、没有 `WAKE_LOCK` | `android/app/src/main/AndroidManifest.xml`（只有 `INTERNET` + `CAMERA`） | Android 进 doze 后同样断 |
| 只在 `resumed` 时重连 | `main.dart` `didChangeAppLifecycleState` | 后台期间本来就不维持连接 |

于是本地通知只在「App 还活着」的窗口内有效，而那个窗口里**应用内 toast 已经覆盖了**
（`shouldShowAttentionNotification` + snackbar，规则跟
`agent-state-notification-design.md` §2.3 一致）。净收益接近零。

更要紧的是**可靠性**：Android 后台进程能活多久取决于系统心情，iOS 基本活不过切走那
一下。审批通知一旦「有时收得到」，用户就会开始信它，然后在没收到的那次错过审批——
这比不做更糟。做这个功能的全部意义就是让用户可以不用盯着，做成半可靠等于没做。

**真要兑现，只有两条路，都需要产品拍板：**

| 方案 | 能兑现吗 | 代价 |
|---|---|---|
| **APNs / FCM 远程推送** | iOS + Android 都能 | 要一台桌面能访问的推送服务器 + 推送凭证；「哪个会话在等审批」这条元信息要过第三方云。与 `remote-ops-roadmap.md` 选定的 iroh P2P「不用信令、不用中转」直接冲突 |
| **Android 前台服务**（常驻通知栏）保住 WebSocket | 只有 Android | 不依赖任何云，跟 P2P 架构不冲突；但 iOS 无对应能力（VoIP 后台模式是滥用，会被拒审），会变成双端体验不一致 |

在这两者之间做出选择之前，**不引入 `flutter_local_notifications`**——先加依赖不会让
决策变容易，只会让 App 里多一条走不通的路径。

### 为什么不做多会话订阅

指挥台的诉求是「不进会话就能批」，很容易顺手推导成「那得同时订阅多个会话」。
不需要，而且不该做。

先说事实。**smeltd 本来就支持多订阅**——`acp_watch` 的协议注释写着「只读镜像，
会话必须已存在，**可多个并存**」。但网关 `remote_gateway.rs` 里
`current_subscription` 是单个 slot，subscribe(B) 会掐掉 subscribe(A)。这不是疏漏：
**PC 端根本不用 `acp_watch`**（全仓搜索，唯一调用方就是网关自己），它的模型是
一条全局 `subscribe` 拿所有会话的四色状态 + `open`/`acp_open` 管当前那一个会话。
网关的单槽忠实镜像了这个模型。

再说指挥台实际需要什么——三件事，**全部已有现成协议，一件都不用订阅**：

| 需要什么 | 走哪条现成的路 | 要订阅吗 |
|---|---|---|
| 谁在等我 | `attention` / `attentionResolved` 推送 + `SessionSummary.attention` | 否。分片 1 的徽标就是这么跑起来的，没动过协议 |
| 待批卡片的内容 | `loadHistory` → smeltd `acp_snapshot`。`to_snapshot_range` 无条件带上 `pending_permissions` / `pending_elicitation` | 否。`handle_acp_snapshot` 自己开连接，不碰订阅 |
| 回批 | `respondApproval`，参数里带显式 `sessionId` → `send_acp_action` 自己开连接 | 否 |

所以多会话订阅在这里是**负优化**：为了渲染几张审批卡，让手机持续接收 N 个会话的
全量 entry 增量（蜂窝流量 + 电池），还要额外背上订阅数上限、重连时逐个重订、
乱序快照分流这些复杂度。换来的信息，一次性快照已经全给了。

指挥台的正确数据流是**推送驱动 + 按需取详情**：`attention` 到了就取一次那个会话的
快照渲染卡片，`attentionResolved` 到了就把卡片撤掉。审批本来就是低频事件，
不值得为它挂一条常开的流。

### 不做

- 不重做工具调用卡片 / diff / markdown 渲染——这层是好的。
- 不给手机做完整 TUI 体验。`remote-ops-roadmap.md` 已经否掉了：
  「小屏 + 软键盘 + 复杂 TUI 体验往往差」。
- 不引入新的设计风格。桌面已经定了整体参考 Grok Bot，移动端延伸同一套
  实色表面和状态色，不要在手机上另起一套，也不要再借 Telegram 毛玻璃。

## 设置页：设计稿 F 落地

改造前的设置页是一列朴素 `ListTile`，跟设计稿 F 只有「都叫设置」这一点相同。补齐后
对应设计稿的三块：分组标题（标签 + 横贯细线）、连接卡、外观。

**只画协议真有的东西。** 连接卡上的 `LAN · 6ms` 和「链路：局域网直连」不是装饰，
背后是 `ConnectionMetrics{kind, latencyMs}`。链路类型认不出（`unknown`）时那一行
**整行不画**——「未知」这个词对用户没有信息量；没连上时也不画，因为那时的延迟是
上一次的残值，画出来等于骗人。

**设计稿里的「通知」和「语言」两组没有落地。** 它们分别对应分片 2 和分片 6，都还
没做。摆一排点不动的开关比没有更糟：用户会以为自己开了推送，然后错过审批。等能力
真的有了再加。

**两处只有上模拟器才看得见的问题：**

1. **同一条信息画了两遍。** 我把常驻连接状态条原样搬进设置页，结果顶部横幅写着
   `LAN · 4 ms`，正下方卡片的 chip 又写一遍 `LAN · 4ms`。设计稿 F 的原意是把状态条
   **收进**这张卡（正常联通时它对用户是噪音），我做成了叠加。现在正常联通时不画横幅，
   异常时才画。已加回归测试（往 `connectionBar` 塞哨兵文本，断言两种状态下的可见性）。
2. **中英文混排。** 我照着设计稿写了中文文案，但全 App 现有文案都是英文
   （`Console` / `Projects` / `Needs you`…）。i18n 落地之前，混排比统一用哪一种都糟，
   已统一到英文。设计稿是中文的，那是文档语言，不是产品语言。

**主题模式必须住在 `MaterialApp` 之上。** 它决定整棵树用哪套配色，放在 `home` 里改不
动自己头顶那个 `MaterialApp`——所以 `SmeltApp` 从 `StatelessWidget` 改成了 Stateful。

**终端字号有了第二个入口。** 原来只有终端页的 `Aa` 菜单，现在设置页也能调。两处共用
**同一个 store 实例**（同一条写队列，不会交错落盘），并且退出终端页时回读一次，免得
设置页的滑杆停在旧值。设计稿画的是连续滑杆，实现用离散档位：字号变化会重算终端
cols/rows 并向 PTY 发 resize，连续拖动等于对着远端狂发 resize。`divisions` 让手柄吸附
到 `TerminalPrefs.steps`，观感仍是滑杆。

**落盘逻辑抽成了 `JsonPrefsFile<T>`。** 「终端字号」和「主题」是两类互不相干的偏好，
各有各的文件和默认值，但读写规则（宽容读、串行写、读不出来给默认值）完全一样。抽出来
之后再来第三类偏好只要给一组 `decode/encode/fallback`，不必第三次抄同一段容错逻辑。
`FileTerminalPrefsStore` 改成委托给它，公开 API 和文件名都没变，原有 8 条测试原样通过。

### 顶部常驻连接条：删掉遥测，留下警告

设置页的连接卡上线后，指挥台 / 项目 / 会话三屏顶部那条 `LAN · 4 ms` 就成了纯粹的
重复。查设计稿确认过：**这个 chip 全篇只在 F（设置）出现过一次**，其余屏幕都没有。

但不能整条砍掉。这个位置上其实叠着两个不同的东西：

- `ConnectionStatusBar` —— 纯链路遥测（`LAN · 4 ms`）。**已删除**，全仓无引用。
- `CachedConnectionBar` —— 「Offline · Showing saved data」加一个 Retry 按钮。这是
  带动作的真警告，**必须留**。设计稿 F 的说明里也写了「异常仍在顶部弹条」。

三处调用点原本各写了一遍同样的三元判断，而且已经开始漂移（列表页判断
`sessionsAreCached`，会话页判断 `snapshotIsCached(id)`）。统一收进
`buildConnectionBanner()`：正常联通时返回零高度，异常或在用缓存时才画。`cached`
仍由各屏自己传——「在用缓存」问的是**这一屏**的数据，不是全局，这个差异是对的，
不能在收敛时抹平。

顺带解决了一个连贯性问题：三屏用同一个入口，翻页时横幅不会忽隐忽现。

### 指挥台第四段「Recently ran」：接住看过就消失的那些

「刚完成」这一段是**看过就消失**的，这本来是刻意的（`done` 自带未读语义，读过退回
`idle`，段落会自己清空，不会越堆越长）。但它有个没想到的副作用：一次跑完的会话在
指挥台上凭空没了，用户失去了「最近跑的是哪个」这条线索。第四段接住它们。

**又一次不用自己编时间窗。** `phase` 和 `status` 是两个独立字段，读过之后只有
`unread` 退回 false，`phase` 仍然是 `succeeded`：

```dart
// mobile/lib/models/session_filters.dart
bool sessionRecentlyDone(SessionSummary session) {
  ...
  return session.unread && session.phase.toLowerCase() == 'succeeded';
}
```

status 只发 `needs_you` / `running` / `idle`，**没有 `done`**；「刚完成」由客户端
用 `unread && phase=='succeeded'` 自算。所以「跑完且已读」= `phase == succeeded && !unread`，
是客户端谓词守住的事实。

**排除 `disconnected`。** 运行时没了（桌面重启后没恢复会话）时 `phase` 只是历史残留，
不排除的话桌面一重启，这一段会被一批陈年会话灌满。

**只截断，不设时间窗。** 这一段跟前三段不同，它**不会自己清空**——`succeeded` 会一直
挂到该会话下次有动静。所以按最后活动时间倒序 + 截断（默认 6 条，`recentRunLimit`
可调）。没有加「只看 24 小时内」这类窗口，因为那会在「今天还没跑过东西」时把答案
整个藏起来，恰好违背这一段存在的理由。截断时明说「N more in Projects」，不让被截掉
的那几条无声消失。头部计数用**全量**，那个数字不能骗人。

**顺带根治了一个建在巧合上的刷新。** `PendingActionsController._rebuild()` 原来只在
待办卡片变化时 `notifyListeners`，而 `running` / `recentlyDone` / `recentRuns` 都直接读
`_sessions`。「会话跑完了但没有任何待办变化」这类更新它是不通知的——今天能刷新，
只是因为父级 HomePage 恰好也在监听会话流并 setState。现在把派生列表的指纹一起比，
controller 自己就是对的。已验证：把这段判断改回去，对应测试立刻失败。

### 会话行上的项目名为什么会随机消失

现象：指挥台上「Copilot 对话 · smelt」这一行没有项目名，别的行都有。

根因是 `_ConsoleRow` 里一条我自己写错的去重规则——**标题里包含项目名就不画项目行**。
当时的注释写着「服务端的标题常常是『Copilot 对话 · smelt』这种自带项目后缀的形态」，
这个前提是**假的**。查 `crates/smelt-remote-gateway/src/lib.rs` 可知，title 依次取
`custom_title`（用户手打）→ `launch_label` → 路径名，**从不追加项目名**。那个 ` · smelt`
是用户自己起的标题。

于是这条规则只在用户碰巧把项目名写进自定义标题时才触发，行结构随机缺一段。而且
`contains` 太松：项目名叫 `api` 之类的短词时，几乎每一行都会被误吞。

改成**只在标题与项目名完全相同时才省**（终端会话标题默认取目录名，常常就等于项目名，
那时重复一遍确实是噪音）。分诊列表里项目名在固定位置可扫，比省掉一次重复重要得多。

教训跟本轮反复出现的是同一条：**注释里写的「服务端常常……」如果没查过，就是猜的。**
猜错了不会报错，只会让界面在某些数据下悄悄少一块。

### 指挥台的会话为什么也换成 agent 图标

Projects 页早就改用了各家 agent 的图标（形状是 who，颜色是 state，跟桌面
`crates/smelt/src/session_list.rs` 的 `provider_icon` 同一套），指挥台三段却还是圆点。
同一个会话在两屏给出两种视觉身份，扫一眼认不出「刚才在 Projects 看到的那条」就是
这条。圆点只编码了状态，把 who 整个丢掉了——而分诊场景里「哪个 agent 在等我」跟
「它处在什么状态」同等重要。

三段统一换成 `AgentIcon` 之后：

- **只有 Running 段保留脉冲。** 「还在动」是这一段存在的全部理由，静止图标区分不出
  「正在跑」和「跑完了」，光靠颜色差别太弱。原来的 `_PulseDot` 重构成通用的 `_Pulse`
  （包裹任意 child），这样身份和活性同时保住，不用退回圆点。
- **`_AgentChip` 用 `AgentGlyph` 而不是 `AgentIcon`。** chip 出现在审批卡的头部，那里
  状态已经由卡片本身的配色表达了；chip 再按状态染一次色只会跟卡片抢，还会在同一块
  区域出现两处含义相同的颜色。`AgentGlyph` 只按 agent 取形状，颜色走
  `onSurfaceVariant`。
- leading 槽从 16 放宽到 20（圆点 9pt → 图标 18pt）。这正是上一轮 `OVERFLOWED BY
  6.0 PIXELS` 的位置，所以补了一条把图标尺寸和槽宽钉在一起的测试，并上模拟器复核过
  三段都不溢出。

顺带记一个测试坑：断言「没有脉冲」不能写 `find.byType(FadeTransition)`——`MaterialApp`
的页面转场自带 4 个，一律会命中。改成给 `_Pulse` 挂 `Key('console-pulse')` 按 key 找。
另外正反两个方向必须各起一个 tester：同一棵树上二次 `pumpWidget` 会复用 `ConsolePage`
的 element，换不掉 controller，断言到的是陈旧的树。

### 会话行为什么要抽成一个组件

指挥台和 Projects 各写了一套会话行，差异全是「先后写出来的」而非有意设计：

| | 指挥台 `_ConsoleRow` | Projects `_buildSessionTile` |
|---|---|---|
| 载体 | `InkWell + Row` | `ListTile` |
| 行高 | ~40 | 56 起 |
| 标题 | `bodyMedium` | `bodyLarge`（ListTile 默认） |
| 副标题 | `labelSmall`，1 行 | `bodySmall`，可折 2 行 |
| 缩进 | 4 | 32 / 16 |

同一个会话在两屏字号、行高、左边缘都对不上，来回切要重新适应一次。现在收敛到
`widgets/session_row.dart` 的 `SessionRow`。

真正该保留的差异只有三处，做成了参数：

- `showProject`：指挥台是**跨项目**的分诊列表，光看标题认不出是哪个项目的事；
  Projects 里行已经挂在项目分组下面，再画一遍是纯重复。
- `leading`：指挥台 Running 段要让图标呼吸，需要包一层动效。
- `trailing`：指挥台放相对时间，Projects 放未读点和操作菜单。

#### 状态短语为什么从文案里去掉

Projects 的副标题原来是 `Idle · terminal · detail`。前两段都是图标已经说过的话——
agent 图标本身就是「形状是 who，颜色是 state」，终端会话画的就是终端图标。留着等于
同一条信息画两遍，还把行撑高了一截。

这里原本有一条**正当**的理由挡着，注释里也写了：「状态不能只靠颜色，色盲用户读不出
蓝点是跑着还是完成了」。这个顾虑是真的，不能一删了之。但它挡不住这次改动，原因有二：

1. 指挥台（分诊主屏）本来就已经全靠颜色了——Projects 单独留一份文字救不了主屏，
   只制造两屏不一致。
2. 真正需要那份信息的是读屏用户，而**颜色对读屏软件根本不存在**。

所以把状态挪进了 `agentIconLabel()`，也就是 `AgentIcon` 的语义标签，从
`"codex conversation"` 变成 `"codex conversation, Running"`。改在图标而不是在
`SessionRow` 里包一层 `Semantics`，是因为图标才是承载状态的那个东西——所有用到
`AgentIcon` 的地方（两屏会话行、审批卡）一次都拿到，不用各自记得包一层。
`SessionRow` 里试过外包一层 `Semantics`，结果是语义树上多出一个节点、标签变成
`"Running\ncodex conversation"` 两段，那是把一件事拆到两处说。

至于色盲用户的视觉通路：终端/各家 agent 的**形状**本来就不同，加上 Running 段的
呼吸动效和未读蓝点，非文字线索并没有全丢。这一条记在这里，将来若要补回状态短语，
应该补在**两屏一起**，而不是只补 Projects。

### 删除会话改成左滑

删除原来藏在行尾一个「⋯」菜单里，而那个菜单**只有 Delete 一项**——为了一个动作
常驻 32pt 宽度、多一次点击，还跟项目分组自己的「⋯」在同一列上下对齐，容易点错。
左滑是移动端列表删除的通用手势（iOS 邮件、Gmail 都是它），也把那一格还给了标题：
长会话名现在能多显示好几个字。

用了 `flutter_slidable`（4.0.3）。手势本身边界很多——跟垂直滚动抢事件、多行同时展开、
点别处收起——自己搓一遍不划算，这类交互该用生态里成熟的实现。

几个刻意的决定：

- **不给 `DismissiblePane`。** 删除会结束终端进程，是不可逆的。一次划到底就完成
  的手势太容易误触，所以保留「滑出按钮 → 点 → 确认对话框」三步。确认框的文案本来
  就区分终端和对话（终端说「结束 shell 进程」，对话说「记录仍在 History 里」），
  这次原样复用。
- **只读配对下连手势都不挂。** 原来菜单项是灰着的 disabled 状态；换成手势后，
  「滑出一个点不动的按钮」比「滑不动」更让人困惑，所以直接不给 `endActionPane`。
- **左滑之外必须有第二条路。** 手势是隐藏的，读屏软件既滑不动也看不见按钮。所以
  行上挂了 `CustomSemanticsAction('Delete')`，走同一个 `_deleteSession`。

#### 为什么把 Slidable 放进 SessionRow 而不是调用方

第一版是在 `main.dart` 的 `_buildSessionTile` 里包 `Slidable`，`SessionRow` 只多收
一个 `semanticsActions` 参数。这样有两个问题：调用方要同时记得挂手势**和**挂无障碍
动作（漏一个就是无声的可访问性缺口），而且删除路径全在私有方法里，测不到。

改成 `SessionRow` 收一个 `onDelete` 回调，自己负责手势和语义动作。调用方只表态
「这行允许删除」。于是滑动这条路终于可测——`test/session_row_test.dart` 里真的
`drag` 出按钮、点它、断言回调触发，还钉住了「划到底不会直接删」和「不给 onDelete
就滑不出任何东西」。

`flutter_slidable` 的依赖也因此圈在 `session_row.dart` 一个文件里（自动收起别行用的
`SlidableAutoCloseBehavior` 也包成了 `SessionRowGroup`），换掉滑动方案时不用动调用方。

#### 删除按钮为什么只有图标

第一版给了 `label: 'Delete'`，真机上文字被拦腰裁掉。会话行高只有 40pt 上下（没有
副标题时更矮），而 `SlidableAction` 的图标叠文字要 48pt 以上。

这个 bug 单测抓不到——`SlidableAction` 是**裁切**内容，不是抛 `RenderFlex overflowed`，
所以 209 条测试全绿、`flutter analyze` 也干净，只有真机截图能看见。又一条记进那份
「只有模拟器才看得见」的清单。

垃圾桶图标本身足够明确，不需要文字复述。读屏那条路也不受影响：它们走的是行上的
`CustomSemanticsAction('Delete')`，根本滑不出这个按钮。测试改成钉「按钮里不许出现
任何 `Text`」并附上原因，把 label 加回去会立刻失败（验证过）。
