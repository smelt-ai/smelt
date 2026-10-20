/// 第二层：语义槽。界面只读这里，不碰 [Primitives]。
///
/// 读法一律 `GalleyColors.of(context)`，缺了就 assert，不回退：回退等于悄悄换上一套
/// 没验过对比度的配色。
library;

import 'dart:ui' show lerpDouble;

import 'package:flutter/material.dart';

import 'primitives.dart';

@immutable
class GalleyColors extends ThemeExtension<GalleyColors> {
  const GalleyColors({
    required this.ground,
    required this.shelf,
    required this.shelfPressed,
    required this.raised,
    required this.hairline,
    required this.rail,
    required this.paper,
    required this.paperEdge,
    required this.onPaper,
    required this.onPaperMuted,
    required this.paperControl,
    required this.ink,
    required this.inkMuted,
    required this.inkFaint,
    required this.heat,
    required this.onHeat,
    required this.heatInk,
    required this.danger,
    required this.dangerWash,
    required this.ok,
    required this.okWash,
    required this.link,
    required this.control,
    required this.onControl,
    required this.controlSelected,
    required this.solid,
    required this.onSolid,
    required this.shadow,
    required this.scrim,
    required this.stations,
  });

  /// 钢底：页面背景。
  final Color ground;

  /// 白色搁板：分组列表、输入框底。
  final Color shelf;
  final Color shelfPressed;

  /// 浮层（底部弹层、菜单、对话框）。
  final Color raised;
  final Color hairline;

  /// 单据轨：一道平涂的结构线。不用渐变去假装金属：钢只在配色里，不在质感里。
  final Color rail;

  /// 单据纸。深色模式也是纸：夜里屏幕上唯一亮着的就是要你动手的事。
  final Color paper;
  final Color paperEdge;
  final Color onPaper;
  final Color onPaperMuted;

  /// 单据纸上的次要按钮底（拒绝、跳过）：比纸深一档，深色模式下不至于是一块钢。
  final Color paperControl;

  final Color ink;
  final Color inkMuted;

  /// 最弱的字：只放在搁板上或大字号上，钢底上也过 4.5。
  final Color inkFaint;

  /// 保温灯琥珀：只给「需要你」——单据上你要按的那颗按钮、等你处理的标记。
  final Color heat;
  final Color onHeat;

  /// 「需要你」作文字时用的深琥珀（琥珀本身做文字不够对比度）。
  final Color heatInk;

  final Color danger;
  final Color dangerWash;

  /// 在做：状态点与耗时。
  final Color ok;

  /// diff 新增行等「好」的浅底。
  final Color okWash;
  final Color link;

  /// 中性按钮底（拒绝、次要动作）。
  final Color control;
  final Color onControl;

  /// 分段控件等的选中块：浅色是白，深色要比中性底亮一档，选中才不像反了。
  final Color controlSelected;

  /// 实心墨按钮（发送、主要但不是「需要你」的动作）。
  final Color solid;
  final Color onSolid;

  final Color shadow;
  final Color scrim;

  /// 灶台标签色板，序号即机器登记里的 `accent`。
  final List<Color> stations;

  Color station(int accent) => stations[accent % stations.length];

  static GalleyColors of(BuildContext context) {
    final colors = Theme.of(context).extension<GalleyColors>();
    assert(colors != null, 'GalleyColors 缺失：ThemeData 要经 buildGalleyTheme 构建');
    return colors!;
  }

  static const light = GalleyColors(
    ground: Primitives.steel93,
    shelf: Primitives.white,
    shelfPressed: Primitives.steel97,
    raised: Primitives.white,
    hairline: Primitives.steel90,
    rail: Primitives.steel82,
    paper: Primitives.paper99,
    paperEdge: Primitives.paperEdge84,
    onPaper: Primitives.ink12,
    onPaperMuted: Primitives.ink38onPaper,
    paperControl: Primitives.paper88,
    ink: Primitives.ink12,
    inkMuted: Primitives.ink40,
    inkFaint: Primitives.ink45,
    heat: Primitives.heat70,
    onHeat: Primitives.ink12,
    heatInk: Primitives.heatInk38,
    danger: Primitives.red45,
    dangerWash: Primitives.red95,
    ok: Primitives.green32,
    okWash: Primitives.green95,
    link: Primitives.blue45,
    control: Primitives.steel90,
    onControl: Primitives.ink12,
    controlSelected: Primitives.white,
    solid: Primitives.ink12,
    onSolid: Primitives.white,
    shadow: Primitives.shadow,
    scrim: Primitives.scrim,
    stations: Primitives.stationLight,
  );

