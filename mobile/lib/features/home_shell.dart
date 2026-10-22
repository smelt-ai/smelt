import 'dart:async';

import 'package:flutter/material.dart';
import 'terminal/terminal_page.dart';
import 'pass/triage_page.dart';
import 'machines/machine_detail_page.dart';
import 'conversation/conversation_page.dart';
import 'package:flutter/foundation.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../data/models.dart';
import '../data/stores.dart';
import '../l10n/gen/l10n.dart';
import '../theme/galley.dart';
import '../ui/adaptive.dart';
import '../ui/page.dart';
import '../data/demo/demo_screens.dart';
import 'agents/agents_page.dart';
import 'pass/pass_page.dart';
import 'session/open_session.dart';
import 'sessions/sessions_page.dart';
import 'settings/settings_page.dart';

enum HomeTab { pass, sessions, agents, settings }

/// 外壳：手机底部 tab；平板左侧导航栏 + 列表栏 + 详情栏。
class HomeShell extends StatefulWidget {
  const HomeShell({super.key, this.demoScreen});

  /// 只在演示模式下有值：启动后直接打开这一屏，供模拟器截图审查（模拟器没法点）。
  final String? demoScreen;

  @override
  State<HomeShell> createState() => _HomeShellState();
}

class _HomeShellState extends State<HomeShell> implements DemoShell {
  HomeTab _tab = HomeTab.pass;
  final _sessionFilter = ValueNotifier<String?>(null);
  final _detailNavigator = GlobalKey<NavigatorState>();
  final _bodyKey = GlobalKey();

  /// 平板详情栏的默认内容跟着当前 tab 走。
  final _tabNotifier = ValueNotifier<HomeTab>(HomeTab.pass);

  /// 切 tab：平板上详情栏回到这个 tab 的默认内容（上一个 tab 打开的详情不留着）。
  void _selectTab(HomeTab tab) {
    if (tab == _tab) return;
    final split = isSplitLayout(context);
    setState(() {
      _tab = tab;
      _tabNotifier.value = tab;
      if (split) _detailKey = null;
    });
    if (!split) return;
    _pinnedDefault.value = null;
    final navigator = _detailNavigator.currentState;
    if (_defaultAtBottom) {
      navigator?.popUntil((route) => route.isFirst);
    } else {
      // 打开详情时整个栈都换掉了，默认内容已经不在栈底：重新放一页。
      _defaultAtBottom = true;
      navigator?.pushAndRemoveUntil(
        PageRouteBuilder<void>(
          pageBuilder: (context, _, _) => _defaultDetail(),
          transitionDuration: Duration.zero,
        ),
        (_) => false,
      );
    }
  }

  String? _detailKey;

  /// 平板详情栏的栈底还是默认内容（没被打开的详情整个换掉）。
  bool _defaultAtBottom = true;

  /// 平板会话 tab 的默认详情显示着「最近动过的会话」时，用户在列表里点了它：钉住这条，
  /// 不再跟着「最近」换。页面原样留着，不重建。
  final _pinnedDefault = ValueNotifier<(String, String)?>(null);
  StreamSubscription<(String, AttentionItem)>? _attention;

  @override
  void initState() {
    super.initState();
    final screen = widget.demoScreen;
    if (screen != null) {
      // 等首帧和演示数据都到了再跳。
      Future<void>.delayed(const Duration(milliseconds: 600), () async {
        if (!mounted) return;
        openDemoScreen(this, screen);
        // 等跳转后的页面画完（转场、首批数据），再告诉截图脚本可以截了：负载高的
        // 模拟器上固定等几秒会截到启动页。
        await Future<void>.delayed(const Duration(milliseconds: 1200));
        await WidgetsBinding.instance.endOfFrame;
        debugPrint('Smelt 演示就绪：$screen');
      });
    }
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _attention ??= AppScope.of(context).catalog.attention.listen(_onAttention);
  }

  /// 外壳里面（DetailHost 之下）的 context：从外面打开要用详情去处的弹层时用它。
  @override
  BuildContext get innerContext => _bodyKey.currentContext ?? context;

  @override
  void selectTab(HomeTab tab) => _selectTab(tab);

  /// 演示跳转要用的详情去处（和页面里拿到的是同一个）。
  @override
  void openDetail(String key, WidgetBuilder builder) =>
      _open(key, builder, split: isSplitLayout(context));

  @override
  void dispose() {
    _attention?.cancel();
    _sessionFilter.dispose();
    _tabNotifier.dispose();
    _pinnedDefault.dispose();
    super.dispose();
  }

