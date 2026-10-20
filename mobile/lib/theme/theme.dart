/// 第三层：把语义槽折进 Material 的 [ColorScheme] 和组件主题，让标准控件自己长对。
///
/// ColorScheme 手写，不用 `fromSeed`：种子派生的几十个角色不跟着我们的覆盖走。
/// 只写我们读的角色，M3 名字保留，Material 控件据此自我着色。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'galley.dart';

ThemeData buildGalleyTheme(Brightness brightness) {
  final c = brightness == Brightness.dark
      ? GalleyColors.dark
      : GalleyColors.light;
  const t = GalleyText.standard;

  final scheme = ColorScheme(
    brightness: brightness,
    primary: c.solid,
    onPrimary: c.onSolid,
    secondary: c.inkMuted,
    onSecondary: c.shelf,
    tertiary: c.heat,
    onTertiary: c.onHeat,
    error: c.danger,
    onError: c.onSolid,
    errorContainer: c.dangerWash,
    onErrorContainer: c.danger,
    surface: c.ground,
    onSurface: c.ink,
    onSurfaceVariant: c.inkMuted,
    surfaceContainerLowest: c.shelf,
    surfaceContainerLow: c.shelf,
    surfaceContainer: c.shelf,
    surfaceContainerHigh: c.raised,
    surfaceContainerHighest: c.control,
    outline: c.hairline,
    outlineVariant: c.hairline,
    shadow: c.shadow,
    scrim: c.scrim,
    inverseSurface: c.solid,
    onInverseSurface: c.onSolid,
    primaryContainer: c.control,
    onPrimaryContainer: c.ink,
    secondaryContainer: c.control,
    onSecondaryContainer: c.ink,
  );

  final controlShape = const RoundedRectangleBorder(
    borderRadius: BorderRadius.all(Radii.control),
  );
  final buttonText = t.label.copyWith(fontSize: 15);
  const buttonSize = Size(Space.touch, Space.touch);

  return ThemeData(
    useMaterial3: true,
    brightness: brightness,
    colorScheme: scheme,
    scaffoldBackgroundColor: c.ground,
    canvasColor: c.ground,
    extensions: [c, t],
    // 统一语言：不要 Android 的水波纹，也不要 iOS 上格格不入的扩散；按下只换底色。
    splashFactory: NoSplash.splashFactory,
    highlightColor: c.shelfPressed,
    hoverColor: c.shelfPressed,
    focusColor: c.shelfPressed,
    visualDensity: VisualDensity.standard,
    materialTapTargetSize: MaterialTapTargetSize.padded,
    textTheme: TextTheme(
      displaySmall: t.largeTitle,
      // SliverAppBar.large 展开时的大标题读 headlineMedium。
      headlineMedium: t.largeTitle,
      headlineSmall: t.title,
      titleLarge: t.title,
      titleMedium: t.headline,
      titleSmall: t.label,
      bodyLarge: t.body,
      bodyMedium: t.body,
      bodySmall: t.meta,
      labelLarge: buttonText,
      labelMedium: t.label,
      labelSmall: t.caption,
    ).apply(bodyColor: c.ink, displayColor: c.ink),
    iconTheme: IconThemeData(color: c.ink, size: Strokes.icon),
    dividerTheme: DividerThemeData(
      color: c.hairline,
      thickness: Strokes.hairline,
      space: Strokes.hairline,
    ),
    appBarTheme: AppBarTheme(
      backgroundColor: c.ground,
      surfaceTintColor: Colors.transparent,
      foregroundColor: c.ink,
      elevation: 0,
      // 内容滚到导航栏下面时出一道发丝线，不然正文像被拦腰截断。
      scrolledUnderElevation: 1,
      shadowColor: c.hairline,
      centerTitle: true,
      titleTextStyle: t.headline.copyWith(color: c.ink, fontSize: 17),
      systemOverlayStyle: brightness == Brightness.dark
          ? SystemUiOverlayStyle.light.copyWith(
              statusBarColor: Colors.transparent,
            )
          : SystemUiOverlayStyle.dark.copyWith(
              statusBarColor: Colors.transparent,
            ),
    ),
    navigationBarTheme: NavigationBarThemeData(
      backgroundColor: c.shelf,
      surfaceTintColor: Colors.transparent,
      indicatorColor: c.control,
      elevation: 0,
      height: 64,
      labelBehavior: NavigationDestinationLabelBehavior.alwaysShow,
      labelTextStyle: WidgetStateProperty.resolveWith(
        (states) => t.caption.copyWith(
          color: states.contains(WidgetState.selected) ? c.ink : c.inkMuted,
          fontWeight: states.contains(WidgetState.selected)
              ? FontWeight.w600
              : FontWeight.w400,
        ),
      ),
      iconTheme: WidgetStateProperty.resolveWith(
        (states) => IconThemeData(
          size: 22,
          color: states.contains(WidgetState.selected) ? c.ink : c.inkMuted,
        ),
      ),
    ),
    navigationRailTheme: NavigationRailThemeData(
      backgroundColor: c.shelf,
      indicatorColor: c.control,
      selectedIconTheme: IconThemeData(color: c.ink, size: 22),
      unselectedIconTheme: IconThemeData(color: c.inkMuted, size: 22),
      selectedLabelTextStyle: t.caption.copyWith(
        color: c.ink,
        fontWeight: FontWeight.w600,
      ),
      unselectedLabelTextStyle: t.caption.copyWith(color: c.inkMuted),
      labelType: NavigationRailLabelType.all,
    ),
    bottomSheetTheme: BottomSheetThemeData(
      backgroundColor: c.raised,
      surfaceTintColor: Colors.transparent,
      modalBackgroundColor: c.raised,
      showDragHandle: true,
      dragHandleColor: c.hairline,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radii.sheet),
      ),
    ),
    dialogTheme: DialogThemeData(
      backgroundColor: c.raised,
      surfaceTintColor: Colors.transparent,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.all(Radii.sheet),
      ),
      titleTextStyle: t.title.copyWith(color: c.ink),
      contentTextStyle: t.body.copyWith(color: c.inkMuted),
    ),
    popupMenuTheme: PopupMenuThemeData(
      color: c.raised,
      surfaceTintColor: Colors.transparent,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.all(Radii.shelf),
      ),
      textStyle: t.body.copyWith(color: c.ink),
    ),
    snackBarTheme: SnackBarThemeData(
      backgroundColor: c.solid,
      contentTextStyle: t.body.copyWith(color: c.onSolid, fontSize: 15),
      // 提示条上的「重试」「打开」不是「需要你」：不用琥珀，用提示条自己的前景色。
      actionTextColor: c.onSolid,
      behavior: SnackBarBehavior.floating,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.all(Radii.control),
      ),
    ),
    filledButtonTheme: FilledButtonThemeData(
      style: FilledButton.styleFrom(
        backgroundColor: c.solid,
        foregroundColor: c.onSolid,
        disabledBackgroundColor: c.control,
        disabledForegroundColor: c.inkFaint,
        minimumSize: buttonSize,
        shape: controlShape,
        textStyle: buttonText,
        elevation: 0,
      ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: OutlinedButton.styleFrom(
        foregroundColor: c.ink,
        minimumSize: buttonSize,
        shape: controlShape,
        side: BorderSide(color: c.hairline),
        textStyle: buttonText,
      ),
    ),
    textButtonTheme: TextButtonThemeData(
      style: TextButton.styleFrom(
        foregroundColor: c.ink,
        minimumSize: buttonSize,
        shape: controlShape,
        textStyle: buttonText,
      ),
    ),
    iconButtonTheme: IconButtonThemeData(
      style: IconButton.styleFrom(
        foregroundColor: c.ink,
        minimumSize: buttonSize,
      ),
    ),
    switchTheme: SwitchThemeData(
      thumbColor: WidgetStateProperty.resolveWith(
        (states) => states.contains(WidgetState.selected) ? c.onSolid : c.shelf,
      ),
      trackColor: WidgetStateProperty.resolveWith(
        (states) => states.contains(WidgetState.selected) ? c.solid : c.control,
      ),
      trackOutlineColor: WidgetStateProperty.all(Colors.transparent),
    ),
    segmentedButtonTheme: SegmentedButtonThemeData(
      style: SegmentedButton.styleFrom(
        backgroundColor: c.control,
        foregroundColor: c.inkMuted,
        selectedBackgroundColor: c.controlSelected,
        selectedForegroundColor: c.ink,
        side: BorderSide(color: c.control),
        textStyle: t.label,
        shape: controlShape,
      ),
    ),
    chipTheme: ChipThemeData(
      backgroundColor: c.shelf,
      selectedColor: c.solid,
      // M3 的选中芯片不读 secondaryLabelStyle：字色要随状态变，只能放在 labelStyle 里。
      labelStyle: t.label.copyWith(
        color: WidgetStateColor.resolveWith(
          (states) => states.contains(WidgetState.selected) ? c.onSolid : c.ink,
        ),
      ),
      secondaryLabelStyle: t.label.copyWith(color: c.onSolid),
      side: BorderSide(color: c.hairline),
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.all(Radii.chip),
      ),
      showCheckmark: false,
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: c.shelf,
      hintStyle: t.body.copyWith(color: c.inkFaint),
      contentPadding: const EdgeInsets.symmetric(
        horizontal: Space.md,
        vertical: Space.md,
      ),
      border: OutlineInputBorder(
        borderRadius: const BorderRadius.all(Radii.control),
        borderSide: BorderSide(color: c.hairline),
      ),
      enabledBorder: OutlineInputBorder(
        borderRadius: const BorderRadius.all(Radii.control),
        borderSide: BorderSide(color: c.hairline),
      ),
      focusedBorder: OutlineInputBorder(
        borderRadius: const BorderRadius.all(Radii.control),
        borderSide: BorderSide(color: c.ink, width: 1.5),
      ),
    ),
    textSelectionTheme: TextSelectionThemeData(
      cursorColor: c.ink,
      selectionColor: c.heat.withValues(alpha: 0.35),
      selectionHandleColor: c.heat,
    ),
    progressIndicatorTheme: ProgressIndicatorThemeData(
      color: c.ink,
      linearTrackColor: c.control,
      circularTrackColor: Colors.transparent,
    ),
    scrollbarTheme: ScrollbarThemeData(
      thumbColor: WidgetStateProperty.all(c.inkFaint.withValues(alpha: 0.5)),
      radius: Radii.tag,
    ),
    listTileTheme: ListTileThemeData(
      tileColor: c.shelf,
      iconColor: c.inkMuted,
      textColor: c.ink,
      titleTextStyle: t.body.copyWith(color: c.ink),
      subtitleTextStyle: t.meta.copyWith(color: c.inkMuted),
      minVerticalPadding: Space.md,
      contentPadding: const EdgeInsets.symmetric(horizontal: Space.lg),
    ),
    tooltipTheme: TooltipThemeData(
      decoration: BoxDecoration(
        color: c.solid,
        borderRadius: const BorderRadius.all(Radii.chip),
      ),
      textStyle: t.caption.copyWith(color: c.onSolid),
    ),
  );
}