  static const dark = GalleyColors(
    ground: Primitives.steel08,
    shelf: Primitives.steel11,
    shelfPressed: Primitives.steel14,
    raised: Primitives.steel14,
    hairline: Primitives.steel18,
    rail: Primitives.steel24,
    paper: Primitives.paper92,
    paperEdge: Primitives.paperEdge70,
    onPaper: Primitives.ink12,
    onPaperMuted: Primitives.ink38onPaper,
    paperControl: Primitives.paper84,
    ink: Primitives.ink93,
    inkMuted: Primitives.ink68,
    inkFaint: Primitives.ink60,
    heat: Primitives.heat74,
    onHeat: Primitives.ink12,
    heatInk: Primitives.heatInk76,
    danger: Primitives.red68,
    dangerWash: Primitives.red20,
    ok: Primitives.green72,
    okWash: Primitives.green18,
    link: Primitives.blue74,
    control: Primitives.steel18,
    onControl: Primitives.ink93,
    controlSelected: Primitives.steel30,
    solid: Primitives.ink93,
    onSolid: Primitives.ink12,
    shadow: Primitives.shadowDeep,
    scrim: Primitives.scrim,
    stations: Primitives.stationDark,
  );

  @override
  GalleyColors copyWith({
    Color? ground,
    Color? shelf,
    Color? shelfPressed,
    Color? raised,
    Color? hairline,
    Color? rail,
    Color? paper,
    Color? paperEdge,
    Color? onPaper,
    Color? onPaperMuted,
    Color? paperControl,
    Color? ink,
    Color? inkMuted,
    Color? inkFaint,
    Color? heat,
    Color? onHeat,
    Color? heatInk,
    Color? danger,
    Color? dangerWash,
    Color? ok,
    Color? okWash,
    Color? link,
    Color? control,
    Color? onControl,
    Color? controlSelected,
    Color? solid,
    Color? onSolid,
    Color? shadow,
    Color? scrim,
    List<Color>? stations,
  }) => GalleyColors(
    ground: ground ?? this.ground,
    shelf: shelf ?? this.shelf,
    shelfPressed: shelfPressed ?? this.shelfPressed,
    raised: raised ?? this.raised,
    hairline: hairline ?? this.hairline,
    rail: rail ?? this.rail,
    paper: paper ?? this.paper,
    paperEdge: paperEdge ?? this.paperEdge,
    onPaper: onPaper ?? this.onPaper,
    onPaperMuted: onPaperMuted ?? this.onPaperMuted,
    paperControl: paperControl ?? this.paperControl,
    ink: ink ?? this.ink,
    inkMuted: inkMuted ?? this.inkMuted,
    inkFaint: inkFaint ?? this.inkFaint,
    heat: heat ?? this.heat,
    onHeat: onHeat ?? this.onHeat,
    heatInk: heatInk ?? this.heatInk,
    danger: danger ?? this.danger,
    dangerWash: dangerWash ?? this.dangerWash,
    ok: ok ?? this.ok,
    okWash: okWash ?? this.okWash,
    link: link ?? this.link,
    control: control ?? this.control,
    onControl: onControl ?? this.onControl,
    controlSelected: controlSelected ?? this.controlSelected,
    solid: solid ?? this.solid,
    onSolid: onSolid ?? this.onSolid,
    shadow: shadow ?? this.shadow,
    scrim: scrim ?? this.scrim,
    stations: stations ?? this.stations,
  );