  /// 不在指挥台、也没开着那条会话时，新的提醒弹一条可点的提示。
  void _onAttention((String, AttentionItem) event) {
    final (runtimeId, item) = event;
    if (!mounted) return;
    final key = sessionKey(runtimeId, item.sessionId);
    if (_tab == HomeTab.pass || _detailKey == key) return;
    final model = AppScope.of(context);
    final l10n = L10n.of(context);
    final session = model.catalog.session(runtimeId, item.sessionId);
    final machine = model.fleet.runtime(runtimeId)?.name;
    final messenger = ScaffoldMessenger.of(context);
    messenger
      ..clearSnackBars()
      ..showSnackBar(
        SnackBar(
          content: Text(
            [?machine, item.title, item.message].join(' · '),
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
          ),
          action: session == null
              ? null
              : SnackBarAction(
                  label: l10n.commonOpen,
                  onPressed: () => openSession(context, runtimeId, session),
                ),
        ),
      );
  }

  void _showSessions(String? runtimeId) {
    _sessionFilter.value = runtimeId;
    _selectTab(HomeTab.sessions);
  }

  void _open(String key, WidgetBuilder builder, {required bool split}) {
    if (split) {
      if (key == _detailKey) {
        // 已经在详情栏里：不重建，详情里往下钻过就退回它。
        _detailNavigator.currentState?.popUntil((route) => route.isFirst);
        return;
      }
      if (_detailKey == null && _defaultAtBottom && _tab == HomeTab.sessions) {
        final latest = _DefaultDetail._latestSession(AppScope.of(context));
        if (latest != null && sessionKey(latest.$1, latest.$2.id) == key) {
          // 默认详情显示的就是它：钉住，算选中。
          _pinnedDefault.value = (latest.$1, latest.$2.id);
          setState(() => _detailKey = key);
          _detailNavigator.currentState?.popUntil((route) => route.isFirst);
          return;
        }
      }
      _defaultAtBottom = false;
      setState(() => _detailKey = key);
      _detailNavigator.currentState?.pushAndRemoveUntil(
        PageRouteBuilder<void>(
          pageBuilder: (context, _, _) => builder(context),
          transitionDuration: Duration.zero,
        ),
        (_) => false,
      );
      return;
    }
    setState(() => _detailKey = key);
    // 压进根导航的页面不在外壳的 DetailHost 下面，给它也包一层，里面再打开详情照样能用。
    Navigator.of(context)
        .push(
          MaterialPageRoute<void>(
            builder: (routeContext) => DetailHost(
              open: (key, builder) => _open(key, builder, split: false),
              selectedKey: null,
              child: Builder(builder: builder),
            ),
          ),
        )
        .whenComplete(() {
          if (mounted && _detailKey == key) setState(() => _detailKey = null);
        });
  }

  /// 平板详情栏的默认内容。
  Widget _defaultDetail() => _DefaultDetail(
    tab: _tabNotifier,
    pinned: _pinnedDefault,
    openSession: (runtimeId, session) => openSessionWith(
      DetailHost(
        open: (key, builder) => _open(key, builder, split: true),
        selectedKey: null,
        child: const SizedBox.shrink(),
      ),
      runtimeId,
      session,
    ),
  );

  Widget _page(HomeTab tab) => switch (tab) {
    HomeTab.pass => PassPage(onShowAllSessions: _showSessions),
    HomeTab.sessions => SessionsPage(filter: _sessionFilter),
    HomeTab.agents => const AgentsPage(),
    HomeTab.settings => const SettingsPage(),
  };

  @override
  Widget build(BuildContext context) {
    final split = isSplitLayout(context);
    final l10n = L10n.of(context);
    final model = AppScope.of(context);
    final c = GalleyColors.of(context);

    final destinations = [
      (HomeTab.pass, LucideIcons.inbox, l10n.navPass),
      (HomeTab.sessions, LucideIcons.messagesSquare, l10n.navSessions),
      (HomeTab.agents, LucideIcons.bot, l10n.navAgents),
      (HomeTab.settings, LucideIcons.settings2, l10n.navSettings),
    ];

    Widget icon(HomeTab tab, IconData data) {
      if (tab != HomeTab.pass) return Icon(data);
      return ValueListenableBuilder(
        valueListenable: model.pass,
        builder: (context, pass, _) => Badge(
          isLabelVisible: pass.tickets.isNotEmpty,
          backgroundColor: c.heat,
          textColor: c.onHeat,
          label: Text('${pass.tickets.length}'),
          child: Icon(data),
        ),
      );
    }

    final body = IndexedStack(
      key: _bodyKey,
      index: _tab.index,
      children: [
        for (final tab in HomeTab.values)
          HeroMode(enabled: tab == _tab, child: _page(tab)),
      ],
    );

    final host = DetailHost(
      open: (key, builder) => _open(key, builder, split: split),
      selectedKey: split ? _detailKey : null,
      child: split
          ? Row(
              children: [
                NavigationRail(
                  selectedIndex: _tab.index,
                  onDestinationSelected: (i) => _selectTab(HomeTab.values[i]),
                  destinations: [
                    for (final (tab, data, label) in destinations)
                      NavigationRailDestination(
                        icon: icon(tab, data),
                        label: Text(label),
                      ),
                  ],
                ),
                SizedBox(width: listPaneWidth(context), child: body),
                Expanded(
                  child: DetailPane(
                    navigatorKey: _detailNavigator,
                    placeholder: _defaultDetail(),
                  ),
                ),
              ],
            )
          : body,
    );

    return Scaffold(
      body: host,
      bottomNavigationBar: split
          ? null
          : DecoratedBox(
              decoration: BoxDecoration(
                border: Border(top: BorderSide(color: c.hairline)),
              ),
              child: NavigationBar(
                selectedIndex: _tab.index,
                onDestinationSelected: (i) => _selectTab(HomeTab.values[i]),
                destinations: [
                  for (final (tab, data, label) in destinations)
                    NavigationDestination(icon: icon(tab, data), label: label),
                ],
              ),
            ),
    );
  }
}

