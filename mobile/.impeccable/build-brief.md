# Smelt 手机端界面构建说明（给并行开发的每个人）

先读：`mobile/PRODUCT.md`、`docs/mobile-redesign.md`、方向契约 `mobile/.impeccable/surfaces/lib-main-dart.md`、
质量底线 `~/.agents/skills/impeccable/reference/craft-floor.md`、平台约定 `~/.agents/skills/impeccable/reference/ios.md` 与 `android.md`。
参照实现：`lib/features/pass/`（指挥台，招牌动作单据轨）、`lib/features/actions/ticket_bodies.dart`、`lib/features/home_shell.dart`。

## 视觉世界「出菜口」——只借四样：字、配色、密度、一个招牌动作
- 钢灰底（`GalleyColors.ground`）+ 白色搁板分组列表（`Shelf` / `ShelfHeader` / `ShelfRow`）。不做卡片墙，不嵌套卡片。
- **纸只给需要你动手的事**（`TicketPaper`）。其它任何地方都不用纸色、不画单据。
- **琥珀只给「需要你」**：`GalleyButton.heat` 只用在单据上你要按的那颗按钮；普通主动作用 `GalleyButton.solid`（实心墨），次要用 `.quiet`。
- 等宽字（`GalleyText.monoHead / monoTime / code`，JetBrains Mono NL）只用于：单据头、耗时、命令、代码、diff、终端。不当「技术感」装饰。
- 数字等宽表格对齐；耗时用 `ElapsedLabel`，相对时间用 `formatAgo`（`lib/ui/time.dart`）。
- 每条内容都带它所在机器的灶台标签色（`GalleyColors.station(accent)` + `StationTape` / `StationLabel`）。
- 状态用 `StatusMark`（图标 + 颜色 + 读屏标签），状态不能只靠颜色。

## 硬规则
- **原始值只在 `lib/theme/`**：feature 代码里不得出现 `Color(0x..)`、`Colors.*`（`transparent` 除外）、`Curves.*`、`Duration(milliseconds:` 字面量、`BorderRadius.circular(数字)`、`fontSize: 数字` 的裸写。用 `GalleyColors / GalleyText / Space / Radii / Strokes / Motion`。确实缺 token 就在报告里提出来，别自己塞进 `lib/theme`（由主线程统一加）。例外：在已有 `GalleyText` 样式上 `.copyWith(fontSize: …)` 微调可以，但尽量少。
- 动画时长一律经 `Motion.resolve(context, …)`：系统要求减少动态效果时归零。
- 图标只用 Lucide（`package:lucide_icons_flutter/lucide_icons.dart` 的 `LucideIcons.*`），不用 Material `Icons.*`、不用 emoji / Unicode 字符当图标。agent 身份用 `AgentGlyph`。
- 触控区 ≥ 48；跟随系统字号（不要写死行高导致大字号截断）；深浅色都要对。
- 标准控件：返回手势、底部弹层（`showModalBottomSheet`）、对话框、开关、分段控件都用 Flutter 自带的，样式已由主题统一；不要重造导航或控件。
- 不加 eyebrow 小标签、不用渐变字、不做装饰性毛玻璃、不用 1px 以上的彩色侧边框。
- 文案：所有界面文字走 `L10n`（`lib/l10n/gen/l10n.dart`）。在 `lib/l10n/src/<你的功能>_en.arb` 和 `_zh.arb` 里加键，键名以功能名开头（如 `conversation…`）。中英都要有。改完跑 `dart run tool/merge_arb.dart && flutter gen-l10n`。不要改别人的片段文件。不出现厨房行话。
- 数据：只通过 `AppScope.of(context)` 拿 `AppModel`，调用 `model.backend`（`lib/data/backend.dart`），读 `model.fleet / model.pass / model.catalog / model.prefs`。不直接 import `src/rust`（类型从 `lib/data/models.dart` 拿）。界面不做业务判断（哪些在等你、怎么排序在内核）。
- 打开会话用 `openSession(context, runtimeId, session)`（`lib/features/session/open_session.dart`）；打开其它详情用 `DetailHost.of(context).open(key, builder)`，这样平板上进右侧详情栏、手机上压栈。
- render 里不做 IO：`build` 里不读写文件、不发请求；放在 initState / 事件里，结果存在 State 上。
- 共享组件放 `lib/ui/` 已有的；你的功能私有组件放你自己的 feature 目录。**不要改别人的目录和 `lib/ui/`、`lib/theme/`、`lib/data/backend.dart`、`lib/data/stores.dart`**；需要改就写进报告。演示数据只改分配给你的 `lib/data/demo/` 文件。

## 自检（自己跑，跑到绿）
```
export PATH=$HOME/.smelt-ci/toolchains/flutter/bin:$PATH
cd mobile
dart run tool/merge_arb.dart && flutter gen-l10n
flutter analyze lib/features/<你的目录> test/<你的测试>
perl -e 'alarm 170; exec @ARGV' flutter test --timeout 30s -r failures-only test/<你的测试文件>
```
- flutter test 会无声挂死：一律套 `perl alarm` 整体超时；被杀后 `pkill -9 -f flutter_tester` 清锁。不碰 widget 的逻辑用 `fake_async`，别在 testWidgets 里 await 流的 close。
- widget 测试用 `DemoBackend` + `AppModel(DemoBackend(), PreferencesStore.memory())`，外面包 `MaterialApp(theme: buildGalleyTheme(...), localizationsDelegates: L10n.localizationsDelegates, supportedLocales: L10n.supportedLocales, home: ...)` 与 `AppScope`；需要 `DetailHost` 时手动包一个。
- 别的目录还没写完时整包 analyze 会报它们的错，只看你自己目录的。

## 交付报告
改了哪些文件；对共享层（ui / theme / data / backend）的需求；没做完或偏离说明的地方；测试结果。不要 git commit。
