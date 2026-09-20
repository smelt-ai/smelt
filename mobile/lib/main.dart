import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:image_picker/image_picker.dart';
import 'models/pairing_config.dart';
import 'models/saved_desktop.dart';
import 'models/session_filters.dart';
import 'widgets/agent_icon.dart';
import 'widgets/project_avatar.dart';
import 'pages/agents_page.dart';
import 'pages/console_page.dart';
import 'pages/qr_scanner_page.dart';
import 'pages/terminal_session_page.dart';
import 'services/gateway_service.dart';
import 'services/message_draft_store.dart';
import 'models/acp_snapshot.dart';
import 'services/pairing_storage.dart';
import 'services/pending_actions_controller.dart';
import 'rust_lib.dart';
import 'src/rust/api_iroh.dart';
import 'utils/image_processing.dart';
import 'pages/settings_page.dart';
import 'services/appearance_prefs_store.dart';
import 'services/terminal_prefs_store.dart';
import 'theme/smelt_theme.dart';
import 'widgets/acp_content.dart';
import 'widgets/approval_card.dart';
import 'widgets/elicitation_card.dart';
import 'widgets/pending_action_badge.dart';
import 'widgets/session_row.dart';

// 现有调用方（含测试）仍从 main.dart 取这些筛选谓词，保持入口不变。
export 'models/session_filters.dart';
export 'widgets/pending_action_badge.dart';

bool isNearMessageBottom(
  double pixels,
  double minScrollExtent, {
  double tolerance = 48,
}) => (pixels - minScrollExtent).abs() <= tolerance;

bool shouldAutoFollowSnapshot({
  required bool initialLoad,
  required bool wasAtBottom,
}) => initialLoad || wasAtBottom;

bool shouldShowAttentionNotification({
  required LifecycleAttention attention,
  required String? activeSessionId,
  required String? subscribedSessionId,
}) {
  if (activeSessionId == attention.sessionId) return false;
  if (subscribedSessionId == attention.sessionId && !attention.requiresAction) {
    return false;
  }
  return true;
}

Future<String?> showDesktopRenameDialog(
  BuildContext context,
  String initialName,
) {
  var editedName = initialName;
  return showDialog<String>(
    context: context,
    builder: (dialogContext) => AlertDialog(
      title: const Text('Rename desktop'),
      content: TextFormField(
        initialValue: initialName,
        autofocus: true,
        textInputAction: TextInputAction.done,
        onChanged: (value) => editedName = value,
        onFieldSubmitted: (value) => Navigator.pop(dialogContext, value),
        decoration: const InputDecoration(
          labelText: 'Name',
          border: OutlineInputBorder(),
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(dialogContext),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(dialogContext, editedName),
          child: const Text('Save'),
        ),
      ],
    ),
  );
}

/// 底部导航的三个去处。顺序即优先级：先回答「有事等我吗」，再是「有哪些项目」。
enum _HomeTab { console, projects, agents, settings }

/// 指挥台图标上的待办角标。徽标从 AppBar 挪到 tab 上——底部导航常驻可见，
/// 比顶栏更适合承载「还有几件事等我」。
class _ConsoleTabIcon extends StatelessWidget {
  const _ConsoleTabIcon({required this.sessions});

  final List<SessionSummary> sessions;

  @override
  Widget build(BuildContext context) {
    final count = sessions.where(sessionNeedsAction).length;
    const icon = Icon(Icons.inbox_outlined);
    if (count == 0) return icon;
    return Badge.count(count: count, child: icon);
  }
}

class _ProjectActionsButton extends StatelessWidget {
  const _ProjectActionsButton({
    required this.canCreate,
    required this.hasAgents,
    required this.onNewSession,
    required this.onHistory,
  });

  final bool canCreate;
  final bool hasAgents;
  final VoidCallback onNewSession;
  final VoidCallback onHistory;

  @override
  Widget build(BuildContext context) {
    return PopupMenuButton<String>(
      tooltip: 'Project actions',
      icon: const Icon(Icons.more_vert),
      onSelected: (action) => switch (action) {
        'session' => onNewSession(),
        'history' => onHistory(),
        _ => null,
      },
      itemBuilder: (context) => [
        // 对话和终端不再是两个入口：具体开哪一种是选择器里那一行的属性，跟桌面
        // 「+」弹层一样。两个入口的时候，用户得先猜「Claude 终端」算哪一类。
        PopupMenuItem(
          value: 'session',
          enabled: canCreate,
          child: const ListTile(
            contentPadding: EdgeInsets.zero,
            leading: Icon(Icons.add),
            title: Text('New session'),
          ),
        ),
        PopupMenuItem(
          value: 'history',
          enabled: hasAgents,
          child: const ListTile(
            contentPadding: EdgeInsets.zero,
            leading: Icon(Icons.history),
            title: Text('Conversation history'),
          ),
        ),
      ],
    );
  }
}

class TurnElapsedLabel extends StatefulWidget {
  const TurnElapsedLabel({
    super.key,
    required this.label,
    required this.startedAtMs,
    required this.color,
  });

  final String label;
  final int startedAtMs;
  final Color color;

  @override
  State<TurnElapsedLabel> createState() => _TurnElapsedLabelState();
}

class _TurnElapsedLabelState extends State<TurnElapsedLabel> {
  late final Timer _timer;

  @override
  void initState() {
    super.initState();
    _timer = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted) setState(() {});
    });
  }

  @override
  Widget build(BuildContext context) {
    final elapsed = DateTime.now().millisecondsSinceEpoch - widget.startedAtMs;
    return Text(
      '${widget.label} · ${_formatElapsed(elapsed)}',
      maxLines: 2,
      overflow: TextOverflow.ellipsis,
      style: TextStyle(color: widget.color, fontSize: 12),
    );
  }

  @override
  void dispose() {
    _timer.cancel();
    super.dispose();
  }
}

/// 连接横幅。
///
/// 正常联通时**什么都不画**：`LAN · 4 ms` 这类链路遥测对用户是噪音，它的去处是
/// 设置页的连接卡——设计稿全篇只有 F 出现过这个 chip。异常或在用缓存兜底时仍然
/// 要画，那时它带的是「Offline · Showing saved data」和一个重试按钮，是真信息。
///
/// 三个屏幕（指挥台 / 项目 / 会话）共用这一个入口，翻页时不会忽隐忽现；`cached`
/// 由各屏自己判断，因为「在用缓存」问的是**这一屏**的数据，不是全局。
Widget buildConnectionBanner({
  required WsState state,
  required bool cached,
  DateTime? cachedAt,
  VoidCallback? onRetry,
}) {
  if (state == WsState.connected && !cached) return const SizedBox.shrink();
  return CachedConnectionBar(state: state, cachedAt: cachedAt, onRetry: onRetry);
}

class CachedConnectionBar extends StatelessWidget {
  const CachedConnectionBar({
    super.key,
    required this.state,
    this.cachedAt,
    this.onRetry,
  });

  final WsState state;
  final DateTime? cachedAt;

  /// 断线时的重试入口。放在这条状态栏上，而不是浮动按钮里：用户是在这里读到
  /// 「Offline」的，动作就该长在同一处。
  final VoidCallback? onRetry;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final label = switch (state) {
      WsState.reconnecting => 'Reconnecting',
      WsState.connecting => 'Connecting',
      WsState.connected => 'Refreshing',
      WsState.disconnected => 'Offline',
    };
    final age = cachedAt == null ? null : _formatCacheAge(cachedAt!);
    final offline = state == WsState.disconnected;
    return Container(
      width: double.infinity,
      constraints: const BoxConstraints(minHeight: 36),
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
      color: colors.tertiaryContainer,
      child: Row(
        children: [
          // 断线时不能转菊花：那会一直暗示「正在恢复」，而实际上没有任何重连在跑。
          if (offline)
            Icon(
              Icons.cloud_off_outlined,
              size: 15,
              color: colors.onTertiaryContainer,
            )
          else
            SizedBox.square(
              dimension: 14,
              child: CircularProgressIndicator(
                strokeWidth: 2,
                color: colors.onTertiaryContainer,
              ),
            ),
          const SizedBox(width: 9),
          Expanded(
            child: Text(
              age == null
                  ? '$label · Showing saved data'
                  : '$label · Saved $age',
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(color: colors.onTertiaryContainer, fontSize: 12),
            ),
          ),
          if (onRetry case final retry?) ...[
            const SizedBox(width: 8),
            TextButton(
              onPressed: retry,
              style: TextButton.styleFrom(
                foregroundColor: colors.onTertiaryContainer,
                visualDensity: VisualDensity.compact,
                padding: const EdgeInsets.symmetric(horizontal: 10),
                minimumSize: const Size(0, 32),
              ),
              child: const Text('Retry'),
            ),
          ],
        ],
      ),
    );
  }
}

String _formatCacheAge(DateTime cachedAt) {
  final age = DateTime.now().difference(cachedAt);
  if (age.inSeconds < 60) return 'just now';
  if (age.inMinutes < 60) return '${age.inMinutes}m ago';
  if (age.inHours < 24) return '${age.inHours}h ago';
  return '${age.inDays}d ago';
}

bool _isInterruptMarker(String text) {
  final value = text.trim();
  return value.startsWith('[Request interrupted by user') &&
      value.endsWith(']');
}

String _formatElapsed(int milliseconds) {
  final seconds = milliseconds < 0 ? 0 : milliseconds ~/ 1000;
  if (seconds < 60) return '${seconds}s';
  return '${seconds ~/ 60}m ${seconds % 60}s';
}

String _imageMimeFromName(String name) {
  final lower = name.toLowerCase();
  if (lower.endsWith('.jpg') || lower.endsWith('.jpeg')) return 'image/jpeg';
  if (lower.endsWith('.webp')) return 'image/webp';
  if (lower.endsWith('.gif')) return 'image/gif';
  if (lower.endsWith('.heic')) return 'image/heic';
  return 'image/png';
}

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // 图标名单来自资产清单，读一次缓存住；没读完之前所有 agent 都是兜底图标。
  await loadAgentIcons();
  await initRustLib();
  // 在组装根接线，而不是让 GatewayService 直接依赖 FFI：服务层保持纯 Dart，
  // 单测才能不带动态库地跑。
  gatewayService.irohTunnelOpener = (endpointId, relayUrl) =>
      irohTunnelStart(endpointId: endpointId, relayUrl: relayUrl);
  gatewayService.irohTunnelStopper = irohTunnelStop;
  gatewayService.irohPathProbe = () async {
    final status = await irohTunnelPathStatus();
    if (status == null) return null;
    final kind = switch (status.kind) {
      'lan' => ConnectionPathKind.lan,
      'p2p' => ConnectionPathKind.p2p,
      'relay' => ConnectionPathKind.relay,
      _ => ConnectionPathKind.unknown,
    };
    return IrohPathSample(kind: kind, rttMs: status.rttMs);
  };
  runApp(const SmeltApp());
}

