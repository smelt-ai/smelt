import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_localizations/flutter_localizations.dart';

import 'data/preferences.dart';
import 'data/stores.dart';
import 'features/home_shell.dart';
import 'l10n/gen/l10n.dart';
import 'theme/theme.dart';

class SmeltApp extends StatelessWidget {
  const SmeltApp({super.key, required this.model, this.demoScreen});

  final AppModel model;

  /// 演示模式直达的那一屏，见 `data/demo/demo_screens.dart`。
  final String? demoScreen;

  @override
  Widget build(BuildContext context) {
    return AppScope(
      model: model,
      child: ValueListenableBuilder<Preferences>(
        valueListenable: model.prefs,
        builder: (context, prefs, _) => MaterialApp(
          onGenerateTitle: (context) => L10n.of(context).appName,
          debugShowCheckedModeBanner: false,
          theme: buildGalleyTheme(Brightness.light),
          darkTheme: buildGalleyTheme(Brightness.dark),
          themeMode: prefs.themeMode,
          // 切明暗不做插值：一次到位，不让整屏颜色慢慢滑过去。
          themeAnimationStyle: AnimationStyle.noAnimation,
          localizationsDelegates: const [
            L10n.delegate,
            GlobalMaterialLocalizations.delegate,
            GlobalWidgetsLocalizations.delegate,
            GlobalCupertinoLocalizations.delegate,
          ],
          supportedLocales: L10n.supportedLocales,
          // 状态栏图标颜色的兜底：系统在屏幕顶部正中取样，平板分栏时取样点上可能没有
          // 导航栏声明样式，就会落回 Android 主题默认的白色图标（浅色界面上看不见）。
          builder: (context, child) {
            final dark = Theme.of(context).brightness == Brightness.dark;
            return AnnotatedRegion<SystemUiOverlayStyle>(
              value:
                  (dark
                          ? SystemUiOverlayStyle.light
                          : SystemUiOverlayStyle.dark)
                      .copyWith(
                        statusBarColor: Colors.transparent,
                        systemNavigationBarColor: Colors.transparent,
                      ),
              child: child!,
            );
          },
          home: HomeShell(demoScreen: demoScreen),
        ),
      ),
    );
  }
}
