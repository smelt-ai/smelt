import 'package:flutter/material.dart';

import '../theme/galley.dart';

/// 宽屏断点：窗口宽度够放下「导航栏 + 列表 + 详情」时用平板布局。按窗口宽度判断，
/// 不按设备型号——iPad 分屏时窗口可能只有手机宽。700 让 iPad 竖屏（744–834）也分栏：
/// 平板上拉长的手机布局是错的，详情栏窄一点仍比整屏铺开一条列表好用。
const double kSplitBreakpoint = 700;

/// 列表栏宽度：中等宽度（iPad 竖屏）收窄，给详情栏留够阅读宽度。
double listPaneWidth(BuildContext context) =>
    MediaQuery.sizeOf(context).width >= 1000
    ? Space.listPane
    : Space.listPaneNarrow;

bool isSplitLayout(BuildContext context) =>
    MediaQuery.sizeOf(context).width >= kSplitBreakpoint;

/// 详情去处。手机上压到根导航栈；平板上替换右侧详情栏。
///
/// 页面打开会话、机器详情等都走 [DetailHost.open]，不直接 `Navigator.push`，这样同一份
/// 页面代码在两种布局下都对。
class DetailHost extends InheritedWidget {
  const DetailHost({
    super.key,
    required this.open,
    required this.selectedKey,
    required super.child,
  });

  /// 打开一个详情。[key] 标识它（比如 `runtimeId/sessionId`），列表据此高亮选中行。
  final void Function(String key, WidgetBuilder builder) open;

  /// 平板上当前详情栏里的是谁；手机上恒为 null。
  final String? selectedKey;

  static DetailHost of(BuildContext context) {
    final host = context.dependOnInheritedWidgetOfExactType<DetailHost>();
    assert(host != null, 'DetailHost 缺失：页面要放在 HomeShell 里');
    return host!;
  }

  @override
  bool updateShouldNotify(DetailHost old) => old.selectedKey != selectedKey;
}

/// 平板右侧详情栏：自己的导航栈，详情里再往下钻也留在这一栏。
class DetailPane extends StatelessWidget {
  const DetailPane({
    super.key,
    required this.navigatorKey,
    required this.placeholder,
  });

  final GlobalKey<NavigatorState> navigatorKey;
  final Widget placeholder;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: c.ground,
        border: Border(left: BorderSide(color: c.hairline)),
      ),
      child: Navigator(
        key: navigatorKey,
        onGenerateRoute: (_) =>
            PageRouteBuilder<void>(pageBuilder: (context, _, _) => placeholder),
      ),
    );
  }
}