class SmeltApp extends StatefulWidget {
  const SmeltApp({
    super.key,
    this.pairingStorage,
    this.messageDraftStore,
    this.appearancePrefsStore,
  });

  final PairingStorage? pairingStorage;
  final MessageDraftStore? messageDraftStore;
  final AppearancePrefsStore? appearancePrefsStore;

  @override
  State<SmeltApp> createState() => _SmeltAppState();
}

/// 主题模式必须住在 `MaterialApp` **之上**——它决定整棵树用哪套配色，放在 home
/// 里改不动自己头顶的那个 MaterialApp。
class _SmeltAppState extends State<SmeltApp> {
  late final AppearancePrefsStore _appearanceStore;
  AppearancePrefs _appearance = const AppearancePrefs();

  @override
  void initState() {
    super.initState();
    _appearanceStore = widget.appearancePrefsStore ?? FileAppearancePrefsStore();
    _loadAppearance();
  }

  Future<void> _loadAppearance() async {
    final prefs = await _appearanceStore.load();
    if (!mounted) return;
    setState(() => _appearance = prefs);
  }

  void _setThemeMode(ThemeMode mode) {
    if (mode == _appearance.themeMode) return;
    // 先改界面再落盘：主题切换要立刻可见，存不存得下去是另一回事。
    setState(() => _appearance = _appearance.copyWith(themeMode: mode));
    _appearanceStore.save(_appearance);
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Smelt',
      theme: smeltTheme(Brightness.light),
      darkTheme: smeltTheme(Brightness.dark),
      themeMode: _appearance.themeMode,
      home: HomePage(
        pairingStorage: widget.pairingStorage,
        messageDraftStore: widget.messageDraftStore,
        themeMode: _appearance.themeMode,
        onThemeModeChanged: _setThemeMode,
      ),
    );
  }
}

class HomePage extends StatefulWidget {
  const HomePage({
    super.key,
    this.pairingStorage,
    this.messageDraftStore,
    this.themeMode = ThemeMode.system,
    this.onThemeModeChanged,
    this.terminalPrefsStore,
  });

  final ThemeMode themeMode;
  final ValueChanged<ThemeMode>? onThemeModeChanged;
  final TerminalPrefsStore? terminalPrefsStore;

