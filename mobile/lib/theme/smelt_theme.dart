import 'package:flutter/material.dart';

/// 移动端的语义色 token。
///
/// 色值直接照搬桌面 `crates/smelt-ui/src/ui_theme.rs` 的 DARK / LIGHT 调色板，
/// Agent 三态照搬 `crates/smelt-core/src/agent_status.rs`：
/// 要你红 > 运行蓝 > 空闲灰。
///
/// 为什么要有这一层：桌面早就因为「0xef4444/0xf59e0b 之类散落各处」收敛过一次
/// （见 `agent_status_color` 上方注释），移动端却还停在散落阶段，而且已经跑偏了——
/// `_getStatusChip` 把「等审批」画成红，`_buildPhaseIndicator` 画成橙，同一个概念
/// 两种颜色。用 `Colors.orange` 这类 Material 原生色还有第二个问题：它跟桌面的
/// 黄不是一个颜色，两端并排看会觉得是两个产品。
///
/// 之所以做成 `ThemeExtension` 而不是一堆全局常量，是为了让浅色模式有地方落：
/// 常量没法随 `Brightness` 切换，而状态色在浅色底上必须换一组更深的值才读得出来。
@immutable
class SmeltColors extends ThemeExtension<SmeltColors> {
  const SmeltColors({
    required this.waitingApproval,
    required this.needsAttention,
    required this.running,
    required this.done,
    required this.idle,
    required this.danger,
    required this.sunken,
    required this.diffAdd,
    required this.diffDelete,
  });

  /// 等审批：最高优先级，agent 停在那里等你点头。
  final Color waitingApproval;

  /// 需处理：等输入或失败。
  final Color needsAttention;

  /// 运行中。
  final Color running;

  /// 刚完成、还没被看过。
  final Color done;

  /// 空闲 / 已断开——没有需要你关注的活动。
  final Color idle;

  /// 错误终态：provider 挂了、历史恢复失败。跟 [waitingApproval] 同色，
  /// 对应桌面 `ui_theme.rs` 里 `red` 同时承担「等审批」和「失败/拒绝」。
  final Color danger;

  /// 下沉面：代码块 / 命令块的底。**它在深浅两色下的方向是相反的**——深色要比
  /// 卡片更暗（桌面的 `bg_rail`），浅色要比卡片更暗一点（`bg_selected`）。
  /// 拿 `surfaceContainerHighest` 顶替会在深色下变成「浮起的亮块」，正好反了。
  final Color sunken;

  final Color diffAdd;
  final Color diffDelete;

  /// DARK 调色板（`ui_theme.rs` 的 `DARK`，Grok Bot sand-dark）。
  static const dark = SmeltColors(
    waitingApproval: Color(0xffff263c),
    needsAttention: Color(0xffff263c),
    running: Color(0xff3b82f6),
    done: Color(0xff22c55e),
    idle: Color(0xff777777),
    danger: Color(0xffff263c),
    sunken: Color(0xff070707),
    diffAdd: Color(0xff78e2b4),
    diffDelete: Color(0xffff8c98),
  );

  /// LIGHT 调色板（`ui_theme.rs` 的 `LIGHT`）。浅色底上同一批语义色整体压深，
  /// 否则黄和绿在白底上基本看不见。
  static const light = SmeltColors(
    waitingApproval: Color(0xffc21d2e),
    needsAttention: Color(0xffc21d2e),
    running: Color(0xff2563eb),
    done: Color(0xff16a34a),
    idle: Color(0xff777777),
    danger: Color(0xffc21d2e),
    sunken: Color(0xffe8e8e8),
    diffAdd: Color(0xff00673a),
    diffDelete: Color(0xffc21d2e),
  );

  /// 会话状态字符串 → 颜色。字符串取值与网关下发的 `SessionSummary.status`
  /// 一致，未知值一律按空闲处理（新状态不该把 UI 打成异常色）。
  Color forStatus(String status) => switch (status.toLowerCase()) {
    'needs_you' || 'waiting_approval' || 'needs_attention' => waitingApproval,
    'running' => running,
    _ => idle,
  };

