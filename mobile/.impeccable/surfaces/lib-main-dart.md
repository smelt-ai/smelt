---
version: 1
slug: "lib-main-dart"
primary_target: "lib/main.dart"
related_targets: ["lib/pages","lib/widgets"]
---

# Surface brief · Smelt 手机端（全 App）

Scope：`mobile/lib/` 全部页面（指挥台、会话、对话、终端、智能体与自动化、设置与机器、配对、新建会话），手机 + 平板。Mode：Operate。

任务：跨多台机器一眼看到「等我处理 / 正在跑 / 刚完成」，原地批权限、回一句，必要时进对话或终端。高频：批权限 / 选择题、快速回话、看进展、新开会话（机器 → 项目 → agent）。
拒绝的结果：通用 SaaS 后台（卡片墙、仪表盘数字），黑客终端 cosplay（全屏等宽、荧光绿）。

## Direction contract

THESIS：手机是出菜口，用户是传菜长。每台机器是一个灶台，每个会话是一张单子；**只有需要你动手的单子才是纸**，其余都是钢。拒绝把收件箱做成又一个聊天列表。

OWN-WORLD：拉丝钢灰底（#ECEEF0／夜间 #15171A），白色搁板列表，热敏单据纸 #FFFDF6，墨黑 #1E1E1E。保温灯琥珀 #FF9F1C 只出现在「需要你」的单子和单子上你要按的那颗按钮上；失败红 #D64545。每台机器一条灶台标签色带（固定色板，用户可换）。正文用系统字（SF / Roboto），单据头、耗时、命令、终端用一套等宽字（JetBrains Mono，随 App 打包），数字一律等宽表格数字。

STORY：掏出手机先看到轨上挂着几张单子，不用想是哪台机器；一张一张批掉、回掉，单子盖章滑走；剩下的灶台列表告诉你谁还在做、谁做完了。

FIRST VIEWPORT（手机竖屏，指挥台）：大标题「指挥台」+ 右上「+」新建；标题下一根钢轨，轨上横排挂着需要你的单据纸（锯齿顶边、单据头写序号 · 机器 · 等待时长，正文写 agent 要做什么，第一张直接带「允许 / 拒绝」或「回答」）；轨下按灶台（机器）分组的白色搁板列表：机器名 + 链路状态，下面是在做和刚完成的会话行（状态点、标题、agent · 项目、右侧等宽耗时）。底部标准 tab 栏：指挥台 / 会话 / 智能体 / 设置。

FORM：出菜口单据轨（我的有序候选第 1 位，用户在决策页选中 IMPECCABLE’S PICK）。seed:0fe024310f472532。
- 招牌动作：**单据轨**。新单子从右侧挂上轨；处理后在单子上盖章（「已允许 · 14:02」）并上滑离轨，下一张补位；点开单子进入逐张处理，下一张已预载在一次横滑之外。减少动态效果时改为淡入淡出。
- 沿用落选挑战者的长处：数字一律等宽对齐（ikeda）；滚动时导航栏写出当前所在灶台（lexicon 导词）；琥珀只给「需要你」（orienteering）；逐张处理不回列表（feed）；决定盖章留痕，不消失（ticket wallet）。

FINISH：unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## 未决
- 机器标签色板的具体色值与可选数量（构建时定，需过深浅两种底的对比度）。
- 平板横屏是否三栏（轨 + 列表 + 详情），先按两栏做。