  final PairingStorage? pairingStorage;
  final MessageDraftStore? messageDraftStore;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> with WidgetsBindingObserver {
  WsState _connectionState = WsState.disconnected;
  List<SessionSummary> _sessions = [];
  WorkspaceCatalog _workspace = const WorkspaceCatalog(
    projects: [],
    agents: [],
  );
  late final StreamSubscription<WsState> _stateSubscription;
  late final StreamSubscription<List<SessionSummary>> _sessionsSubscription;
  late final StreamSubscription<WorkspaceCatalog> _workspaceSubscription;
  late final StreamSubscription<String> _sessionCreatedSubscription;
  late final StreamSubscription<LifecycleAttention> _attentionSubscription;
  late final StreamSubscription<String> _attentionResolvedSubscription;
  late final StreamSubscription<String> _errorSubscription;
  late final PairingStorage _pairingStorage;
  late final MessageDraftStore _messageDraftStore;
  String? _shownAttentionSessionId;
  PairingConfig? _pendingPairing;
  bool _restoringPairing = true;
  SavedDesktopCollection _savedDesktops = const SavedDesktopCollection();
  bool _showPairingCode = false;
  /// 首屏落在指挥台，不是项目树。`docs/product-roadmap.md §6`：手机是指挥台。
  _HomeTab _tab = _HomeTab.console;

  /// 指挥台的数据源建在 HomePage 上而不是页内，切 tab 时不重建，回到指挥台
  /// 不用重新取一遍详情。
  final PendingActionsController _pendingActions = PendingActionsController();

  /// 终端字号在设置页也能调，所以状态提到这里；终端页每次都是新 push，会在
  /// initState 从同一个文件读到最新值。
  late final TerminalPrefsStore _terminalPrefsStore;
  TerminalPrefs _terminalPrefs = const TerminalPrefs();
  bool _acceptConnectionNotifications = true;
  String? _pendingOpenSessionId;
  String? _activeSessionId;

  final _pairingCodeController = TextEditingController();

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _pairingStorage = widget.pairingStorage ?? SecurePairingStorage();
    _messageDraftStore = widget.messageDraftStore ?? FileMessageDraftStore();
    _terminalPrefsStore = widget.terminalPrefsStore ?? FileTerminalPrefsStore();
    _terminalPrefsStore.load().then((prefs) {
      if (!mounted) return;
      setState(() => _terminalPrefs = prefs);
    });
    _stateSubscription = gatewayService.stateStream.listen((state) {
      if (!mounted) return;
      setState(() => _connectionState = state);
      if (state == WsState.connected && _pendingPairing != null) {
        unawaited(_savePendingPairing());
      }
    });
    _sessionsSubscription = gatewayService.sessionsStream.listen((sessions) {
      if (!mounted) return;
      setState(() => _sessions = sessions);
      final pendingId = _pendingOpenSessionId;
      final created = pendingId == null
          ? null
          : sessions.where((session) => session.id == pendingId).firstOrNull;
      if (created != null) {
        _pendingOpenSessionId = null;
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (mounted) _openSession(created);
        });
      }
    });
    _workspaceSubscription = gatewayService.workspaceStream.listen((workspace) {
      if (!mounted) return;
      setState(() => _workspace = workspace);
    });
    _sessionCreatedSubscription = gatewayService.sessionCreatedStream.listen((
      id,
    ) {
      if (!mounted) return;
      _pendingOpenSessionId = id;
    });
    _attentionSubscription = gatewayService.attentionStream.listen((item) {
      if (!mounted || !_acceptConnectionNotifications) return;
      gatewayService.listSessions();
      if (_activeSessionId == item.sessionId) {
        gatewayService.markRead(item.sessionId);
      }
      final isCurrent = gatewayService.subscribedSessionId == item.sessionId;
      if (!shouldShowAttentionNotification(
        attention: item,
        activeSessionId: _activeSessionId,
        subscribedSessionId: gatewayService.subscribedSessionId,
      )) {
        return;
      }
      final session = _sessions
          .where((candidate) => candidate.id == item.sessionId)
          .firstOrNull;
      final messenger = ScaffoldMessenger.of(context);
      messenger.clearSnackBars();
      _shownAttentionSessionId = item.sessionId;
      messenger
          .showSnackBar(
            SnackBar(
              content: Text('${item.title}: ${item.message}'),
              action: session != null && !isCurrent
                  ? SnackBarAction(
                      label: 'Open',
                      onPressed: () => _openSession(session),
                    )
                  : null,
            ),
          )
          .closed
          .then((_) {
            if (_shownAttentionSessionId == item.sessionId) {
              _shownAttentionSessionId = null;
            }
          });
    });
    _attentionResolvedSubscription = gatewayService.attentionResolvedStream
        .listen((sessionId) {
          gatewayService.listSessions();
          if (!mounted || _shownAttentionSessionId != sessionId) return;
          _shownAttentionSessionId = null;
          ScaffoldMessenger.of(context).hideCurrentSnackBar();
        });
    _errorSubscription = gatewayService.errorStream.listen((error) {
      if (!mounted || !_acceptConnectionNotifications) return;
      final messenger = ScaffoldMessenger.of(context);
      messenger.clearSnackBars();
      messenger.showSnackBar(SnackBar(content: Text(error)));
    });
    unawaited(_restorePairing());
  }

  /// 回到前台立刻确认连接还活着。系统在后台会冻结心跳定时器，这段时间里
  /// 网络往往已经换过，而半开的 TCP 不会通知我们——不主动探一下，用户看到的
  /// 就是进后台那一刻的旧会话。
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    super.didChangeAppLifecycleState(state);
    if (state == AppLifecycleState.resumed) {
      gatewayService.verifyConnection();
    }
  }

  Future<void> _restorePairing() async {
    try {
      final savedDesktops = await _pairingStorage.load();
      if (!mounted) return;
      setState(() {
        _restoringPairing = false;
        _savedDesktops = savedDesktops;
      });
      final active = savedDesktops.activeDesktop;
      if (active != null) {
        _connect(active.pairing, saveWhenConnected: false);
      }
    } catch (error) {
      if (!mounted) return;
      setState(() => _restoringPairing = false);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not restore pairing: $error')),
      );
    }
  }

  Future<void> _savePendingPairing() async {
    final pairing = _pendingPairing;
    if (pairing == null) return;
    // 只在真正连上的就是这组配对时才落盘，避免自动重连到旧桌面时把新扫的
    // endpoint/token 写进去。
    if (!gatewayService.matchesTarget(pairing.endpoint, pairing.token)) return;
    _pendingPairing = null;
    try {
      final savedDesktops = await _pairingStorage.save(pairing);
      if (mounted) setState(() => _savedDesktops = savedDesktops);
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Connected, but pairing was not saved: $error')),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    // 连不上、也没有缓存会话时，整屏只谈「怎么连上」——这时三个 tab 都没有内容，
    // 显示导航栏只会给出可点却没反应的假选项。
    final navigable = !_restoringPairing && !_showsConnectionTakeover;
    return Scaffold(
      appBar: AppBar(
        title: Text(
          navigable
              ? switch (_tab) {
                  _HomeTab.console => 'Console',
                  _HomeTab.projects => 'Projects',
                  _HomeTab.agents => 'Agents',
                  _HomeTab.settings => 'Settings',
                }
              : 'Smelt',
        ),
        actions: [
          // 指挥台本身就是待办的落点，在那一页再挂徽标是重复的。
          if (navigable && _tab != _HomeTab.console)
            PendingActionBadge(onPressed: _showPendingActions),
        ],
      ),
      body: SafeArea(top: false, child: _buildBody()),
      bottomNavigationBar: navigable
          ? NavigationBar(
              selectedIndex: _tab.index,
              onDestinationSelected: (index) =>
                  setState(() => _tab = _HomeTab.values[index]),
              destinations: [
                NavigationDestination(
                  icon: _ConsoleTabIcon(sessions: _sessions),
                  label: 'Console',
                ),
                const NavigationDestination(
                  icon: Icon(Icons.folder_outlined),
                  selectedIcon: Icon(Icons.folder),
                  label: 'Projects',
                ),
                const NavigationDestination(
                  icon: Icon(Icons.smart_toy_outlined),
                  selectedIcon: Icon(Icons.smart_toy),
                  label: 'Agents',
                ),
                const NavigationDestination(
                  icon: Icon(Icons.settings_outlined),
                  selectedIcon: Icon(Icons.settings),
                  label: 'Settings',
                ),
              ],
            )
          : null,
    );
  }

  /// 还没有任何会话可谈的连接状态。此时不分 tab，整屏只讲连接。
  bool get _showsConnectionTakeover =>
      _sessions.isEmpty && _connectionState != WsState.connected;

  /// 待办徽标的落点：切到指挥台。会话页里点它会先 pop 回来，两边落到同一处，
  /// 用户不用记「刚才是从哪进来的」。
  void _showPendingActions() {
    setState(() => _tab = _HomeTab.console);
  }

  Widget _buildBody() {
    if (_restoringPairing) {
      return const Center(child: CircularProgressIndicator());
    }
    // 跟导航栏用同一个判断，两者不会各说各话。
    if (_showsConnectionTakeover) {
      return switch (_connectionState) {
        WsState.disconnected => _buildDisconnectedView(),
        WsState.connecting => _buildConnectingView(),
        WsState.reconnecting => _buildReconnectingView(),
        WsState.connected => _buildSessionList(),
      };
    }
    return switch (_tab) {
      _HomeTab.console => Column(
        children: [
          _buildConnectionBar(),
          Expanded(
            child: ConsolePage(
              onOpenSession: _openSession,
              controller: _pendingActions,
            ),
          ),
        ],
      ),
      _HomeTab.projects => _buildSessionList(),
      _HomeTab.agents => AgentsPage(
        onOpenSession: _openSessionById,
        startableAgentIds: _startableAgentIds,
        onStartConversation: _startAgentConversation,
      ),
      _HomeTab.settings => _buildSettings(),
    };
  }

  /// 设置页。内容在 `pages/settings_page.dart`，这里只把 home 手上的状态和动作
  /// 递进去。见设计稿 F。
  Widget _buildSettings() {
    return SettingsPage(
      desktops: _savedDesktops,
      connectionState: _connectionState,
      connectionBar: _buildConnectionBar(),
      themeMode: widget.themeMode,
      onThemeModeChanged: widget.onThemeModeChanged ?? (_) {},
      terminalFontSize: _terminalPrefs.fontSize,
      onTerminalFontSizeChanged: _setTerminalFontSize,
      onSwitchDesktop: _showDesktopSwitcher,
      onPair: _scanQrCode,
      onDisconnect: _disconnect,
    );
  }

  void _setTerminalFontSize(double size) {
    if (size == _terminalPrefs.fontSize) return;
    setState(() => _terminalPrefs = _terminalPrefs.copyWith(fontSize: size));
    _terminalPrefsStore.save(_terminalPrefs);
  }

  Widget _buildConnectionBar() {
    return buildConnectionBanner(
      state: _connectionState,
      cached: gatewayService.sessionsAreCached,
      cachedAt: gatewayService.cachedAt,
      onRetry: _connectionState == WsState.disconnected
          ? gatewayService.retryCurrentConnection
          : null,
    );
  }

  Widget _buildDisconnectedView() {
    return SingleChildScrollView(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Icon(
            Icons.link_off,
            size: 64,
            color: Theme.of(context).colorScheme.onSurfaceVariant,
          ),
          const SizedBox(height: 16),
          const Text('Not connected', textAlign: TextAlign.center),
          if (_savedDesktops.activeDesktop case final active?) ...[
            const SizedBox(height: 8),
            Text(
              active.name,
              textAlign: TextAlign.center,
              style: TextStyle(
                color: Theme.of(context).colorScheme.onSurfaceVariant,
              ),
            ),
            const SizedBox(height: 12),
            OutlinedButton.icon(
              onPressed: gatewayService.retryCurrentConnection,
              icon: const Icon(Icons.sync),
              label: const Text('Retry saved desktop'),
            ),
          ],
          const SizedBox(height: 24),

          TextField(
            controller: _pairingCodeController,
            obscureText: !_showPairingCode,
            enableSuggestions: false,
            autocorrect: false,
            textInputAction: TextInputAction.done,
            onSubmitted: (_) => _manualConnect(),
            decoration:
                const InputDecoration(
                  labelText: 'Pairing Code',
                  hintText: 'Paste code from Smelt Desktop',
                  border: OutlineInputBorder(),
                ).copyWith(
                  suffixIcon: IconButton(
                    tooltip: _showPairingCode
                        ? 'Hide pairing code'
                        : 'Show pairing code',
                    onPressed: () =>
                        setState(() => _showPairingCode = !_showPairingCode),
                    icon: Icon(
                      _showPairingCode
                          ? Icons.visibility_off
                          : Icons.visibility,
                    ),
                  ),
                ),
          ),
          const SizedBox(height: 16),
          ElevatedButton.icon(
            onPressed: _manualConnect,
            icon: const Icon(Icons.link),
            label: const Text('Connect'),
          ),

          const SizedBox(height: 24),
          const Divider(),
          const SizedBox(height: 24),

          ElevatedButton.icon(
            onPressed: _scanQrCode,
            icon: const Icon(Icons.qr_code_scanner),
            label: const Text('Scan QR Code to Pair'),
          ),
          if (_savedDesktops.desktops.isNotEmpty) ...[
            const SizedBox(height: 12),
            TextButton.icon(
              onPressed: _showDesktopSwitcher,
              icon: const Icon(Icons.desktop_mac_outlined),
              label: const Text('Manage saved desktops'),
            ),
          ],
        ],
      ),
    );
  }

  Widget _buildSessionList() {
    final orderedSessions = _sessions
        .where(sessionBelongsToProjectTree)
        .toList()
      ..sort(compareSessionMenuOrder);

    return Column(
      children: [
        _buildConnectionBar(),
        Expanded(child: _buildProjectSessionList(orderedSessions)),
      ],
    );
  }

  /// 下拉刷新。手机上「怀疑列表过期了」的第一反应是下拉，而不是去够右下角的
  /// 浮动按钮。等一次 sessions 推送再收起指示器，超时兜底避免离线时一直转。
  Future<void> _refreshSessions() async {
    if (_connectionState == WsState.disconnected) {
      await gatewayService.retryCurrentConnection();
      return;
    }
    final next = gatewayService.sessionsStream.first;
    gatewayService.listSessions();
    gatewayService.listWorkspace();
    try {
      await next.timeout(const Duration(seconds: 5));
    } on TimeoutException {
      // 刷新没回来就直接收起指示器：状态栏已经在表达连接情况了。
    }
  }

  /// 哪些项目是展开的。ExpansionTile 自己管展开状态，但我们要在 trailing 里画
  /// 一个跟它同步的箭头，所以得在外面镜像一份。
  final Set<String> _expandedProjects = <String>{};

  String _projectSubtitle(int total, int needing) {
    final sessions = '$total session${total == 1 ? '' : 's'}';
    if (total == 0) return 'No sessions';
    if (needing == 0) return sessions;
    return '$sessions · $needing needs you';
  }

  Widget _buildProjectSessionList(List<SessionSummary> orderedSessions) {
    final projects =
        <String, ({WorkspaceProject project, List<SessionSummary> sessions})>{};
    for (final project in _workspace.projects) {
      projects[project.root] = (project: project, sessions: []);
    }
    for (final session in orderedSessions) {
      final key = _projectKey(session);
      final group = projects.putIfAbsent(
        key,
        () => (
          project: WorkspaceProject(
            root: key,
            title: _projectName(session),
            order: session.projectOrder,
          ),
          sessions: <SessionSummary>[],
        ),
      );
      group.sessions.add(session);
    }

    if (projects.isEmpty) {
      return _refreshable(
        _buildEmptySessions(
          icon: Icons.folder_off_outlined,
          title: 'No projects',
          message:
              'Open a project in Smelt Desktop, then pull down to refresh.',
        ),
      );
    }
    return RefreshIndicator(
      onRefresh: _refreshSessions,
      // 滑开一行删除按钮时，自动收起别行敞着的那个。
      child: SessionRowGroup(
        child: ListView(
        // 底部留一点余量，最后一个项目分组不要贴着导航栏。
        padding: const EdgeInsets.only(bottom: 16),
        children: projects.entries.map((entry) {
          final project = entry.value.project;
          final sessions = entry.value.sessions;
          final needing = sessions.where(sessionNeedsAction).length;
          return ExpansionTile(
            // ExpansionTile 展开时会把 trailing 的图标染成主色，导致 ⋮ 变蓝、
            // 跟会话行里的灰 ⋯ 不是一套。这里钉死中性色。
            iconColor: Theme.of(context).colorScheme.onSurfaceVariant,
            collapsedIconColor: Theme.of(context).colorScheme.onSurfaceVariant,
            textColor: Theme.of(context).colorScheme.onSurface,
            collapsedTextColor: Theme.of(context).colorScheme.onSurface,
            // 自定义了 leading，内置箭头必须让位——否则它会顶掉项目色块。
            // 但直接不画箭头等于丢掉「这行可以展开」的提示，所以下面自己画一个
            // 受控的，跟 ⋮ 并排。
            controlAffinity: ListTileControlAffinity.trailing,
            key: PageStorageKey('project-${project.root}'),
            initiallyExpanded: _expandedProjects.contains(project.root),
            onExpansionChanged: (expanded) => setState(() {
              if (expanded) {
                _expandedProjects.add(project.root);
              } else {
                _expandedProjects.remove(project.root);
              }
            }),
            // 项目色块 + 状态点：设计稿 C 点名要补的身份识别。原来项目只有一行
            // 文字，几个项目并排时全靠读字分辨。
            leading: ProjectAvatar(
              title: project.title,
              identityKey: project.root.isEmpty ? project.title : project.root,
              status: projectStatusColor(context, sessions),
            ),
            title: Text(
              project.title,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(fontWeight: FontWeight.w600),
            ),
            subtitle: Text(
              _projectSubtitle(sessions.length, needing),
              style: TextStyle(
                color: needing > 0
                    ? context.smeltColors.needsAttention
                    : Theme.of(context).colorScheme.onSurfaceVariant,
              ),
            ),
            // 三个独立 IconButton 要占掉 ~144dp，在 360dp 宽的屏上把项目名挤没了。
            // 收成一个菜单：这些都是低频动作，标题的可读性更值钱。
            trailing: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                AnimatedRotation(
                  turns: _expandedProjects.contains(project.root) ? 0.5 : 0,
                  duration: const Duration(milliseconds: 180),
                  child: Icon(
                    Icons.expand_more,
                    size: 20,
                    color: Theme.of(context).colorScheme.onSurfaceVariant,
                  ),
                ),
                _ProjectActionsButton(
                  canCreate: gatewayService.writeEnabled,
                  hasAgents: _workspace.agents.isNotEmpty,
                  onNewSession: () => _createSession(project),
                  onHistory: () => _openHistory(project),
                ),
              ],
            ),
            children: sessions
                .map(
                  (session) => _buildSessionTile(
                    session,
                    // 左边缩进到项目色块之后，行才明显是挂在这个项目下的。
                    padding: const EdgeInsets.fromLTRB(32, 9, 12, 9),
                  ),
                )
                .toList(),
          );
        }).toList(),
        ),
      ),
    );
  }

  Widget _buildSessionTile(
    SessionSummary session, {
    EdgeInsetsGeometry? padding,
  }) {
    final theme = Theme.of(context);
    return SessionRow(
      session: session,
      // 行已经在项目分组下面了，项目名不用再画一遍。
      showProject: false,
      padding:
          padding ?? const EdgeInsets.symmetric(horizontal: 4, vertical: 9),
      onTap: () => _openSession(session),
      // 只读配对下不给删除入口。手势和无障碍动作都由 SessionRow 自己挂。
      onDelete: gatewayService.writeEnabled
          ? () => _deleteSession(session)
          : null,
      trailing: session.unread
          ? Container(
              width: 8,
              height: 8,
              decoration: BoxDecoration(
                color: theme.colorScheme.primary,
                shape: BoxShape.circle,
              ),
            )
          : null,
    );
  }

  /// 空状态也要能下拉刷新：列表空恰恰是最想手动拉一把的时候，而 Center 本身
  /// 不可滚动，手势会落空。
  Widget _refreshable(Widget child) {
    return RefreshIndicator(
      onRefresh: _refreshSessions,
      child: LayoutBuilder(
        builder: (context, constraints) => SingleChildScrollView(
          physics: const AlwaysScrollableScrollPhysics(),
          child: ConstrainedBox(
            constraints: BoxConstraints(minHeight: constraints.maxHeight),
            child: child,
          ),
        ),
      ),
    );
  }

  /// 空状态带一句「接下来做什么」和可选动作。只画一个灰图标等于把用户扔在
  /// 死胡同里，尤其 Action / Running 两个筛选很容易空。
  Widget _buildEmptySessions({
    required IconData icon,
    required String title,
    String? message,
    ({String label, VoidCallback onPressed})? action,
  }) {
    final colors = Theme.of(context).colorScheme;
    return Center(
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 42, color: colors.onSurfaceVariant),
            const SizedBox(height: 12),
            Text(title, style: TextStyle(color: colors.onSurfaceVariant)),
            if (message != null) ...[
              const SizedBox(height: 6),
              Text(
                message,
                textAlign: TextAlign.center,
                style: TextStyle(color: colors.onSurfaceVariant, fontSize: 12),
              ),
            ],
            if (action case final action?) ...[
              const SizedBox(height: 14),
              FilledButton.tonal(
                onPressed: action.onPressed,
                child: Text(action.label),
              ),
            ],
          ],
        ),
      ),
    );
  }

  /// 新建会话选择器。分组、顺序、Pin 都由电脑那边算好（跟桌面「+」弹层同一份
  /// 目录），这里只把「常用 / 终端 / 对话」平铺出来——手机上多一层归类就多一次
  /// 点击，而「常用」本来就是为了少点几下。
  Future<LaunchAction?> _chooseLaunchAction() {
    const sectionTitles = {
      LaunchSection.common: 'Pinned',
      LaunchSection.terminal: 'Terminals',
      LaunchSection.conversation: 'Conversations',
    };
    return showModalBottomSheet<LaunchAction>(
      context: context,
      showDragHandle: true,
      builder: (context) {
        final theme = Theme.of(context);
        final rows = <Widget>[
          const ListTile(title: Text('New session')),
        ];
        for (final section in LaunchSection.values) {
          final actions = _workspace.actionsIn(section);
          if (actions.isEmpty) continue;
          rows.add(
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
              child: Text(
                sectionTitles[section]!,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                  fontWeight: FontWeight.w600,
                ),
              ),
            ),
          );
          rows.addAll(
            actions.map(
              (action) => ListTile(
                leading: _launchActionIcon(action),
                title: Text(action.label),
                subtitle: Text(action.kindLabel),
                onTap: () => Navigator.pop(context, action),
              ),
            ),
          );
        }
        if (_workspace.launchActions.isEmpty) {
          rows.add(
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 24),
              child: Text(
                'No launch options yet. Update Smelt on your computer, then pull to refresh.',
                style: TextStyle(color: theme.colorScheme.onSurfaceVariant),
              ),
            ),
          );
        }
        return SafeArea(
          child: ConstrainedBox(
            constraints: BoxConstraints(
              maxHeight: MediaQuery.sizeOf(context).height * 0.7,
            ),
            child: ListView(shrinkWrap: true, children: rows),
          ),
        );
      },
    );
  }

  Widget _launchActionIcon(LaunchAction action) {
    if (action.target == LaunchTarget.blankTerminal) {
      return const Icon(Icons.terminal);
    }
    return AgentGlyph(
      agent: action.agent,
      // 认不出是哪家时，终端动作就画终端，对话动作画机器人——别让一条终端动作
      // 看起来像对话。
      fallback: action.isConversation
          ? Icons.smart_toy_outlined
          : Icons.terminal,
    );
  }

  Future<void> _createSession(WorkspaceProject project) async {
    final action = await _chooseLaunchAction();
    if (!mounted || action == null) return;
    gatewayService.createSessionFromLaunch(project.root, action.key);
  }

  /// 工作区目录里真的能开对话的智能体。目录还没到就是空集——那时「开始对话」
  /// 不出现，比出现一个按了会报错的按钮好。
  Set<String> get _startableAgentIds => {
    for (final agent in _workspace.agents) ?agent.agentDefinitionId,
  };

  /// 从「智能体」栏开对话：不绑项目，网关把它落在智能体自己的 space，跟桌面
  /// 点智能体开对话是同一条路径。
  void _startAgentConversation(String agentDefinitionId) {
    final option = _workspace.agents
        .where((agent) => agent.agentDefinitionId == agentDefinitionId)
        .firstOrNull;
    if (option == null) return;
    gatewayService.createSession('', option.id);
  }

  Future<void> _openHistory(WorkspaceProject project) async {
    await Navigator.push<void>(
      context,
      MaterialPageRoute(
        builder: (context) =>
            SessionHistoryPage(project: project, agents: _workspace.agents),
      ),
    );
  }

  Future<void> _deleteSession(SessionSummary session) async {
    final isTerminal = session.kind == SessionKind.terminal;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(isTerminal ? 'Delete terminal?' : 'Delete conversation?'),
        content: Text(
          isTerminal
              ? 'This closes ${sessionListTitle(session)} and ends the shell process.'
              : 'This stops ${sessionListTitle(session)} and removes it from the active list. The agent transcript remains available in History.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (confirmed == true) gatewayService.deleteSession(session.id);
  }

  String _projectName(SessionSummary session) {
    final projectTitle = session.projectTitle?.trim();
    if (projectTitle != null && projectTitle.isNotEmpty) {
      return projectTitle;
    }
    final cwd = session.cwd?.replaceAll(RegExp(r'/+$'), '');
    if (cwd != null && cwd.isNotEmpty) {
      return cwd.split('/').last;
    }
    return session.title.isNotEmpty ? session.title : 'Other';
  }

  String _projectKey(SessionSummary session) {
    final projectRoot = session.projectRoot?.replaceAll(RegExp(r'/+$'), '');
    if (projectRoot != null && projectRoot.isNotEmpty) {
      return projectRoot;
    }
    final cwd = session.cwd?.replaceAll(RegExp(r'/+$'), '');
    if (cwd != null && cwd.isNotEmpty) return cwd;
    return session.title.isNotEmpty ? session.title : session.id;
  }

  Widget _buildConnectingView() {
    return Center(
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          const CircularProgressIndicator(),
          const SizedBox(height: 16),
          const Text('Connecting...', textAlign: TextAlign.center),
          const SizedBox(height: 16),
          TextButton.icon(
            onPressed: _disconnect,
            icon: const Icon(Icons.close),
            label: const Text('Cancel'),
          ),
        ],
      ),
    );
  }

  Widget _buildReconnectingView() {
    return Center(
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          const CircularProgressIndicator(),
          const SizedBox(height: 16),
          const Text("Reconnecting..."),
          const SizedBox(height: 16),
          TextButton.icon(
            onPressed: _disconnect,
            icon: const Icon(Icons.link_off),
            label: const Text("Change pairing"),
          ),
        ],
      ),
    );
  }

  Future<void> _scanQrCode() async {
    final pairing = await Navigator.push<PairingConfig>(
      context,
      MaterialPageRoute(builder: (context) => const QrScannerPage()),
    );
    if (!mounted || pairing == null) return;
    _connect(pairing);
  }

  void _manualConnect() {
    try {
      final pairing = PairingConfig.parse(_pairingCodeController.text);
      _connect(pairing);
    } on FormatException catch (error) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(error.message.toString())));
    }
  }

  void _connect(PairingConfig pairing, {bool saveWhenConnected = true}) {
    _acceptConnectionNotifications = true;
    ScaffoldMessenger.of(context).clearSnackBars();
    _pendingPairing = saveWhenConnected ? pairing : null;
    gatewayService.connect(pairing.endpoint, pairing.token);
    // 重扫同一台桌面时 connect() 是幂等空操作，不会再有 connected 状态变化来
    // 触发落盘，这里补一次。
    if (saveWhenConnected && gatewayService.state == WsState.connected) {
      unawaited(_savePendingPairing());
    }
  }

  void _disconnect() {
    _acceptConnectionNotifications = false;
    _pendingPairing = null;
    _shownAttentionSessionId = null;
    ScaffoldMessenger.of(context).clearSnackBars();
    gatewayService.disconnect();
    setState(() => _sessions = const []);
  }

  Future<void> _showDesktopSwitcher() async {
    final selected = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (sheetContext) {
        final activeId = _savedDesktops.activeDesktopId;
        return SafeArea(
          child: ConstrainedBox(
            constraints: BoxConstraints(
              maxHeight: MediaQuery.sizeOf(sheetContext).height * 0.72,
            ),
            child: ListView(
              shrinkWrap: true,
              children: [
                const ListTile(
                  title: Text('Desktops'),
                  subtitle: Text('Switch or manage paired computers'),
                ),
                ..._savedDesktops.desktops.map(
                  (desktop) => ListTile(
                    leading: Icon(
                      desktop.id == activeId
                          ? Icons.desktop_windows
                          : Icons.desktop_windows_outlined,
                    ),
                    title: Text(desktop.name),
                    subtitle: Text(
                      desktop.pairing.isIroh ? 'Remote via iroh' : 'Local',
                    ),
                    selected: desktop.id == activeId,
                    onTap: () => Navigator.pop(sheetContext, desktop.id),
                    trailing: PopupMenuButton<String>(
                      tooltip: 'Desktop actions',
                      onSelected: (action) {
                        Navigator.pop(sheetContext);
                        if (action == 'rename') {
                          unawaited(_renameDesktop(desktop));
                        } else if (action == 'remove') {
                          unawaited(_removeDesktop(desktop));
                        }
                      },
                      itemBuilder: (context) => const [
                        PopupMenuItem(
                          value: 'rename',
                          child: ListTile(
                            contentPadding: EdgeInsets.zero,
                            leading: Icon(Icons.edit_outlined),
                            title: Text('Rename'),
                          ),
                        ),
                        PopupMenuItem(
                          value: 'remove',
                          child: ListTile(
                            contentPadding: EdgeInsets.zero,
                            leading: Icon(Icons.delete_outline),
                            title: Text('Forget'),
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
                const Divider(height: 1),
                ListTile(
                  leading: const Icon(Icons.qr_code_scanner),
                  title: const Text('Pair another desktop'),
                  onTap: () {
                    Navigator.pop(sheetContext);
                    unawaited(_scanQrCode());
                  },
                ),
              ],
            ),
          ),
        );
      },
    );
    if (!mounted || selected == null) return;
    await _switchDesktop(selected);
  }

  Future<void> _switchDesktop(String desktopId) async {
    try {
      final savedDesktops = await _pairingStorage.setActive(desktopId);
      if (!mounted) return;
      final active = savedDesktops.activeDesktop;
      setState(() {
        _savedDesktops = savedDesktops;
        _pendingPairing = null;
        _pairingCodeController.clear();
      });
      if (active != null) {
        _connect(active.pairing, saveWhenConnected: false);
      }
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not switch desktop: $error')),
      );
    }
  }

  Future<void> _renameDesktop(SavedDesktop desktop) async {
    final name = await showDesktopRenameDialog(context, desktop.name);
    if (!mounted || name == null || name.trim().isEmpty) return;
    try {
      final savedDesktops = await _pairingStorage.rename(desktop.id, name);
      if (mounted) setState(() => _savedDesktops = savedDesktops);
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not rename desktop: $error')),
      );
    }
  }

  Future<void> _removeDesktop(SavedDesktop desktop) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Forget desktop?'),
        content: Text(
          '${desktop.name} will be removed from this phone. You can pair it again later.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Forget'),
          ),
        ],
      ),
    );
    if (!mounted || confirmed != true) return;

    try {
      final wasActive = _savedDesktops.activeDesktopId == desktop.id;
      final savedDesktops = await _pairingStorage.remove(desktop.id);
      if (!mounted) return;
      setState(() {
        _savedDesktops = savedDesktops;
        _pendingPairing = null;
        _pairingCodeController.clear();
      });
      if (!wasActive) return;
      final next = savedDesktops.activeDesktop;
      if (next == null) {
        _disconnect();
      } else {
        _connect(next.pairing, saveWhenConnected: false);
      }
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not forget desktop: $error')),
      );
    }
  }

  /// 按 id 打开会话。自动化目录里只有 Run 的 session id，没有整条摘要——
  /// 会话还没投影过来（比如刚触发）时什么都不做，不去伪造一条摘要，
  /// 否则会话页会拿着空标题和空 cwd 打开一个「幽灵会话」。
  void _openSessionById(String sessionId) {
    final session = _sessions
        .where((candidate) => candidate.id == sessionId)
        .firstOrNull;
    if (session == null) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('That run is not available yet')),
      );
      return;
    }
    _openSession(session);
  }

  void _openSession(SessionSummary session) {
    gatewayService.markRead(session.id);
    if (_shownAttentionSessionId == session.id) {
      _shownAttentionSessionId = null;
      ScaffoldMessenger.of(context).hideCurrentSnackBar();
    }
    final previousActiveSessionId = _activeSessionId;
    _activeSessionId = session.id;
    unawaited(
      Navigator.push(
        context,
        MaterialPageRoute(
          builder: (pageContext) {
            // 会话页里点待办徽标：先退回列表，再切到 Action。两个入口落到同一处，
            // 用户不用记「刚才是从哪进来的」。
            void showPendingActions() {
              Navigator.pop(pageContext);
              _showPendingActions();
            }

            return session.kind == SessionKind.terminal
                ? TerminalSessionPage(
                    session: session,
                    // 跟设置页共用同一个 store 实例：同一条写队列，两个入口改
                    // 字号不会交错落盘。
                    prefsStore: _terminalPrefsStore,
                    onShowPendingActions: showPendingActions,
                  )
                : SessionPage(
                    session: session,
                    messageDraftStore: _messageDraftStore,
                    onShowPendingActions: showPendingActions,
                  );
          },
        ),
      ).whenComplete(() {
        if (mounted && _activeSessionId == session.id) {
          _activeSessionId = previousActiveSessionId;
        }
        // 终端页的 Aa 菜单也能改字号，回来时把设置页的滑杆同步过来。
        if (session.kind == SessionKind.terminal) {
          _terminalPrefsStore.load().then((prefs) {
            if (!mounted) return;
            setState(() => _terminalPrefs = prefs);
          });
        }
      }),
    );
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _pendingActions.dispose();
    _stateSubscription.cancel();
    _sessionsSubscription.cancel();
    _workspaceSubscription.cancel();
    _sessionCreatedSubscription.cancel();
    _attentionSubscription.cancel();
    _attentionResolvedSubscription.cancel();
    _errorSubscription.cancel();
    _pairingCodeController.dispose();
    super.dispose();
  }
}