  @override
  GalleyColors lerp(GalleyColors? other, double t) {
    if (other == null) return this;
    Color c(Color a, Color b) => Color.lerp(a, b, t)!;
    return GalleyColors(
      ground: c(ground, other.ground),
      shelf: c(shelf, other.shelf),
      shelfPressed: c(shelfPressed, other.shelfPressed),
      raised: c(raised, other.raised),
      hairline: c(hairline, other.hairline),
      rail: c(rail, other.rail),
      paper: c(paper, other.paper),
      paperEdge: c(paperEdge, other.paperEdge),
      onPaper: c(onPaper, other.onPaper),
      onPaperMuted: c(onPaperMuted, other.onPaperMuted),
      paperControl: c(paperControl, other.paperControl),
      ink: c(ink, other.ink),
      inkMuted: c(inkMuted, other.inkMuted),
      inkFaint: c(inkFaint, other.inkFaint),
      heat: c(heat, other.heat),
      onHeat: c(onHeat, other.onHeat),
      heatInk: c(heatInk, other.heatInk),
      danger: c(danger, other.danger),
      dangerWash: c(dangerWash, other.dangerWash),
      ok: c(ok, other.ok),
      okWash: c(okWash, other.okWash),
      link: c(link, other.link),
      control: c(control, other.control),
      onControl: c(onControl, other.onControl),
      controlSelected: c(controlSelected, other.controlSelected),
      solid: c(solid, other.solid),
      onSolid: c(onSolid, other.onSolid),
      shadow: c(shadow, other.shadow),
      scrim: c(scrim, other.scrim),
      stations: [
        for (var i = 0; i < stations.length; i++)
          c(stations[i], other.stations[i % other.stations.length]),
      ],
    );
  }
}

/// 字。正文用系统字（iOS SF、Android Roboto，中文落到系统 CJK 字体），单据头、
/// 耗时、命令、代码、终端用随包的 JetBrains Mono NL。数字一律等宽表格数字。
///
/// 样式不带颜色：颜色在用处从 [GalleyColors] 取，同一字号在纸上和钢上颜色不同。
@immutable
class GalleyText extends ThemeExtension<GalleyText> {
  const GalleyText({
    required this.largeTitle,
    required this.title,
    required this.headline,
    required this.body,
    required this.bodyStrong,
    required this.meta,
    required this.note,
    required this.callout,
    required this.button,
    required this.navTitle,
    required this.codeRow,
    required this.label,
    required this.caption,
    required this.monoHead,
    required this.monoTime,
    required this.code,
  });

  /// 顶层页大标题。
  final TextStyle largeTitle;

  /// 弹层、详情页标题。
  final TextStyle title;

  /// 行标题、单据正文标题。
  final TextStyle headline;
  final TextStyle body;
  final TextStyle bodyStrong;

  /// 行副标题（agent · 项目）。
  final TextStyle meta;

  /// 说明文字（空状态正文、状态条、提示）：比 meta 大一号，读起来不费劲。
  final TextStyle note;

  /// 紧凑正文（搁板里次要的行、选项）。
  final TextStyle callout;

  /// 按钮字。
  final TextStyle button;

  /// 内联导航栏标题。
  final TextStyle navTitle;

  /// 可点的等宽行（斜杠命令列表等）。
  final TextStyle codeRow;

  /// 分组标题。
  final TextStyle label;
  final TextStyle caption;

  /// 单据头（序号 · 机器 · 等待时长）。
  final TextStyle monoHead;

  /// 行尾耗时。
  final TextStyle monoTime;

  /// 命令、代码、diff。
  final TextStyle code;

  static const monoFamily = 'JetBrainsMonoNL';
  static const _tabular = [FontFeature.tabularFigures()];

  static GalleyText of(BuildContext context) {
    final text = Theme.of(context).extension<GalleyText>();
    assert(text != null, 'GalleyText 缺失：ThemeData 要经 buildGalleyTheme 构建');
    return text!;
  }