/// 平板详情栏没选东西时显示什么：不留一大块空白。
///
/// - 指挥台：有单据就嵌入逐张处理，没有就说没有。
/// - 会话：最近动过的那条会话；用户在列表里点了它就钉住（见 [pinned]）。
/// - 设置：第一台机器的详情。
/// - 智能体：一句该选什么。
class _DefaultDetail extends StatelessWidget {
  const _DefaultDetail({
    required this.tab,
    required this.pinned,
    required this.openSession,
  });

  final ValueListenable<HomeTab> tab;

  /// 钉住的会话（机器 id，会话 id）。为空时跟着「最近动过」走。
  final ValueListenable<(String, String)?> pinned;
  final void Function(String runtimeId, SessionSummary session) openSession;

  @override
  Widget build(BuildContext context) {
    final model = AppScope.of(context);
    final l10n = L10n.of(context);
    return ValueListenableBuilder<HomeTab>(
      valueListenable: tab,
      builder: (context, tab, _) => switch (tab) {
        HomeTab.pass => ValueListenableBuilder<Pass>(
          valueListenable: model.pass,
          builder: (context, pass, _) => pass.tickets.isEmpty
              ? _note(l10n.passRailEmpty, LucideIcons.check)
              : TriagePage(
                  key: const ValueKey('embedded-triage'),
                  initialIndex: 0,
                  embedded: true,
                  onOpenSession: openSession,
                ),
        ),
        HomeTab.sessions => ListenableBuilder(
          listenable: Listenable.merge([model.catalog, pinned]),
          builder: (context, _) {
            final latest = _pinnedSession(model) ?? _latestSession(model);
            if (latest == null) {
              return _note(l10n.navSessions, LucideIcons.messagesSquare);
            }
            final (runtimeId, session) = latest;
            return KeyedSubtree(
              key: ValueKey('latest/$runtimeId/${session.id}'),
              child: session.isTerminal
                  ? TerminalPage(runtimeId: runtimeId, sessionId: session.id)
                  : ConversationPage(
                      runtimeId: runtimeId,
                      sessionId: session.id,
                    ),
            );
          },
        ),
        HomeTab.settings => ValueListenableBuilder<FleetView>(
          valueListenable: model.fleet,
          builder: (context, fleet, _) => fleet.runtimes.isEmpty
              ? _note(l10n.navSettings, LucideIcons.settings2)
              : MachineDetailPage(
                  key: ValueKey(fleet.runtimes.first.record.runtimeId),
                  runtimeId: fleet.runtimes.first.record.runtimeId,
                ),
        ),
        HomeTab.agents => _note(l10n.shellPickAgentHint, LucideIcons.bot),
      },
    );
  }

  /// 钉住的那条会话。机器掉线了列表成了缓存也照样显示：页面自己会说没连上。
  (String, SessionSummary)? _pinnedSession(AppModel model) {
    final key = pinned.value;
    if (key == null) return null;
    final session = model.catalog.session(key.$1, key.$2);
    return session == null ? null : (key.$1, session);
  }

  /// 所有在线机器里最近动过的会话。只看实时列表：缓存里的会话不自动打开。
  static (String, SessionSummary)? _latestSession(AppModel model) {
    (String, SessionSummary)? best;
    for (final runtime in model.fleet.value.runtimes) {
      final list = model.catalog.list(runtime.record.runtimeId);
      if (list == null || list.cached) continue;
      for (final s in list.sessions) {
        if (best == null || s.updatedAt > best.$2.updatedAt) {
          best = (runtime.record.runtimeId, s);
        }
      }
    }
    return best;
  }

  Widget _note(String title, IconData icon) => Scaffold(
    body: Center(
      child: EmptyNote(icon: icon, title: title),
    ),
  );
}