class SessionHistoryPage extends StatefulWidget {
  const SessionHistoryPage({
    super.key,
    required this.project,
    required this.agents,
  });

  final WorkspaceProject project;
  final List<AcpAgentOption> agents;

  @override
  State<SessionHistoryPage> createState() => _SessionHistoryPageState();
}

class _SessionHistoryPageState extends State<SessionHistoryPage> {
  late AcpAgentOption _agent;
  List<HistorySessionSummary> _sessions = const [];
  bool _loading = true;
  late final StreamSubscription<SessionHistoryResult> _historySubscription;
  late final StreamSubscription<SessionHistoryRenameResult> _renameSubscription;

  @override
  void initState() {
    super.initState();
    _agent = widget.agents.first;
    _historySubscription = gatewayService.sessionHistoryStream.listen((result) {
      if (!mounted ||
          result.projectRoot != widget.project.root ||
          result.agentOptionId != _agent.id) {
        return;
      }
      setState(() {
        _sessions = result.sessions;
        _loading = false;
      });
    });
    // 改名的结果由 PC 算好回传（恢复默认时要拿回 agent 原始标题），这里就地替换，
    // 不用为了一个名字重扫一遍历史目录。
    _renameSubscription = gatewayService.sessionHistoryRenameStream.listen((
      result,
    ) {
      if (!mounted || result.agentOptionId != _agent.id) return;
      setState(() {
        _sessions = [
          for (final session in _sessions)
            session.resumeId == result.resumeId
                ? session.withCustomTitle(result.customTitle)
                : session,
        ];
      });
    });
    _load();
  }