  static const standard = GalleyText(
    largeTitle: TextStyle(
      fontSize: 32,
      height: 1.15,
      fontWeight: FontWeight.w700,
      letterSpacing: -0.4,
    ),
    title: TextStyle(fontSize: 20, height: 1.25, fontWeight: FontWeight.w600),
    headline: TextStyle(fontSize: 16, height: 1.3, fontWeight: FontWeight.w600),
    body: TextStyle(fontSize: 16, height: 1.45, fontWeight: FontWeight.w400),
    bodyStrong: TextStyle(
      fontSize: 16,
      height: 1.45,
      fontWeight: FontWeight.w600,
    ),
    meta: TextStyle(
      fontSize: 13,
      height: 1.3,
      fontWeight: FontWeight.w400,
      fontFeatures: _tabular,
    ),
    note: TextStyle(fontSize: 14, height: 1.4, fontWeight: FontWeight.w400),
    callout: TextStyle(fontSize: 15, height: 1.4, fontWeight: FontWeight.w400),
    button: TextStyle(fontSize: 15, height: 1.2, fontWeight: FontWeight.w600),
    navTitle: TextStyle(
      fontSize: 17,
      height: 1.25,
      fontWeight: FontWeight.w600,
    ),
    codeRow: TextStyle(
      fontFamily: monoFamily,
      fontSize: 15,
      height: 1.35,
      fontWeight: FontWeight.w400,
    ),
    label: TextStyle(fontSize: 13, height: 1.3, fontWeight: FontWeight.w600),
    caption: TextStyle(fontSize: 12, height: 1.3, fontWeight: FontWeight.w400),
    monoHead: TextStyle(
      fontFamily: monoFamily,
      fontSize: 12,
      height: 1.3,
      fontWeight: FontWeight.w500,
      fontFeatures: _tabular,
    ),
    monoTime: TextStyle(
      fontFamily: monoFamily,
      fontSize: 13,
      height: 1.3,
      fontWeight: FontWeight.w500,
      fontFeatures: _tabular,
    ),
    code: TextStyle(
      fontFamily: monoFamily,
      fontSize: 13,
      height: 1.45,
      fontWeight: FontWeight.w400,
    ),
  );

  @override
  GalleyText copyWith() => this;

  /// 字号不随明暗变，切换时直接取目标。
  @override
  GalleyText lerp(GalleyText? other, double t) =>
      t < 0.5 ? this : other ?? this;
}

/// 间距：4 的倍数。组内紧，组间松，标题上方比下方大。
abstract final class Space {
  static const xxs = 2.0;
  static const xs = 4.0;
  static const sm = 8.0;
  static const md = 12.0;
  static const lg = 16.0;
  static const xl = 20.0;
  static const xxl = 24.0;
  static const section = 28.0;
  static const page = 16.0;

  /// 触控区下限：iOS 44pt / Android 48dp，统一取 48。
  static const touch = 48.0;

  /// 平板上列表栏宽。
  static const listPane = 380.0;
  static const listPaneNarrow = 320.0;

  /// 单据宽度与下一张单据的露边。
  static const ticketMin = 240.0;
  static const ticketMax = 340.0;
  static const ticketPeek = 28.0;

  /// 顶层页大标题栏展开时的总高（工具栏 + 一行大标题）。
  static const largeTitleBar = 116.0;

  /// 对话阅读栏最大宽度。
  static const readingMax = 720.0;
}

/// 圆角：行与按钮 10，搁板 12，单据 4（纸是方的），弹层 20，输入胶囊 24。
abstract final class Radii {
  static const ticket = Radius.circular(4);
  static const control = Radius.circular(10);
  static const shelf = Radius.circular(12);
  static const sheet = Radius.circular(20);
  static const pill = Radius.circular(24);
  static const chip = Radius.circular(8);
  static const tag = Radius.circular(2);
}

/// 线宽、色带等小尺寸。
abstract final class Strokes {
  static const hairline = 1.0;
  static const stationTape = 4.0;
  static const statusDot = 8.0;
  static const railHeight = 4.0;
  static const ticketTear = 6.0;
  static const icon = 20.0;
  static const iconSmall = 16.0;

  /// 行副标题里的 agent 小图标。
  static const glyphSmall = 13.0;

  /// 分组标题前的灶台色带高度。
  static const headerTape = 14.0;

  /// 小转圈的线宽。
  static const spinner = 2.0;
}

/// 动效。一次只有一个讲究的动作（单据上轨 / 离轨），其余是系统默认转场。
abstract final class Motion {
  static const tap = Duration(milliseconds: 120);
  static const standard = Duration(milliseconds: 220);
  static const ticket = Duration(milliseconds: 420);
  static const stamp = Duration(milliseconds: 520);
  static const pulse = Duration(milliseconds: 1400);

  /// 指数缓出：从已可见的状态快速到位。
  static const easeOut = Cubic(0.16, 1, 0.3, 1);
  static const easeInOut = Cubic(0.65, 0, 0.35, 1);

  /// 系统要求减少动态效果时一律归零，不是「轻一点」。
  static Duration resolve(BuildContext context, Duration full) =>
      MediaQuery.maybeDisableAnimationsOf(context) == true
      ? Duration.zero
      : full;

  static double lerp(double a, double b, double t) => lerpDouble(a, b, t)!;
}