  @override
  SmeltColors copyWith({
    Color? waitingApproval,
    Color? needsAttention,
    Color? running,
    Color? done,
    Color? idle,
    Color? danger,
    Color? sunken,
    Color? diffAdd,
    Color? diffDelete,
  }) {
    return SmeltColors(
      waitingApproval: waitingApproval ?? this.waitingApproval,
      needsAttention: needsAttention ?? this.needsAttention,
      running: running ?? this.running,
      done: done ?? this.done,
      idle: idle ?? this.idle,
      danger: danger ?? this.danger,
      sunken: sunken ?? this.sunken,
      diffAdd: diffAdd ?? this.diffAdd,
      diffDelete: diffDelete ?? this.diffDelete,
    );
  }

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is SmeltColors &&
          other.waitingApproval == waitingApproval &&
          other.needsAttention == needsAttention &&
          other.running == running &&
          other.done == done &&
          other.idle == idle &&
          other.danger == danger &&
          other.sunken == sunken &&
          other.diffAdd == diffAdd &&
          other.diffDelete == diffDelete;

  /// `ThemeData` 的相等性会走到扩展上；不给值相等的话，每次 `smeltTheme()`
  /// 都会被当成新主题，白白触发整棵树重建。
  @override
  int get hashCode => Object.hash(
    waitingApproval,
    needsAttention,
    running,
    done,
    idle,
    danger,
    sunken,
    diffAdd,
    diffDelete,
  );

  @override
  SmeltColors lerp(ThemeExtension<SmeltColors>? other, double t) {
    if (other is! SmeltColors) return this;
    return SmeltColors(
      waitingApproval: Color.lerp(waitingApproval, other.waitingApproval, t)!,
      needsAttention: Color.lerp(needsAttention, other.needsAttention, t)!,
      running: Color.lerp(running, other.running, t)!,
      done: Color.lerp(done, other.done, t)!,
      idle: Color.lerp(idle, other.idle, t)!,
      danger: Color.lerp(danger, other.danger, t)!,
      sunken: Color.lerp(sunken, other.sunken, t)!,
      diffAdd: Color.lerp(diffAdd, other.diffAdd, t)!,
      diffDelete: Color.lerp(diffDelete, other.diffDelete, t)!,
    );
  }
}

/// `Theme.of(context).extension<SmeltColors>()` 的短写。
///
/// 兜底到 [SmeltColors.dark] 而不是 `!`：widget 测试经常自带一个裸
/// `MaterialApp`，没有注册扩展，不该因此崩在渲染上。
extension SmeltColorsX on BuildContext {
  SmeltColors get smeltColors =>
      Theme.of(this).extension<SmeltColors>() ??
      (Theme.of(this).brightness == Brightness.light
          ? SmeltColors.light
          : SmeltColors.dark);
}

/// 交互强调：Grok Bot `fill/accent`，与桌面 `accent` 同值。只给 fromSeed 的
/// 残余槽位用；真正的主按钮走近白/近黑，见下面 `primary`。
const _seed = Color(0xff1084fe);

/// 表面色同样取自 `ui_theme.rs`，不交给 `ColorScheme.fromSeed` 生成。
///
/// 用 seed 算出来的中性色会被染上品牌色的色相：浅色下整屏泛蓝，深色下压到接近
/// 纯黑，两者都跟桌面色板对不上。
///
/// **深色层级跟 Discord 那版相反，别顺手改回去**：Grok Bot 是窗口底和舞台同色
/// 近黑，会话列表和卡片往上抬。浅色同理：舞台近白，卡片和栏略压暗。
class _Surfaces {
  const _Surfaces({
    required this.stage,
    required this.column,
    required this.bar,
    required this.card,
    required this.selected,
    required this.onSurface,
    required this.onSurfaceVariant,
    required this.outline,
    required this.outlineVariant,
    required this.error,
  });