  void _load() {
    setState(() {
      _loading = true;
      _sessions = const [];
    });
    gatewayService.listSessionHistory(widget.project.root, _agent.id);
  }

  /// 跟 PC 历史页同一套：改过名的会话，副标题前面补一段 agent 原始标题，好让人
  /// 知道这条历史本来叫什么。
  String _historySubtitle(HistorySessionSummary session) {
    final active = session.lastActiveAt?.toLocal();
    final date = active == null
        ? null
        : '${active.month.toString().padLeft(2, '0')}-${active.day.toString().padLeft(2, '0')} '
              '${active.hour.toString().padLeft(2, '0')}:${active.minute.toString().padLeft(2, '0')}';
    final messages =
        '${session.messageCount} message${session.messageCount == 1 ? '' : 's'}';
    final when = date == null ? messages : '$date · $messages';
    return session.hasCustomTitle && session.title.trim().isNotEmpty
        ? '${session.title} · $when'
        : when;
  }

  Future<void> _rename(HistorySessionSummary session) async {
    final controller = TextEditingController(text: session.displayTitle);
    final title = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Rename conversation'),
        content: TextField(
          controller: controller,
          autofocus: true,
          decoration: const InputDecoration(
            labelText: 'Name',
            border: OutlineInputBorder(),
          ),
          onSubmitted: (value) => Navigator.pop(context, value),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Cancel'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(context, controller.text),
            child: const Text('Save'),
          ),
        ],
      ),
    );
    controller.dispose();
    if (title == null) return;
    final trimmed = title.trim();
    // 清空 = 恢复默认名称，跟 PC 的「恢复默认名称」落到同一条路径。
    gatewayService.renameSessionHistory(
      widget.project.root,
      _agent.id,
      session.resumeId,
      title: trimmed.isEmpty ? null : trimmed,
    );
  }

  void _resetName(HistorySessionSummary session) {
    gatewayService.renameSessionHistory(
      widget.project.root,
      _agent.id,
      session.resumeId,
    );
  }

  void _resume(HistorySessionSummary session) {
    gatewayService.createSession(
      widget.project.root,
      _agent.id,
      resumeId: session.resumeId,
    );
    Navigator.pop(context);
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text('${widget.project.title} history')),
      body: Column(
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 12),
            child: DropdownButtonFormField<String>(
              initialValue: _agent.id,
              decoration: const InputDecoration(
                labelText: 'Agent',
                border: OutlineInputBorder(),
              ),
              items: widget.agents
                  .map(
                    (agent) => DropdownMenuItem(
                      value: agent.id,
                      child: Text(agent.label),
                    ),
                  )
                  .toList(),
              onChanged: (id) {
                if (id == null) return;
                _agent = widget.agents.firstWhere((agent) => agent.id == id);
                _load();
              },
            ),
          ),
          Expanded(
            child: _loading
                ? const Center(child: CircularProgressIndicator())
                : _sessions.isEmpty
                ? const Center(child: Text('No resumable conversations'))
                : RefreshIndicator(
                    onRefresh: () async => _load(),
                    child: ListView.separated(
                      itemCount: _sessions.length,
                      separatorBuilder: (_, _) => const Divider(height: 1),
                      itemBuilder: (context, index) {
                        final session = _sessions[index];
                        return ListTile(
                          leading: const Icon(Icons.history),
                          title: Text(
                            session.displayTitle,
                            maxLines: 2,
                            overflow: TextOverflow.ellipsis,
                          ),
                          subtitle: Text(
                            _historySubtitle(session),
                            maxLines: 2,
                            overflow: TextOverflow.ellipsis,
                          ),
                          trailing: Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              IconButton(
                                tooltip: 'Resume conversation',
                                icon: const Icon(Icons.play_arrow),
                                onPressed: gatewayService.writeEnabled
                                    ? () => _resume(session)
                                    : null,
                              ),
                              PopupMenuButton<String>(
                                tooltip: 'More',
                                enabled: gatewayService.writeEnabled,
                                onSelected: (action) => switch (action) {
                                  'rename' => _rename(session),
                                  'reset' => _resetName(session),
                                  _ => null,
                                },
                                itemBuilder: (context) => [
                                  const PopupMenuItem(
                                    value: 'rename',
                                    child: Text('Rename'),
                                  ),
                                  if (session.hasCustomTitle)
                                    const PopupMenuItem(
                                      value: 'reset',
                                      child: Text('Reset name'),
                                    ),
                                ],
                              ),
                            ],
                          ),
                          onTap: gatewayService.writeEnabled
                              ? () => _resume(session)
                              : null,
                        );
                      },
                    ),
                  ),
          ),
        ],
      ),
    );
  }

  @override
  void dispose() {
    _historySubscription.cancel();
    _renameSubscription.cancel();
    super.dispose();
  }
}

class SessionPage extends StatefulWidget {
  final SessionSummary session;
  final MessageDraftStore messageDraftStore;

  /// 点全局待办徽标时调用。由调用方决定「回到待办」意味着什么，页面自己不
  /// 假设自己是被谁 push 出来的。
  final VoidCallback? onShowPendingActions;

  const SessionPage({
    super.key,
    required this.session,
    required this.messageDraftStore,
    this.onShowPendingActions,
  });

  @override
  State<SessionPage> createState() => _SessionPageState();
}

class _SessionPageState extends State<SessionPage> {
  final TextEditingController _messageController = TextEditingController();
  final FocusNode _messageFocusNode = FocusNode();
  final ScrollController _scrollController = ScrollController();
  final ImagePicker _imagePicker = ImagePicker();
  AcpSnapshot? _snapshot;
  bool _loading = true;
  bool _isAtBottom = true;
  bool _loadingOlder = false;
  bool _isMessageFocused = false;
  bool _restoringDraft = true;
  bool _sendingMessage = false;
  WsState _connectionState = gatewayService.state;
  String? _sendRequestId;
  String? _sendError;
  String? _permissionSubmittingToolId;
  final List<AcpImageData> _pendingImages = [];
  final Map<int, String> _elicitationTextValues = {};
  late final StreamSubscription<AcpSnapshot> _snapshotSubscription;
  late final StreamSubscription<String> _attentionResolvedSubscription;
  late final StreamSubscription<MessageSendResult> _messageSendSubscription;
  late final StreamSubscription<WsState> _connectionStateSubscription;
  Timer? _draftSaveTimer;

  @override
  void initState() {
    super.initState();
    _snapshot = gatewayService.cachedSnapshot(widget.session.id);
    _loading = _snapshot == null;
    _syncSnapshotControls();
    _messageController.addListener(_handleMessageChanged);
    _messageFocusNode.addListener(_handleMessageFocus);
    _scrollController.addListener(_handleScrollPosition);
    _attentionResolvedSubscription = gatewayService.attentionResolvedStream
        .listen((sessionId) {
          if (!mounted || sessionId != widget.session.id) return;
          // 重新挂载 watcher 获取完整权威快照，避免本地根据 phase 猜哪张卡已解决。
          gatewayService.subscribe(widget.session.id);
        });
    _messageSendSubscription = gatewayService.messageSendStream.listen(
      (result) => unawaited(_handleMessageSendResult(result)),
    );
    _connectionStateSubscription = gatewayService.stateStream.listen((state) {
      if (!mounted) return;
      setState(() => _connectionState = state);
    });
    _subscribeSession();
    unawaited(_restoreDraft());
  }

  Future<void> _restoreDraft() async {
    var draft = await widget.messageDraftStore.load(widget.session.id);
    final unconfirmedRequestId = draft?.requestId;
    var recoveredUnconfirmed = false;
    if (unconfirmedRequestId != null) {
      final recovered = await widget.messageDraftStore.resolveRequest(
        widget.session.id,
        unconfirmedRequestId,
        succeeded: false,
      );
      if (!recovered) {
        draft = await widget.messageDraftStore.load(widget.session.id);
      } else {
        recoveredUnconfirmed = true;
        draft = draft!.copyWith(clearRequestId: true);
      }
    }
    if (!mounted) return;
    _restoringDraft = true;
    if (draft != null) {
      _messageController.text = draft.content;
      _pendingImages
        ..clear()
        ..addAll(draft.images);
      if (recoveredUnconfirmed) {
        _sendError = 'Delivery was not confirmed. Review and retry.';
      }
    }
    _restoringDraft = false;
    if (mounted) setState(() {});
  }

  void _handleMessageChanged() {
    if (!mounted || _restoringDraft) return;
    setState(() => _sendError = null);
    _scheduleDraftSave();
  }

  void _scheduleDraftSave() {
    if (_restoringDraft || _sendingMessage) return;
    _draftSaveTimer?.cancel();
    _draftSaveTimer = Timer(
      const Duration(milliseconds: 300),
      () => unawaited(_saveDraft()),
    );
  }

  Future<void> _saveDraft({String? requestId}) {
    return widget.messageDraftStore.save(
      widget.session.id,
      MessageDraft(
        content: _messageController.text,
        images: List<AcpImageData>.of(_pendingImages),
        requestId: requestId,
      ),
    );
  }

  Future<void> _handleMessageSendResult(MessageSendResult result) async {
    if (result.requestId != _sendRequestId) return;
    final resolved = await widget.messageDraftStore.resolveRequest(
      widget.session.id,
      result.requestId,
      succeeded: result.ok,
    );
    if (!mounted) {
      await _messageSendSubscription.cancel();
      return;
    }
    if (!resolved) {
      setState(() {
        _sendingMessage = false;
        _sendRequestId = null;
      });
      return;
    }
    _restoringDraft = true;
    setState(() {
      _sendingMessage = false;
      _sendRequestId = null;
      if (result.ok) {
        _messageController.clear();
        _pendingImages.clear();
        _sendError = null;
      } else {
        _sendError = result.error ?? 'Message could not be sent';
      }
    });
    _restoringDraft = false;
  }

  void _handleMessageFocus() {
    if (!mounted || _isMessageFocused == _messageFocusNode.hasFocus) return;
    setState(() => _isMessageFocused = _messageFocusNode.hasFocus);
  }

  void _dismissKeyboard() => _messageFocusNode.unfocus();

  void _handleScrollPosition() {
    if (!_scrollController.hasClients) return;
    final position = _scrollController.position;
    final isAtBottom = isNearMessageBottom(
      position.pixels,
      position.minScrollExtent,
    );
    if (!mounted) return;
    if (isAtBottom != _isAtBottom) {
      setState(() => _isAtBottom = isAtBottom);
    }
    _maybeLoadOlder();
  }

  void _maybeLoadOlder() {
    if (!mounted || !_scrollController.hasClients || _loadingOlder) return;
    final position = _scrollController.position;
    if (position.maxScrollExtent - position.pixels > 240) return;
    if (gatewayService.loadOlder(widget.session.id)) {
      setState(() => _loadingOlder = true);
    }
  }

  void _syncSnapshotControls() {
    final activePermission = _snapshot?.pendingPermissions.firstOrNull;
    if (activePermission?.toolCallId != _permissionSubmittingToolId) {
      _permissionSubmittingToolId = null;
    }
    final elicitation = _snapshot?.pendingElicitation;
    if (elicitation == null) {
      _elicitationTextValues.clear();
    } else {
      for (final entry in elicitation.textValues.entries) {
        _elicitationTextValues.putIfAbsent(entry.key, () => entry.value);
      }
    }
  }

  void _subscribeSession() {
    _snapshotSubscription = gatewayService.snapshotStream.listen((snapshot) {
      if (!mounted || gatewayService.subscribedSessionId != widget.session.id) {
        return;
      }
      final initialLoad = _snapshot == null;
      final previousOffset = _snapshot?.entriesOffset;
      final shouldFollowLatest = shouldAutoFollowSnapshot(
        initialLoad: initialLoad,
        wasAtBottom: _isAtBottom,
      );
      setState(() {
        _snapshot = snapshot;
        _syncSnapshotControls();
        if (previousOffset == null ||
            snapshot.entriesOffset < previousOffset ||
            !snapshot.hasMoreBefore) {
          _loadingOlder = false;
        }
        _loading = false;
      });
      if (shouldFollowLatest) {
        _scrollToBottom(animate: !initialLoad);
      }
    });
    gatewayService.subscribe(widget.session.id);
  }