  /// 舞台底（桌面的 bg_stage）。
  final Color stage;
  /// 左右栏底（桌面的 bg_column）。
  final Color column;

  /// 顶栏与底部导航（桌面的 bg_bar）。
  final Color bar;
  final Color card;
  final Color selected;
  final Color onSurface;
  final Color onSurfaceVariant;
  final Color outline;
  final Color outlineVariant;
  final Color error;

  static const dark = _Surfaces(
    stage: Color(0xff070707),
    column: Color(0xff111111),
    bar: Color(0xff111111),
    card: Color(0xff181818),
    selected: Color(0xff262626),
    onSurface: Color(0xfff3f3f3),
    onSurfaceVariant: Color(0xff959595),
    outline: Color(0xff262626),
    outlineVariant: Color(0xff181818),
    error: Color(0xffff263c),
  );

  static const light = _Surfaces(
    stage: Color(0xfffcfcfc),
    column: Color(0xfff7f7f7),
    bar: Color(0xfff7f7f7),
    card: Color(0xfff3f3f3),
    selected: Color(0xffe8e8e8),
    onSurface: Color(0xff141414),
    onSurfaceVariant: Color(0xff5a5a5a),
    outline: Color(0xffd5d5d5),
    outlineVariant: Color(0xffe8e8e8),
    error: Color(0xffc21d2e),
  );

  /// Material 的 container 梯子必须单调：深色下越高越亮，浅色下越高越暗。
  /// Grok Bot 深色是舞台最暗、卡片上抬；浅色是舞台最亮、卡片略压暗。
  /// 「顶栏取 bar」这层关系 Material 的梯子表达不了，所以顶栏/底栏单独指定。
  ColorScheme apply(ColorScheme base) => base.copyWith(
    // 主按钮走近白/近黑（Grok Bot fill/primary），强调蓝不当事 CTA。
    primary: base.brightness == Brightness.dark
        ? const Color(0xfffafafa)
        : const Color(0xff141414),
    onPrimary: base.brightness == Brightness.dark
        ? const Color(0xff141414)
        : const Color(0xfffcfcfc),
    error: error,
    onError: const Color(0xfffcfcfc),
    surface: stage,
    onSurface: onSurface,
    onSurfaceVariant: onSurfaceVariant,
    outline: outline,
    outlineVariant: outlineVariant,
    surfaceContainerLowest: stage,
    surfaceContainerLow: bar,
    surfaceContainer: base.brightness == Brightness.dark
        ? const Color(0xff151515)
        : card,
    surfaceContainerHigh: base.brightness == Brightness.dark
        ? card
        : const Color(0xffeeeeee),
    surfaceContainerHighest: selected,
  );
}

ThemeData smeltTheme(Brightness brightness) {
  final isLight = brightness == Brightness.light;
  final surfaces = isLight ? _Surfaces.light : _Surfaces.dark;
  final scheme = surfaces.apply(
    ColorScheme.fromSeed(seedColor: _seed, brightness: brightness),
  );
  return ThemeData(
    colorScheme: scheme,
    useMaterial3: true,
    scaffoldBackgroundColor: surfaces.stage,
    // 顶栏和底部导航取 bg_bar：深色下它比舞台略亮（抬起），浅色下比舞台略暗。
    // 这个「和舞台错开一档」的关系是 Material 的 container 梯子表达不了的。
    appBarTheme: AppBarTheme(
      backgroundColor: surfaces.bar,
      foregroundColor: scheme.onSurface,
      elevation: 0,
      scrolledUnderElevation: 0,
    ),
    navigationBarTheme: NavigationBarThemeData(
      backgroundColor: surfaces.bar,
      elevation: 0,
    ),
    cardTheme: CardThemeData(color: surfaces.card),
    dividerTheme: DividerThemeData(color: surfaces.outlineVariant),
    extensions: [isLight ? SmeltColors.light : SmeltColors.dark],
  );
}