  void _scrollToBottom({bool animate = true}) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_scrollController.hasClients) {
        final bottom = _scrollController.position.minScrollExtent;
        if (animate) {
          _scrollController.animateTo(
            bottom,
            duration: const Duration(milliseconds: 300),
            curve: Curves.easeOut,
          );
        } else {
          _scrollController.jumpTo(bottom);
        }
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(
          widget.session.title.isNotEmpty
              ? widget.session.title
              : widget.session.id,
        ),
        actions: [
          if (widget.onShowPendingActions case final show?)
            PendingActionBadge(onPressed: show),
          if (_snapshot != null)
            Padding(
              padding: const EdgeInsets.only(right: 16),
              child: Center(child: _buildPhaseIndicator()),
            ),
        ],
      ),
      body: SafeArea(
        top: false,
        child: Column(
          children: [
            buildConnectionBanner(
              state: _connectionState,
              cached: gatewayService.snapshotIsCached(widget.session.id),
              cachedAt: gatewayService.cachedAt,
              onRetry: _connectionState == WsState.disconnected
                  ? gatewayService.retryCurrentConnection
                  : null,
            ),
            if (_snapshot case final snapshot?) _buildSessionStatus(snapshot),
            if (_snapshot?.plan case final plan?) _buildPlanPanel(plan),
            if (_snapshot?.pendingPermissions
                case final List<PendingPermission> permissions
                when permissions.isNotEmpty)
              _buildPermissionBanner(permissions.first, permissions.length),
            if (_snapshot?.pendingElicitation case final elicitation?)
              _buildElicitationCard(elicitation),

            Expanded(
              child: _loading
                  ? const Center(child: CircularProgressIndicator())
                  : Stack(
                      children: [
                        Positioned.fill(child: _buildEntryList()),
                        if (!_isAtBottom)
                          Positioned(
                            left: 0,
                            right: 0,
                            bottom: 12,
                            child: Center(
                              child: FilledButton.tonalIcon(
                                key: const ValueKey('scroll-to-bottom'),
                                onPressed: _scrollToBottom,
                                icon: const Icon(
                                  Icons.arrow_downward,
                                  size: 18,
                                ),
                                label: const Text('Jump to latest'),
                              ),
                            ),
                          ),
                      ],
                    ),
            ),
            _buildInputBar(),
          ],
        ),
      ),
    );
  }

  Widget _buildPhaseIndicator() {
    final phase = _snapshot!.phase;
    // 图标 + 颜色是这里唯一的信息载体，读屏和色盲都拿不到。补一句文字标签。
    final label = switch (phase) {
      AcpPhaseIdle() => 'Idle',
      AcpPhaseStarting() => 'Starting',
      AcpPhaseRunning() => 'Running',
      AcpPhaseAwaitingApproval() => 'Waiting for your approval',
      AcpPhaseAwaitingChoice() => 'Waiting for your choice',
      AcpPhaseEnded(reason: final r) => 'Ended: $r',
    };
    return Semantics(label: label, child: _phaseIcon(phase));
  }

  Widget _phaseIcon(AcpPhase phase) {
    final status = context.smeltColors;
    return switch (phase) {
      AcpPhaseIdle() => Icon(Icons.pause_circle, color: status.idle),
      AcpPhaseStarting() => const SizedBox(
        width: 20,
        height: 20,
        child: CircularProgressIndicator(strokeWidth: 2),
      ),
      AcpPhaseRunning() => const SizedBox(
        width: 20,
        height: 20,
        child: CircularProgressIndicator(strokeWidth: 2),
      ),
      // 等审批用红：跟列表「要你」同一色。这里原本是橙色，
      // 同一个会话在列表里是红、进去以后变成橙。
      AcpPhaseAwaitingApproval() => Icon(
        Icons.warning_amber,
        color: status.waitingApproval,
      ),
      AcpPhaseAwaitingChoice() => Icon(
        Icons.help_outline,
        color: status.needsAttention,
      ),
      // Ended 只在 Fatal / RestoreFailed 时出现——正常结束一轮走的是 Idle
      // （见 acp_session.rs `finish_turn`）。所以它确实是错误终态，用红。
      AcpPhaseEnded(reason: final r) => Tooltip(
        message: r,
        child: Icon(Icons.stop_circle, color: status.danger),
      ),
    };
  }

  Widget _buildSessionStatus(AcpSnapshot snapshot) {
    final colors = Theme.of(context).colorScheme;
    // 运行色用 running token 而不是 primary：指挥台的运行点是蓝的，这里再用蓝紫，
    // 同一个会话换个页面就换个颜色——跟当初 chip 红、指示器橙那处漂移是一回事。
    final running = context.smeltColors.running;
    final phase = snapshot.phase;
    if (phase is AcpPhaseIdle ||
        phase is AcpPhaseAwaitingApproval ||
        phase is AcpPhaseAwaitingChoice) {
      return const SizedBox.shrink();
    }
    final (icon, label, color) = switch (phase) {
      AcpPhaseStarting() => (
        Icons.rocket_launch_outlined,
        snapshot.statusLine ?? 'Starting agent...',
        running,
      ),
      AcpPhaseRunning() => (
        Icons.auto_awesome,
        snapshot.statusLine ?? 'Agent is working',
        running,
      ),
      AcpPhaseEnded(reason: final reason) => (
        Icons.error_outline,
        reason.isEmpty ? 'Session ended' : reason,
        colors.error,
      ),
      _ => (Icons.info_outline, '', colors.onSurfaceVariant),
    };
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      // 设计稿是 12%；原来的 7% 在深色底上几乎看不出这是一条独立的带子。
      color: color.withAlpha(31),
      child: Row(
        children: [
          if (phase is AcpPhaseRunning || phase is AcpPhaseStarting)
            SizedBox(
              width: 16,
              height: 16,
              child: CircularProgressIndicator(strokeWidth: 2, color: color),
            )
          else
            Icon(icon, size: 18, color: color),
          const SizedBox(width: 8),
          Expanded(
            child: phase is AcpPhaseRunning && snapshot.turnStartedAtMs != null
                ? TurnElapsedLabel(
                    label: label,
                    startedAtMs: snapshot.turnStartedAtMs!,
                    color: color,
                  )
                : Text(
                    label,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(color: color, fontSize: 12),
                  ),
          ),
          if (phase is AcpPhaseRunning && gatewayService.writeEnabled)
            IconButton(
              visualDensity: VisualDensity.compact,
              tooltip: 'Stop current turn',
              onPressed: () => gatewayService.cancelTurn(widget.session.id),
              icon: const Icon(Icons.stop_circle_outlined),
            ),
        ],
      ),
    );
  }

  Widget _buildPlanPanel(AcpPlan plan) {
    if (plan.steps.isEmpty) return const SizedBox.shrink();
    final completed = plan.steps.where((step) => step.isCompleted).length;
    return ExpansionTile(
      dense: true,
      initiallyExpanded: plan.steps.any((step) => step.isInProgress),
      leading: const Icon(Icons.checklist, size: 19),
      title: Text('Plan · $completed/${plan.steps.length}'),
      shape: const Border(bottom: BorderSide(color: Colors.transparent)),
      collapsedShape: const Border(
        bottom: BorderSide(color: Colors.transparent),
      ),
      children: plan.steps.map((step) {
        final (icon, color) = step.isCompleted
            ? (Icons.check_circle, context.smeltColors.done)
            : step.isInProgress
            ? (
                Icons.radio_button_checked,
                Theme.of(context).colorScheme.primary,
              )
            : (
                Icons.radio_button_unchecked,
                Theme.of(context).colorScheme.onSurfaceVariant,
              );
        return ListTile(
          dense: true,
          visualDensity: VisualDensity.compact,
          leading: Icon(icon, size: 17, color: color),
          title: Text(
            step.title,
            style: TextStyle(
              fontSize: 13,
              decoration: step.isCompleted ? TextDecoration.lineThrough : null,
            ),
          ),
        );
      }).toList(),
    );
  }

  Widget _buildPermissionBanner(
    PendingPermission permission,
    int pendingCount,
  ) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
      child: ApprovalCard(
        permission: permission,
        submitting: _permissionSubmittingToolId == permission.toolCallId,
        onRespond: (optionId) =>
            _respondApproval(permission.toolCallId, optionId),
        header: Row(
          children: [
            const Icon(Icons.gpp_maybe_outlined, size: 18),
            const SizedBox(width: 8),
            const Expanded(
              child: Text(
                'Permission required',
                style: TextStyle(fontWeight: FontWeight.bold),
              ),
            ),
            if (pendingCount > 1)
              Chip(
                visualDensity: VisualDensity.compact,
                label: Text('$pendingCount pending'),
              ),
          ],
        ),
      ),
    );
  }

  Widget _buildElicitationCard(PendingElicitation elicitation) {
    return ElicitationCard(
      elicitation: elicitation,
      textValues: _elicitationTextValues,
      onTextChanged: (index, value) => _elicitationTextValues[index] = value,
      onChoose: (fieldIndex, optionIndex) => gatewayService.chooseElicitation(
        widget.session.id,
        fieldIndex,
        optionIndex,
      ),
      onSubmit: _submitElicitation,
      onDismiss: () => gatewayService.dismissElicitation(widget.session.id),
    );
  }

  void _submitElicitation() {
    for (final entry in _elicitationTextValues.entries) {
      gatewayService.updateElicitationText(
        widget.session.id,
        entry.key,
        entry.value,
      );
    }
    gatewayService.submitElicitation(widget.session.id);
  }

  Widget _buildEntryList() {
    final entries = _snapshot?.entries ?? [];
    if (entries.isEmpty) {
      return const Center(child: Text('No messages yet'));
    }

    return ListView.builder(
      controller: _scrollController,
      reverse: true,
      keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
      itemCount: entries.length + (_snapshot!.hasMoreBefore ? 1 : 0),
      itemBuilder: (context, index) {
        if (index == entries.length) {
          return Padding(
            padding: const EdgeInsets.symmetric(vertical: 16),
            child: Center(
              child: _loadingOlder
                  ? const SizedBox(
                      width: 20,
                      height: 20,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : TextButton.icon(
                      onPressed: _maybeLoadOlder,
                      icon: const Icon(Icons.history, size: 18),
                      label: const Text('Load earlier messages'),
                    ),
            ),
          );
        }
        final entryIndex = entries.length - 1 - index;
        return _buildEntry(entryIndex, entries[entryIndex]);
      },
    );
  }

  Widget _buildEntry(int index, AcpEntry entry) {
    return switch (entry) {
      AcpEntryUser(text: final text) =>
        _isInterruptMarker(text)
            ? _buildDivider('Interrupted')
            : _buildUserMessage(text),
      AcpEntryUserWithImages(text: final text, images: final images) =>
        _buildUserMessage(text, images: images),
      AcpEntryAssistant(text: final text, thought: final thought) =>
        AcpAssistantMessage(
          text: text,
          thought: thought,
          isFinal: !thought && _isFinalAnswer(index),
          durationMs: !thought && _isFinalAnswer(index)
              ? _snapshot?.lastTurnDurationMs
              : null,
        ),
      AcpEntryToolCall(
        title: final title,
        status: final status,
        output: final output,
      )
          when isTaskCompletionToolTitle(title) =>
        AcpCompletionMessage(
          output: output,
          status: status,
          isFinal: _isFinalAnswer(index),
        ),
      AcpEntryToolCall(
        id: _,
        title: final title,
        kind: final kind,
        status: final status,
        output: final output,
      ) =>
        AcpToolCallCard(
          title: title,
          kind: kind,
          status: status,
          output: output,
        ),
      AcpEntryDivider(label: final label) => _buildDivider(label),
      AcpEntryUnknown() => const SizedBox.shrink(),
    };
  }

  bool _isFinalAnswer(int index) {
    if (_snapshot?.phase is! AcpPhaseIdle &&
        _snapshot?.phase is! AcpPhaseEnded) {
      return false;
    }
    final entries = _snapshot?.entries ?? const <AcpEntry>[];
    for (var candidate = entries.length - 1; candidate >= 0; candidate--) {
      if (entries[candidate] case AcpEntryAssistant(
        thought: false,
        text: final text,
      ) when text.trim().isNotEmpty) {
        return index == candidate;
      }
      if (entries[candidate] case AcpEntryToolCall(
        title: final title,
      ) when isTaskCompletionToolTitle(title)) {
        return index == candidate;
      }
    }
    return false;
  }

  Widget _buildUserMessage(
    String text, {
    List<AcpImageData> images = const [],
  }) {
    return Align(
      alignment: Alignment.centerRight,
      child: Container(
        margin: const EdgeInsets.all(8),
        padding: const EdgeInsets.all(12),
        constraints: BoxConstraints(
          maxWidth: MediaQuery.of(context).size.width * 0.8,
        ),
        decoration: BoxDecoration(
          color: Theme.of(context).colorScheme.primaryContainer,
          borderRadius: BorderRadius.circular(8),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (text.trim().isNotEmpty) AcpMarkdown(data: text),
            if (images.isNotEmpty) ...[
              if (text.trim().isNotEmpty) const SizedBox(height: 8),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                children: images
                    .map((image) => AcpImageThumbnail(image: image))
                    .toList(),
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _buildDivider(String label) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 8),
      child: Row(
        children: [
          const Expanded(child: Divider()),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 8),
            child: Text(
              label,
              style: TextStyle(
                color: Theme.of(context).colorScheme.onSurfaceVariant,
                fontSize: 12,
              ),
            ),
          ),
          const Expanded(child: Divider()),
        ],
      ),
    );
  }

  Widget _buildInputBar() {
    final hasSnapshot = _snapshot != null;
    final acceptsPrompt = _snapshot?.phase.acceptsPrompt ?? false;
    final canCompose = gatewayService.writeEnabled && !_sendingMessage;
    final hasContent =
        _messageController.text.trim().isNotEmpty || _pendingImages.isNotEmpty;
    final canSend =
        hasSnapshot &&
        acceptsPrompt &&
        gatewayService.writeEnabled &&
        !_sendingMessage &&
        hasContent;

    return Container(
      padding: const EdgeInsets.all(8),
      decoration: BoxDecoration(
        // 输入区用 bar 面，比消息区(panel)暗一档才分得开。
        color: Theme.of(context).colorScheme.surfaceContainerLow,
        border: Border(
          top: BorderSide(color: Theme.of(context).colorScheme.outlineVariant),
        ),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (_snapshot case final snapshot?) _buildComposerMetadata(snapshot),
          if (_pendingImages.isNotEmpty) _buildPendingImages(),
          if (_sendError case final error?)
            Padding(
              padding: const EdgeInsets.only(bottom: 6),
              child: Text(
                error,
                style: TextStyle(
                  color: Theme.of(context).colorScheme.error,
                  fontSize: 12,
                ),
              ),
            ),
          Row(
            crossAxisAlignment: CrossAxisAlignment.end,
            children: [
              IconButton(
                tooltip: _snapshot?.supportsImage == false
                    ? 'This agent does not support images'
                    : 'Attach images',
                onPressed: canCompose && (_snapshot?.supportsImage ?? false)
                    ? _pickImages
                    : null,
                icon: const Icon(Icons.add_photo_alternate_outlined),
              ),
              if (_snapshot?.availableCommands.isNotEmpty == true)
                PopupMenuButton<List<String>>(
                  tooltip: 'Insert command',
                  icon: const Icon(Icons.terminal),
                  onSelected: (command) {
                    _messageController.text = '/${command.first} ';
                    _messageController.selection = TextSelection.collapsed(
                      offset: _messageController.text.length,
                    );
                    _messageFocusNode.requestFocus();
                    setState(() {});
                  },
                  itemBuilder: (context) => _snapshot!.availableCommands
                      .map(
                        (command) => PopupMenuItem(
                          value: command,
                          child: ListTile(
                            contentPadding: EdgeInsets.zero,
                            title: Text('/${command.first}'),
                            subtitle: command.length > 1
                                ? Text(command[1])
                                : null,
                          ),
                        ),
                      )
                      .toList(),
                ),
              Expanded(
                child: TextField(
                  controller: _messageController,
                  focusNode: _messageFocusNode,
                  enabled: canCompose,
                  minLines: 1,
                  maxLines: 5,
                  textInputAction: TextInputAction.newline,
                  onTapOutside: (_) => _dismissKeyboard(),
                  decoration: InputDecoration(
                    hintText: !gatewayService.writeEnabled
                        ? _connectionState == WsState.connected
                              ? 'Desktop connection is read-only'
                              : 'Reconnect to send a message'
                        : _sendingMessage
                        ? 'Waiting for desktop confirmation...'
                        : !hasSnapshot
                        ? 'Loading session...'
                        : acceptsPrompt
                        ? 'Message the agent...'
                        : 'Finish the pending action first...',
                    border: const OutlineInputBorder(),
                    suffixIcon: _isMessageFocused
                        ? IconButton(
                            tooltip: 'Dismiss keyboard',
                            onPressed: _dismissKeyboard,
                            icon: const Icon(Icons.keyboard_hide_outlined),
                          )
                        : null,
                  ),
                ),
              ),
              const SizedBox(width: 4),
              IconButton.filled(
                tooltip: 'Send message',
                onPressed: canSend ? _sendMessage : null,
                icon: _sendingMessage
                    ? const SizedBox.square(
                        dimension: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.send),
              ),
            ],
          ),
        ],
      ),
    );
  }

  Widget _buildComposerMetadata(AcpSnapshot snapshot) {
    final items = <Widget>[];
    if (snapshot.usage case final usage? when usage.contextWindow > 0) {
      final percent = (usage.usedTokens / usage.contextWindow * 100)
          .clamp(0, 100)
          .round();
      items.add(Chip(label: Text('Context $percent%')));
    }
    if (snapshot.model case final model? when model.currentName.isNotEmpty) {
      items.add(
        PopupMenuButton<String>(
          tooltip: 'Switch model',
          enabled: model.options.length > 1 && gatewayService.writeEnabled,
          onSelected: (value) => gatewayService.setConfigOption(
            widget.session.id,
            model.configId,
            value,
          ),
          itemBuilder: (context) => model.options
              .map(
                (option) => CheckedPopupMenuItem(
                  value: option.first,
                  checked: option.length > 1 && option[1] == model.currentName,
                  child: Text(option.length > 1 ? option[1] : option.first),
                ),
              )
              .toList(),
          child: Chip(
            avatar: const Icon(Icons.memory, size: 16),
            label: Text(model.currentName),
          ),
        ),
      );
    }
    for (final config in snapshot.configOptions) {
      if (config.options.length < 2) continue;
      items.add(
        PopupMenuButton<String>(
          tooltip: config.description ?? config.name,
          enabled: gatewayService.writeEnabled,
          onSelected: (value) => gatewayService.setConfigOption(
            widget.session.id,
            config.configId,
            value,
          ),
          itemBuilder: (context) => config.options
              .map(
                (option) => CheckedPopupMenuItem(
                  value: option.first,
                  checked: option.length > 1 && option[1] == config.currentName,
                  child: Text(option.length > 1 ? option[1] : option.first),
                ),
              )
              .toList(),
          child: Chip(label: Text(config.currentName)),
        ),
      );
    }
    if (items.isEmpty) return const SizedBox.shrink();
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      padding: const EdgeInsets.only(bottom: 6),
      child: Row(spacing: 6, children: items),
    );
  }

  Widget _buildPendingImages() {
    return SizedBox(
      height: 76,
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.only(bottom: 8),
        itemCount: _pendingImages.length,
        separatorBuilder: (_, _) => const SizedBox(width: 8),
        itemBuilder: (context, index) => Stack(
          clipBehavior: Clip.none,
          children: [
            SizedBox(
              width: 76,
              height: 68,
              child: AcpImageThumbnail(image: _pendingImages[index]),
            ),
            Positioned(
              top: -6,
              right: -6,
              child: IconButton.filled(
                visualDensity: VisualDensity.compact,
                tooltip: 'Remove image',
                onPressed: () {
                  setState(() => _pendingImages.removeAt(index));
                  _scheduleDraftSave();
                },
                icon: const Icon(Icons.close, size: 14),
              ),
            ),
          ],
        ),
      ),
    );
  }

  Future<void> _pickImages() async {
    final remaining = 4 - _pendingImages.length;
    if (remaining <= 0) return;
    try {
      final files = await _imagePicker.pickMultiImage(
        limit: remaining,
        maxWidth: 2048,
        maxHeight: 2048,
        imageQuality: 85,
        requestFullMetadata: false,
      );
      var skipped = 0;
      final images = <AcpImageData>[];
      for (final file in files) {
        var bytes = await file.readAsBytes();
        var mimeType = file.mimeType ?? _imageMimeFromName(file.name);
        if (mimeType == 'image/jpeg' || mimeType == 'image/heic') {
          final normalized = await compute(normalizeJpegOrientation, bytes);
          if (normalized == null) {
            skipped++;
            continue;
          }
          bytes = normalized;
          mimeType = 'image/jpeg';
        }
        if (bytes.length > 5 * 1024 * 1024) {
          skipped++;
          continue;
        }
        images.add(
          AcpImageData(mimeType: mimeType, base64: base64Encode(bytes)),
        );
      }
      if (!mounted) return;
      setState(() => _pendingImages.addAll(images));
      _scheduleDraftSave();
      if (skipped > 0) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('$skipped image(s) exceeded the 5 MB limit')),
        );
      }
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text('Could not attach image: $error')));
    }
  }

  Future<void> _sendMessage() async {
    final text = _messageController.text.trim();
    if (_sendingMessage || (text.isEmpty && _pendingImages.isEmpty)) return;

    _draftSaveTimer?.cancel();
    final images = List<AcpImageData>.of(_pendingImages);
    final requestId = gatewayService.createMessageRequestId();
    setState(() {
      _sendingMessage = true;
      _sendRequestId = requestId;
      _sendError = null;
    });
    try {
      await _saveDraft(requestId: requestId);
      gatewayService.sendMessage(
        widget.session.id,
        text,
        images: images,
        requestId: requestId,
      );
    } catch (error) {
      if (!mounted) {
        await _messageSendSubscription.cancel();
        return;
      }
      setState(() {
        _sendingMessage = false;
        _sendRequestId = null;
        _sendError = 'Could not save the draft: $error';
      });
    }
  }

  void _respondApproval(String toolCallId, String optionKey) {
    if (_permissionSubmittingToolId != null) return;
    // 触感回执由 ApprovalCard 统一负责，这里再打一次会变成双震。
    setState(() => _permissionSubmittingToolId = toolCallId);
    gatewayService.respondApproval(widget.session.id, toolCallId, optionKey);
  }

  @override
  void dispose() {
    _draftSaveTimer?.cancel();
    if (!_sendingMessage) _messageSendSubscription.cancel();
    _connectionStateSubscription.cancel();
    _snapshotSubscription.cancel();
    _attentionResolvedSubscription.cancel();
    if (gatewayService.subscribedSessionId == widget.session.id) {
      gatewayService.unsubscribe();
    }
    _messageController.dispose();
    _messageFocusNode
      ..removeListener(_handleMessageFocus)
      ..dispose();
    _scrollController.dispose();
    super.dispose();
  }
}
