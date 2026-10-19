import 'dart:async';
import 'dart:io';

import 'package:device_info_plus/device_info_plus.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:image_picker/image_picker.dart';
import 'package:path_provider/path_provider.dart';
import 'models/saved_runtime.dart';
import 'models/session_filters.dart';
import 'widgets/agent_icon.dart';
import 'widgets/project_avatar.dart';
import 'pages/agents_page.dart';
import 'pages/console_page.dart';
import 'pages/qr_scanner_page.dart';
import 'pages/terminal_session_page.dart';
import 'services/message_draft_store.dart';
import 'services/pairing_storage.dart';
import 'services/pending_actions_controller.dart';
import 'services/runtime_service.dart';
import 'rust_lib.dart';
import 'src/rust/api/identity.dart' show initClient;
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
  required AttentionItem attention,
  required String? activeSessionId,
  required String? subscribedSessionId,
}) {
  if (activeSessionId == attention.sessionId) return false;
  if (subscribedSessionId == attention.sessionId && !attention.requiresAction) {
    return false;
  }
  return true;
}

Future<String?> showRuntimeRenameDialog(
  BuildContext context,
  String initialName,
) {
  var editedName = initialName;
  return showDialog<String>(
    context: context,
    builder: (dialogContext) => AlertDialog(
      title: const Text('Rename runtime'),
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
    required this.hasAgents,
    required this.onNewSession,
    required this.onHistory,
  });

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
        const PopupMenuItem(
          value: 'session',
          child: ListTile(
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
  required RuntimeConnectionState state,
  required bool cached,
  DateTime? cachedAt,
  VoidCallback? onRetry,
}) {
  if (state == RuntimeConnectionState.connected && !cached) {
    return const SizedBox.shrink();
  }
  return CachedConnectionBar(
    state: state,
    cachedAt: cachedAt,
    onRetry: onRetry,
  );
}

class CachedConnectionBar extends StatelessWidget {
  const CachedConnectionBar({
    super.key,
    required this.state,
    this.cachedAt,
    this.onRetry,
  });

  final RuntimeConnectionState state;
  final DateTime? cachedAt;

  /// 断线时的重试入口。放在这条状态栏上，而不是浮动按钮里：用户是在这里读到
  /// 「Offline」的，动作就该长在同一处。
  final VoidCallback? onRetry;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final label = switch (state) {
      RuntimeConnectionState.reconnecting => 'Reconnecting',
      RuntimeConnectionState.connecting => 'Connecting',
      RuntimeConnectionState.connected => 'Refreshing',
      RuntimeConnectionState.disconnected => 'Offline',
    };
    final age = cachedAt == null ? null : _formatCacheAge(cachedAt!);
    final offline = state == RuntimeConnectionState.disconnected;
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

/// 离线缓存的保存时间（Unix 毫秒）转成横幅要的时间。
DateTime? cachedAtFromMs(int? savedAtMs) =>
    savedAtMs == null ? null : DateTime.fromMillisecondsSinceEpoch(savedAtMs);

/// 配对时报给运行时的设备名，电脑端的设备列表里显示的就是它。
///
/// iOS 16 起，没有苹果单独批的 entitlement 读不到用户起的名字（系统只给通用的
/// 「iPhone」），所以 iOS 上实际用的是机型名，如「iPhone 12 Pro」。
Future<String> defaultDeviceName() async {
  final plugin = DeviceInfoPlugin();
  try {
    if (Platform.isIOS) {
      final info = await plugin.iosInfo;
      return deviceNameFor(userAssigned: info.name, model: info.modelName);
    }
    if (Platform.isAndroid) {
      final info = await plugin.androidInfo;
      return deviceNameFor(model: '${info.manufacturer} ${info.model}');
    }
  } catch (_) {
    // 读不到设备信息不该挡住配对，退回主机名。
  }
  return deviceNameFor(model: Platform.localHostname);
}

/// 系统不给真实名字时返回的通用名。
const _genericDeviceNames = {'iPhone', 'iPad', 'iPod touch'};

/// 用户起的名字可用就用它，否则用机型名。
@visibleForTesting
String deviceNameFor({String? userAssigned, required String model}) {
  final name = userAssigned?.trim() ?? '';
  if (name.isNotEmpty && !_genericDeviceNames.contains(name)) return name;
  final fallback = model.trim();
  if (fallback.isNotEmpty && fallback != 'localhost') return fallback;
  return 'Smelt Mobile';
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
  // 设备身份（iroh 密钥）和离线缓存都落在 App Support 目录里，配对和连接之前
  // 必须先初始化。
  final dataDir = await getApplicationSupportDirectory();
  await initClient(dataDir: dataDir.path);
  runtimeService.start();
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
    _appearanceStore =
        widget.appearancePrefsStore ?? FileAppearancePrefsStore();
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
  ConnectionEvent _connection = runtimeService.connection;
  SessionList _sessionList = runtimeService.sessionList;
  WorkspaceCatalog _workspace = runtimeService.workspace;
  late final StreamSubscription<ConnectionEvent> _connectionSubscription;
  late final StreamSubscription<SessionList> _sessionsSubscription;
  late final StreamSubscription<WorkspaceCatalog> _workspaceSubscription;
  late final StreamSubscription<AttentionItem> _attentionSubscription;
  late final StreamSubscription<String> _attentionResolvedSubscription;
  late final StreamSubscription<String> _errorSubscription;
  late final PairingStorage _pairingStorage;
  late final MessageDraftStore _messageDraftStore;
  String? _shownAttentionSessionId;
  bool _restoringPairing = true;

  /// 正在配对。配对失败由配对流程自己报，连接状态流里同一个错误不再报一遍。
  bool _pairing = false;
  SavedRuntimeCollection _savedRuntimes = const SavedRuntimeCollection();
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

  /// 刚建好、等它出现在会话列表里就打开的会话。
  String? _pendingOpenSessionId;
  String? _activeSessionId;

  final _pairingCodeController = TextEditingController();

  RuntimeConnectionState get _connectionState => _connection.state;
  List<SessionSummary> get _sessions => _sessionList.sessions;

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
    _connectionSubscription = runtimeService.connectionStream.listen((event) {
      if (!mounted) return;
      final previousError = _connection.error;
      setState(() => _connection = event);
      final error = event.error;
      if (error != null &&
          error != previousError &&
          event.state == RuntimeConnectionState.disconnected) {
        _showConnectionError(error);
      }
    });
    _sessionsSubscription = runtimeService.sessionListStream.listen((list) {
      if (!mounted) return;
      setState(() => _sessionList = list);
      _openPendingCreatedSession();
    });
    _workspaceSubscription = runtimeService.workspaceStream.listen((workspace) {
      if (!mounted) return;
      setState(() => _workspace = workspace);
    });
    _attentionSubscription = runtimeService.attentionStream.listen((item) {
      if (!mounted || !_acceptConnectionNotifications) return;
      if (_activeSessionId == item.sessionId) {
        runtimeService.markRead(item.sessionId);
      }
      final watchedSessionId = runtimeService.watchedSessionId;
      final isCurrent = watchedSessionId == item.sessionId;
      if (!shouldShowAttentionNotification(
        attention: item,
        activeSessionId: _activeSessionId,
        subscribedSessionId: watchedSessionId,
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
    _attentionResolvedSubscription = runtimeService.attentionResolvedStream
        .listen((sessionId) {
          if (!mounted || _shownAttentionSessionId != sessionId) return;
          _shownAttentionSessionId = null;
          ScaffoldMessenger.of(context).hideCurrentSnackBar();
        });
    _errorSubscription = runtimeService.errorStream.listen(_showMessage);
    unawaited(_restorePairing());
  }

  /// 回到前台立刻确认连接还活着。系统在后台会冻结心跳，这段时间里网络往往已经
  /// 换过——不主动探一下，用户看到的就是进后台那一刻的旧会话。
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    super.didChangeAppLifecycleState(state);
    if (state == AppLifecycleState.resumed) {
      runtimeService.verifyConnection();
    }
  }

  void _showMessage(String message) {
    if (!mounted || !_acceptConnectionNotifications) return;
    final messenger = ScaffoldMessenger.of(context);
    messenger.clearSnackBars();
    messenger.showSnackBar(SnackBar(content: Text(message)));
  }

  /// 运行时把这台设备拒在门外时说清楚该怎么办。离线由横幅表达，被新请求取代的
  /// 连接本来就没人关心，都不弹。
  void _showConnectionError(SmeltError error) {
    if (_pairing || !_acceptConnectionNotifications) return;
    switch (error.kind) {
      case SmeltErrorKind.offline:
      case SmeltErrorKind.cancelled:
        return;
      case SmeltErrorKind.unpaired:
        final messenger = ScaffoldMessenger.of(context);
        messenger.clearSnackBars();
        messenger.showSnackBar(
          SnackBar(
            content: Text(_unpairedMessage),
            duration: const Duration(seconds: 8),
            action: SnackBarAction(label: 'Scan', onPressed: _scanQrCode),
          ),
        );
      case SmeltErrorKind.invalidPairing:
      case SmeltErrorKind.incompatible:
      case SmeltErrorKind.other:
        _showMessage(error.message);
    }
  }

  String get _unpairedMessage {
    final name = _savedRuntimes.activeRuntime?.name ?? 'this runtime';
    return 'This device was revoked or is not paired with $name. '
        'Scan a new pairing code to reconnect.';
  }

  Future<void> _restorePairing() async {
    try {
      final savedRuntimes = await _pairingStorage.load();
      if (!mounted) return;
      setState(() {
        _restoringPairing = false;
        _savedRuntimes = savedRuntimes;
      });
      final active = savedRuntimes.activeRuntime;
      if (active != null) _connectRuntime(active);
    } catch (error) {
      if (!mounted) return;
      setState(() => _restoringPairing = false);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not restore pairing: $error')),
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
      _sessions.isEmpty && _connectionState != RuntimeConnectionState.connected;

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
        RuntimeConnectionState.disconnected => _buildDisconnectedView(),
        RuntimeConnectionState.connecting => _buildConnectingView(),
        RuntimeConnectionState.reconnecting => _buildReconnectingView(),
        RuntimeConnectionState.connected => _buildSessionList(),
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
      runtimes: _savedRuntimes,
      connectionState: _connectionState,
      link: _connection.link,
      connectionBar: _buildConnectionBar(),
      themeMode: widget.themeMode,
      onThemeModeChanged: widget.onThemeModeChanged ?? (_) {},
      terminalFontSize: _terminalPrefs.fontSize,
      onTerminalFontSizeChanged: _setTerminalFontSize,
      onSwitchRuntime: _showRuntimeSwitcher,
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
      cached: _sessionList.cached,
      cachedAt: cachedAtFromMs(_sessionList.savedAtMs),
      onRetry: _connectionState == RuntimeConnectionState.disconnected
          ? _retry
          : null,
    );
  }

  /// 重连当前保存的运行时。
  void _retry() {
    final active = _savedRuntimes.activeRuntime;
    if (active != null &&
        _connectionState == RuntimeConnectionState.disconnected) {
      _connectRuntime(active);
    } else {
      unawaited(runtimeService.retry());
    }
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
          if (_connection.error?.kind == SmeltErrorKind.unpaired) ...[
            const SizedBox(height: 8),
            Text(
              _unpairedMessage,
              textAlign: TextAlign.center,
              style: TextStyle(color: Theme.of(context).colorScheme.error),
            ),
          ],
          if (_savedRuntimes.activeRuntime case final active?) ...[
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
              onPressed: _retry,
              icon: const Icon(Icons.sync),
              label: const Text('Retry saved runtime'),
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
                  hintText: 'smelt+iroh://…',
                  helperText: pairingGuidance,
                  helperMaxLines: 3,
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
            onPressed: _pairing ? null : _manualConnect,
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
          if (_savedRuntimes.runtimes.isNotEmpty) ...[
            const SizedBox(height: 12),
            TextButton.icon(
              onPressed: _showRuntimeSwitcher,
              icon: const Icon(Icons.desktop_mac_outlined),
              label: const Text('Manage saved runtimes'),
            ),
          ],
        ],
      ),
    );
  }

  Widget _buildSessionList() {
    // 列表已由 Rust 按桌面菜单的顺序排好。
    final orderedSessions = _sessions
        .where(sessionBelongsToProjectTree)
        .toList();

    return Column(
      children: [
        _buildConnectionBar(),
        Expanded(child: _buildProjectSessionList(orderedSessions)),
      ],
    );
  }

  /// 下拉刷新。手机上「怀疑列表过期了」的第一反应是下拉，而不是去够右下角的
  /// 浮动按钮。会话列表由运行时推送，这里探一次活、重取工作区目录；超时兜底
  /// 避免离线时一直转。
  Future<void> _refreshSessions() async {
    if (_connectionState == RuntimeConnectionState.disconnected) {
      _retry();
      return;
    }
    runtimeService.verifyConnection();
    try {
      await runtimeService.refreshWorkspace().timeout(
        const Duration(seconds: 5),
      );
    } catch (_) {
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
              collapsedIconColor: Theme.of(
                context,
              ).colorScheme.onSurfaceVariant,
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
                identityKey: project.root.isEmpty
                    ? project.title
                    : project.root,
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
      // 手势和无障碍动作都由 SessionRow 自己挂。
      onDelete: () => _deleteSession(session),
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
    final sections = _workspace.sections;
    final grouped = [
      ('Pinned', sections.common),
      ('Terminals', sections.terminal),
      ('Conversations', sections.conversation),
    ];
    return showModalBottomSheet<LaunchAction>(
      context: context,
      showDragHandle: true,
      builder: (context) {
        final theme = Theme.of(context);
        final rows = <Widget>[const ListTile(title: Text('New session'))];
        for (final (title, actions) in grouped) {
          if (actions.isEmpty) continue;
          rows.add(
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
              child: Text(
                title,
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
        if (sections.isEmpty) {
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
    await _createAndOpen(
      runtimeService.createSession(
        projectRoot: project.root,
        launchKey: action.key,
      ),
    );
  }

  /// 建好之后等它出现在会话列表里就打开。
  Future<void> _createAndOpen(Future<String> request) async {
    try {
      final sessionId = await request;
      if (!mounted) return;
      _pendingOpenSessionId = sessionId;
      _openPendingCreatedSession();
    } catch (error) {
      _showMessage(
        'Could not start the session: ${describeRuntimeError(error)}',
      );
    }
  }

  void _openPendingCreatedSession() {
    final pendingId = _pendingOpenSessionId;
    if (pendingId == null) return;
    final created = _sessions
        .where((session) => session.id == pendingId)
        .firstOrNull;
    if (created == null) return;
    _pendingOpenSessionId = null;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _openSession(created);
    });
  }

  /// 工作区目录里真的能开对话的智能体。目录还没到就是空集——那时「开始对话」
  /// 不出现，比出现一个按了会报错的按钮好。
  Set<String> get _startableAgentIds => {
    for (final agent in _workspace.agents) ?agent.agentDefinitionId,
  };

  /// 从「智能体」栏开对话：不绑项目，运行时把它落在智能体自己的 space，跟桌面
  /// 点智能体开对话是同一条路径。
  void _startAgentConversation(String agentDefinitionId) {
    final option = _workspace.agents
        .where((agent) => agent.agentDefinitionId == agentDefinitionId)
        .firstOrNull;
    if (option == null) return;
    unawaited(
      _createAndOpen(
        runtimeService.createSession(projectRoot: '', agentOptionId: option.id),
      ),
    );
  }

  Future<void> _openHistory(WorkspaceProject project) async {
    final resume = await Navigator.push<HistoryResume>(
      context,
      MaterialPageRoute(
        builder: (context) =>
            SessionHistoryPage(project: project, agents: _workspace.agents),
      ),
    );
    if (!mounted || resume == null) return;
    await _createAndOpen(
      runtimeService.createSession(
        projectRoot: project.root,
        agentOptionId: resume.agentOptionId,
        resumeId: resume.resumeId,
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
            style: dangerFillStyle(context.smeltColors),
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (confirmed != true) return;
    try {
      await runtimeService.deleteSession(session.id);
    } catch (error) {
      _showMessage('Could not delete: ${describeRuntimeError(error)}');
    }
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
          Text(
            _pairing ? 'Pairing...' : 'Connecting...',
            textAlign: TextAlign.center,
          ),
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
    final uri = await Navigator.push<String>(
      context,
      MaterialPageRoute(builder: (context) => const QrScannerPage()),
    );
    if (!mounted || uri == null) return;
    await _pair(uri);
  }

  void _manualConnect() {
    final uri = _pairingCodeController.text.trim();
    try {
      runtimeService.parsePairing(uri);
    } on SmeltError catch (error) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(error.message)));
      return;
    }
    unawaited(_pair(uri));
  }

  /// 用配对串配对。成功后运行时就是当前运行时（已连上），记下来；失败时连回原来
  /// 的运行时——配对会先断开当前连接，配对码打错不该让人连原来的也丢了。
  Future<void> _pair(String uri) async {
    _acceptConnectionNotifications = true;
    ScaffoldMessenger.of(context).clearSnackBars();
    setState(() => _pairing = true);
    final RuntimeInfo info;
    try {
      info = await runtimeService.pair(uri, await defaultDeviceName());
    } catch (error) {
      if (!mounted) return;
      setState(() => _pairing = false);
      final previous = _savedRuntimes.activeRuntime;
      if (previous != null) _connectRuntime(previous);
      _showMessage('Pairing failed: ${describeRuntimeError(error)}');
      return;
    }
    if (!mounted) return;
    setState(() {
      _pairing = false;
      _pairingCodeController.clear();
    });
    try {
      final saved = await _pairingStorage.save(info);
      if (mounted) setState(() => _savedRuntimes = saved);
    } catch (error) {
      _showMessage('Connected, but the runtime was not saved: $error');
    }
  }

  void _connectRuntime(SavedRuntime runtime) {
    _acceptConnectionNotifications = true;
    ScaffoldMessenger.of(context).clearSnackBars();
    unawaited(runtimeService.connect(runtime.endpointId, runtime.relayUrl));
  }

  void _disconnect({bool forget = false}) {
    _acceptConnectionNotifications = false;
    _shownAttentionSessionId = null;
    ScaffoldMessenger.of(context).clearSnackBars();
    unawaited(runtimeService.disconnect(forget: forget));
    setState(
      () => _sessionList = const SessionList(sessions: [], cached: true),
    );
  }

  Future<void> _showRuntimeSwitcher() async {
    final selected = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (sheetContext) {
        final activeId = _savedRuntimes.activeRuntimeId;
        return SafeArea(
          child: ConstrainedBox(
            constraints: BoxConstraints(
              maxHeight: MediaQuery.sizeOf(sheetContext).height * 0.72,
            ),
            child: ListView(
              shrinkWrap: true,
              children: [
                const ListTile(
                  title: Text('Runtimes'),
                  subtitle: Text('Switch or manage paired runtimes'),
                ),
                ..._savedRuntimes.runtimes.map(
                  (runtime) => ListTile(
                    leading: Icon(
                      runtime.runtimeId == activeId
                          ? Icons.desktop_windows
                          : Icons.desktop_windows_outlined,
                    ),
                    title: Text(runtime.name),
                    subtitle: Text(_runtimeSubtitle(runtime)),
                    selected: runtime.runtimeId == activeId,
                    onTap: () => Navigator.pop(sheetContext, runtime.runtimeId),
                    trailing: PopupMenuButton<String>(
                      tooltip: 'Runtime actions',
                      onSelected: (action) {
                        Navigator.pop(sheetContext);
                        if (action == 'rename') {
                          unawaited(_renameRuntime(runtime));
                        } else if (action == 'remove') {
                          unawaited(_removeRuntime(runtime));
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
                  title: const Text('Pair another runtime'),
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
    await _switchRuntime(selected);
  }

  /// 同名的运行时靠 endpoint 前缀分开。
  String _runtimeSubtitle(SavedRuntime runtime) {
    final id = runtime.endpointId;
    return id.length > 12 ? 'Endpoint ${id.substring(0, 12)}…' : 'Endpoint $id';
  }

  Future<void> _switchRuntime(String runtimeId) async {
    try {
      final savedRuntimes = await _pairingStorage.setActive(runtimeId);
      if (!mounted) return;
      final active = savedRuntimes.activeRuntime;
      setState(() {
        _savedRuntimes = savedRuntimes;
        _pairingCodeController.clear();
      });
      if (active != null) _connectRuntime(active);
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not switch runtime: $error')),
      );
    }
  }

  Future<void> _renameRuntime(SavedRuntime runtime) async {
    final name = await showRuntimeRenameDialog(context, runtime.name);
    if (!mounted || name == null || name.trim().isEmpty) return;
    try {
      final savedRuntimes = await _pairingStorage.rename(
        runtime.runtimeId,
        name,
      );
      if (mounted) setState(() => _savedRuntimes = savedRuntimes);
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not rename runtime: $error')),
      );
    }
  }

  /// 只删本机记录；运行时那边的设备授权由运行时自己管理。
  Future<void> _removeRuntime(SavedRuntime runtime) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Forget runtime?'),
        content: Text(
          '${runtime.name} will be removed from this phone. You can pair it again later.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            style: dangerFillStyle(context.smeltColors),
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Forget'),
          ),
        ],
      ),
    );
    if (!mounted || confirmed != true) return;

    try {
      final wasActive = _savedRuntimes.activeRuntimeId == runtime.runtimeId;
      final savedRuntimes = await _pairingStorage.remove(runtime.runtimeId);
      if (!mounted) return;
      setState(() {
        _savedRuntimes = savedRuntimes;
        _pairingCodeController.clear();
      });
      if (!wasActive) return;
      final next = savedRuntimes.activeRuntime;
      if (next == null) {
        _disconnect(forget: true);
      } else {
        _connectRuntime(next);
      }
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not forget runtime: $error')),
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
    runtimeService.markRead(session.id);
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
    _connectionSubscription.cancel();
    _sessionsSubscription.cancel();
    _workspaceSubscription.cancel();
    _attentionSubscription.cancel();
    _attentionResolvedSubscription.cancel();
    _errorSubscription.cancel();
    _pairingCodeController.dispose();
    super.dispose();
  }
}

/// 历史页选中「续接」时带回去的东西：哪个 agent 选项、哪段历史。
typedef HistoryResume = ({String agentOptionId, String resumeId});

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
  String? _error;

  /// 切 agent 时旧请求的结果要丢掉。
  int _loadGeneration = 0;

  @override
  void initState() {
    super.initState();
    _agent = widget.agents.first;
    unawaited(_load());
  }

  Future<void> _load() async {
    final generation = ++_loadGeneration;
    final agentId = _agent.id;
    setState(() {
      _loading = true;
      _error = null;
      _sessions = const [];
    });
    try {
      final sessions = await runtimeService.listHistory(
        widget.project.root,
        agentId,
      );
      if (!mounted || generation != _loadGeneration) return;
      setState(() {
        _sessions = sessions;
        _loading = false;
      });
    } catch (error) {
      if (!mounted || generation != _loadGeneration) return;
      setState(() {
        _loading = false;
        _error = describeRuntimeError(error);
      });
    }
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
          FilledButton(
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
    await _applyRename(session, trimmed.isEmpty ? null : trimmed);
  }

  /// 改名结果由运行时算好回传（恢复默认时要拿回 agent 原始标题），这里就地替换，
  /// 不用为了一个名字重扫一遍历史目录。
  Future<void> _applyRename(
    HistorySessionSummary session,
    String? title,
  ) async {
    final agentId = _agent.id;
    try {
      final result = await runtimeService.renameHistory(
        widget.project.root,
        agentId,
        session.resumeId,
        title: title,
      );
      if (!mounted || agentId != _agent.id) return;
      setState(() {
        _sessions = [
          for (final item in _sessions)
            item.resumeId == session.resumeId
                ? HistorySessionSummary(
                    resumeId: item.resumeId,
                    // 有自定义名时 `result.title` 就是它，agent 原始标题不变；
                    // 恢复默认时 `result.title` 才是原始标题。
                    title: result.customTitle == null
                        ? result.title
                        : item.title,
                    customTitle: result.customTitle,
                    lastActiveAtMs: item.lastActiveAtMs,
                    messageCount: item.messageCount,
                  )
                : item,
        ];
      });
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text('Could not rename: ${describeRuntimeError(error)}'),
        ),
      );
    }
  }

  void _resume(HistorySessionSummary session) {
    Navigator.pop<HistoryResume>(context, (
      agentOptionId: _agent.id,
      resumeId: session.resumeId,
    ));
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
                unawaited(_load());
              },
            ),
          ),
          Expanded(
            child: _loading
                ? const Center(child: CircularProgressIndicator())
                : _error != null
                ? Center(
                    child: Padding(
                      padding: const EdgeInsets.all(24),
                      child: Text(_error!, textAlign: TextAlign.center),
                    ),
                  )
                : _sessions.isEmpty
                ? const Center(child: Text('No resumable conversations'))
                : RefreshIndicator(
                    onRefresh: _load,
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
                                onPressed: () => _resume(session),
                              ),
                              PopupMenuButton<String>(
                                tooltip: 'More',
                                onSelected: (action) => switch (action) {
                                  'rename' => _rename(session),
                                  'reset' => _applyRename(session, null),
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
                          onTap: () => _resume(session),
                        );
                      },
                    ),
                  ),
          ),
        ],
      ),
    );
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

/// 发送没等到确认时的提示：消息可能已经到了，重发前要先看一眼。
const _unconfirmedDeliveryHint =
    'Delivery was not confirmed. Review and retry.';

class _SessionPageState extends State<SessionPage> {
  final TextEditingController _messageController = TextEditingController();
  final FocusNode _messageFocusNode = FocusNode();
  final ScrollController _scrollController = ScrollController();
  final ImagePicker _imagePicker = ImagePicker();
  ConversationView? _view;

  /// 当前视图来自离线缓存，还没收到运行时的实时数据。
  bool _cached = false;
  int? _savedAtMs;
  bool _loading = true;
  bool _isAtBottom = true;
  bool _loadingOlder = false;
  bool _isMessageFocused = false;
  bool _restoringDraft = true;
  bool _sendingMessage = false;
  RuntimeConnectionState _connectionState = runtimeService.state;

  /// 输入框上次的内容。控制器在光标移动时也会通知，只有内容变了才算编辑。
  String _draftText = '';

  /// 上一次没等到确认的发送。草稿没改过就重发时沿用这个 id，运行时按它去重；
  /// 改过内容就是一次新的提交，id 随之作废。
  String? _unconfirmedRequestId;
  String? _sendError;
  String? _permissionSubmittingToolId;
  final List<ImageInput> _pendingImages = [];

  /// 询问里用户改过、还没交给运行时的文字，提交时一并带上。
  final Map<int, String> _elicitationTextValues = {};

  /// 这场对话已加载的技能。null = 还没问到（或引擎没有技能这个概念），界面就不画
  /// 入口——与桌面一致：技能是 Pi 的概念，别家 agent 上画一个永远空的列表没有意义。
  List<SessionSkill>? _skills;

  /// 运行时接不上这段对话（或订阅出错）时的原因。非空就别再转圈。
  String? _unavailableReason;
  late final ConversationWatch _watch;
  late final StreamSubscription<ConversationEvent> _watchSubscription;
  late final StreamSubscription<ConnectionEvent> _connectionSubscription;
  Timer? _draftSaveTimer;

  @override
  void initState() {
    super.initState();
    _messageController.addListener(_handleMessageChanged);
    _messageFocusNode.addListener(_handleMessageFocus);
    _scrollController.addListener(_handleScrollPosition);
    _watch = runtimeService.watchConversation(widget.session.id);
    _watchSubscription = _watch.events.listen(_handleConversationEvent);
    _connectionSubscription = runtimeService.connectionStream.listen(
      _handleConnection,
    );
    unawaited(_loadSkills());
    unawaited(_restoreDraft());
  }

  void _handleConnection(ConnectionEvent event) {
    if (!mounted) return;
    final previous = _connectionState;
    setState(() => _connectionState = event.state);
    if (event.state != RuntimeConnectionState.connected ||
        previous == RuntimeConnectionState.connected) {
      return;
    }
    // 断开过或换了运行时，Rust 的订阅已经停了，要重新订阅；只是后台重连
    // （reconnecting）的话它自己会接上。
    if (previous != RuntimeConnectionState.reconnecting) _watch.restart();
    // 技能表在运行时那边算，重连后重新问：对方可能已经换了一台机器。
    unawaited(_loadSkills());
  }

  void _handleConversationEvent(ConversationEvent event) {
    if (!mounted) return;
    switch (event.kind) {
      case ConversationEventKind.snapshot:
        final view = event.view;
        if (view == null) return;
        final initialLoad = _view == null;
        final previousOffset = _view?.entriesOffset;
        final shouldFollowLatest = shouldAutoFollowSnapshot(
          initialLoad: initialLoad,
          wasAtBottom: _isAtBottom,
        );
        setState(() {
          _view = view;
          _cached = event.cached;
          _savedAtMs = event.savedAtMs;
          _syncViewControls();
          if (previousOffset == null ||
              view.entriesOffset < previousOffset ||
              !view.hasMoreBefore) {
            _loadingOlder = false;
          }
          _loading = false;
          _unavailableReason = null;
        });
        if (shouldFollowLatest) _scrollToBottom(animate: !initialLoad);
      case ConversationEventKind.unavailable:
        setState(() {
          _unavailableReason =
              event.message ??
              'This conversation is not running on the computer.';
          _loading = false;
          _loadingOlder = false;
        });
      case ConversationEventKind.error:
        setState(() {
          _unavailableReason =
              event.message ?? 'Could not load this conversation.';
          _loading = false;
          _loadingOlder = false;
        });
    }
  }

  Future<void> _loadSkills() async {
    try {
      final skills = await runtimeService.listSkills(widget.session.id);
      if (!mounted) return;
      setState(() => _skills = skills.supported ? skills.skills : null);
    } catch (_) {
      // 问不到就不画入口：技能只是辅助信息，不该为它打扰用户。
    }
  }

  Future<void> _restoreDraft() async {
    final draft = await widget.messageDraftStore.load(widget.session.id);
    if (!mounted) return;
    _restoringDraft = true;
    if (draft != null) {
      _messageController.text = draft.content;
      _pendingImages
        ..clear()
        ..addAll(draft.images);
      // 草稿上还挂着请求 id：上次发出去没等到确认（可能 App 被杀了）。
      if (draft.requestId case final requestId?) {
        _unconfirmedRequestId = requestId;
        _sendError = _unconfirmedDeliveryHint;
      }
    }
    _draftText = _messageController.text;
    _restoringDraft = false;
    setState(() {});
  }

  void _handleMessageChanged() {
    if (!mounted || _restoringDraft) return;
    if (_messageController.text == _draftText) return;
    _draftText = _messageController.text;
    _markDraftEdited();
  }

  /// 草稿被改过：之前的错误提示作废，没确认的请求 id 也不再适用。
  void _markDraftEdited() {
    setState(() {
      _sendError = null;
      _unconfirmedRequestId = null;
    });
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
        images: List<ImageInput>.of(_pendingImages),
        requestId: requestId ?? _unconfirmedRequestId,
      ),
    );
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
    if (_view?.hasMoreBefore != true) return;
    final position = _scrollController.position;
    if (position.maxScrollExtent - position.pixels > 240) return;
    setState(() => _loadingOlder = true);
    // 翻到的页从订阅流推回来；没翻（已在翻、前面没了）就把加载圈收掉。
    runtimeService
        .loadOlder(widget.session.id)
        .then(
          (started) {
            if (!started && mounted) setState(() => _loadingOlder = false);
          },
          onError: (Object _) {
            if (mounted) setState(() => _loadingOlder = false);
          },
        );
  }

  void _syncViewControls() {
    final activePermission = _view?.pendingPermissions.firstOrNull;
    if (activePermission?.toolCallId != _permissionSubmittingToolId) {
      _permissionSubmittingToolId = null;
    }
    if (_view?.pendingElicitation == null) _elicitationTextValues.clear();
  }

  void _retrySubscribe() {
    setState(() {
      _unavailableReason = null;
      _loading = _view == null;
    });
    _watch.restart();
  }

  void _showError(Object error) {
    if (!mounted) return;
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(SnackBar(content: Text(describeRuntimeError(error))));
  }

  /// 不需要结果的动作（选模型、点选项……）：成功的效果从视图流回来，失败才提示。
  void _fire(Future<void> request) {
    request.catchError(_showError);
  }

  Widget _buildUnavailable(String reason) {
    final colors = Theme.of(context).colorScheme;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.cloud_off_outlined, color: colors.onSurfaceVariant),
            const SizedBox(height: 12),
            Text(
              reason,
              textAlign: TextAlign.center,
              style: TextStyle(color: colors.onSurfaceVariant),
            ),
            const SizedBox(height: 12),
            FilledButton.tonal(
              onPressed: _retrySubscribe,
              child: const Text('Try again'),
            ),
          ],
        ),
      ),
    );
  }

  Widget _buildUnavailableBanner(String reason) {
    final colors = Theme.of(context).colorScheme;
    return Material(
      color: colors.surfaceContainerHighest,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 4, 4, 4),
        child: Row(
          children: [
            Icon(
              Icons.cloud_off_outlined,
              size: 18,
              color: colors.onSurfaceVariant,
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                reason,
                style: TextStyle(color: colors.onSurfaceVariant),
              ),
            ),
            TextButton(onPressed: _retrySubscribe, child: const Text('Retry')),
          ],
        ),
      ),
    );
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
          if (_view != null)
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
              cached: _cached,
              cachedAt: cachedAtFromMs(_savedAtMs),
              onRetry: _connectionState == RuntimeConnectionState.disconnected
                  ? () => unawaited(runtimeService.retry())
                  : null,
            ),
            if (_unavailableReason case final reason? when _view != null)
              _buildUnavailableBanner(reason),
            if (_view case final view?) _buildSessionStatus(view),
            if (_view?.plan case final plan? when plan.isNotEmpty)
              _buildPlanPanel(plan),
            if (_view?.pendingPermissions case final permissions?
                when permissions.isNotEmpty)
              _buildPermissionBanner(permissions.first, permissions.length),
            if (_view?.pendingElicitation case final elicitation?)
              _buildElicitationCard(elicitation),

            Expanded(
              child: _loading
                  ? const Center(child: CircularProgressIndicator())
                  : _unavailableReason != null && _view == null
                  ? _buildUnavailable(_unavailableReason!)
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
    final view = _view!;
    final phase = view.phase;
    // 图标 + 颜色是这里唯一的信息载体，读屏和色盲都拿不到。补一句文字标签。
    final label = switch (phase) {
      ConversationPhase.connecting => 'Starting',
      ConversationPhase.thinking => 'Thinking',
      ConversationPhase.executingTool => 'Running a tool',
      ConversationPhase.awaitingApproval => 'Waiting for your approval',
      ConversationPhase.waitingForUser => 'Waiting for your choice',
      ConversationPhase.succeeded => 'Completed',
      ConversationPhase.failed => 'Failed',
      ConversationPhase.idle => 'Idle',
      ConversationPhase.dead =>
        view.endReason.isEmpty ? 'Ended' : 'Ended: ${view.endReason}',
    };
    return Semantics(label: label, child: _phaseIcon(view));
  }

  Widget _phaseIcon(ConversationView view) {
    final status = context.smeltColors;
    return switch (view.phase) {
      ConversationPhase.connecting ||
      ConversationPhase.thinking ||
      ConversationPhase.executingTool => const SizedBox(
        width: 20,
        height: 20,
        child: CircularProgressIndicator(strokeWidth: 2),
      ),
      ConversationPhase.awaitingApproval => Icon(
        Icons.warning_amber,
        color: status.waitingApproval,
      ),
      ConversationPhase.waitingForUser => Icon(
        Icons.help_outline,
        color: status.needsAttention,
      ),
      ConversationPhase.succeeded => Icon(
        Icons.check_circle,
        color: status.idle,
      ),
      ConversationPhase.failed || ConversationPhase.dead => Tooltip(
        message: view.endReason,
        child: Icon(Icons.stop_circle, color: status.danger),
      ),
      ConversationPhase.idle => Icon(Icons.pause_circle, color: status.idle),
    };
  }

  Widget _buildSessionStatus(ConversationView view) {
    final colors = Theme.of(context).colorScheme;
    // 运行色用 running token 而不是 primary：指挥台的运行点是蓝的，这里再用蓝紫，
    // 同一个会话换个页面就换个颜色——跟当初 chip 红、指示器橙那处漂移是一回事。
    final running = context.smeltColors.running;
    final phase = view.phase;
    if (phase == ConversationPhase.idle ||
        phase == ConversationPhase.awaitingApproval ||
        phase == ConversationPhase.waitingForUser) {
      return const SizedBox.shrink();
    }
    final (icon, label, color) = switch (phase) {
      ConversationPhase.connecting => (
        Icons.rocket_launch_outlined,
        view.statusLine ?? 'Starting agent...',
        running,
      ),
      ConversationPhase.thinking || ConversationPhase.executingTool => (
        Icons.auto_awesome,
        view.statusLine ?? 'Agent is working',
        running,
      ),
      ConversationPhase.succeeded => (
        Icons.check_circle_outline,
        view.statusLine ?? 'Completed',
        running,
      ),
      ConversationPhase.failed => (
        Icons.error_outline,
        view.endReason.isEmpty ? 'Failed' : view.endReason,
        colors.error,
      ),
      ConversationPhase.dead => (
        Icons.error_outline,
        view.endReason.isEmpty ? 'Session ended' : view.endReason,
        colors.error,
      ),
      _ => (Icons.info_outline, '', colors.onSurfaceVariant),
    };
    final working = phase.isWorking;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      // 设计稿是 12%；原来的 7% 在深色底上几乎看不出这是一条独立的带子。
      color: color.withAlpha(31),
      child: Row(
        children: [
          if (working || phase == ConversationPhase.connecting)
            SizedBox(
              width: 16,
              height: 16,
              child: CircularProgressIndicator(strokeWidth: 2, color: color),
            )
          else
            Icon(icon, size: 18, color: color),
          const SizedBox(width: 8),
          Expanded(
            child: working && view.turnStartedAtMs != null
                ? TurnElapsedLabel(
                    label: label,
                    startedAtMs: view.turnStartedAtMs!,
                    color: color,
                  )
                : Text(
                    label,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(color: color, fontSize: 12),
                  ),
          ),
          if (working)
            IconButton(
              tooltip: 'Stop current turn',
              style: IconButton.styleFrom(
                foregroundColor: context.smeltColors.danger,
                minimumSize: const Size(40, 40),
              ),
              onPressed: () =>
                  _fire(runtimeService.cancelTurn(widget.session.id)),
              icon: const Icon(Icons.stop_circle_outlined),
            ),
        ],
      ),
    );
  }

  Widget _buildPlanPanel(List<PlanStep> plan) {
    final completed = plan
        .where((step) => step.status == PlanStepStatus.completed)
        .length;
    return ExpansionTile(
      dense: true,
      initiallyExpanded: plan.any(
        (step) => step.status == PlanStepStatus.inProgress,
      ),
      leading: const Icon(Icons.checklist, size: 19),
      title: Text('Plan · $completed/${plan.length}'),
      shape: const Border(bottom: BorderSide(color: Colors.transparent)),
      collapsedShape: const Border(
        bottom: BorderSide(color: Colors.transparent),
      ),
      children: plan.map((step) {
        final done = step.status == PlanStepStatus.completed;
        final (icon, color) = switch (step.status) {
          PlanStepStatus.completed => (
            Icons.check_circle,
            context.smeltColors.done,
          ),
          PlanStepStatus.inProgress => (
            Icons.radio_button_checked,
            Theme.of(context).colorScheme.primary,
          ),
          PlanStepStatus.pending => (
            Icons.radio_button_unchecked,
            Theme.of(context).colorScheme.onSurfaceVariant,
          ),
        };
        return ListTile(
          dense: true,
          visualDensity: VisualDensity.compact,
          leading: Icon(icon, size: 17, color: color),
          title: Text(
            step.content,
            style: TextStyle(
              fontSize: 13,
              decoration: done ? TextDecoration.lineThrough : null,
            ),
          ),
        );
      }).toList(),
    );
  }

  Widget _buildPermissionBanner(
    PermissionRequest permission,
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

  Widget _buildElicitationCard(Elicitation elicitation) {
    return ElicitationCard(
      elicitation: elicitation,
      textValues: _elicitationTextValues,
      // 要重建：必填文字框填没填决定 Submit 能不能点。
      onTextChanged: (index, value) =>
          setState(() => _elicitationTextValues[index] = value),
      onChoose: (fieldIndex, optionIndex) => _fire(
        runtimeService.chooseElicitation(
          widget.session.id,
          fieldIndex,
          optionIndex,
        ),
      ),
      onSubmit: _submitElicitation,
      onDismiss: () =>
          _fire(runtimeService.dismissElicitation(widget.session.id)),
    );
  }

  void _submitElicitation() {
    _fire(
      runtimeService.submitElicitation(widget.session.id, [
        for (final entry in _elicitationTextValues.entries)
          ElicitationTextInput(fieldIndex: entry.key, value: entry.value),
      ]),
    );
  }

  Widget _buildEntryList() {
    final view = _view;
    final entries = view?.entries ?? const <ConversationEntry>[];
    if (view == null || entries.isEmpty) {
      return const Center(child: Text('No messages yet'));
    }

    return ListView.builder(
      controller: _scrollController,
      reverse: true,
      keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
      itemCount: entries.length + (view.hasMoreBefore ? 1 : 0),
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
        return _buildEntry(view, entryIndex, entries[entryIndex]);
      },
    );
  }

  Widget _buildEntry(
    ConversationView view,
    int index,
    ConversationEntry entry,
  ) {
    final isFinal = view.finalAnswerIndex == index;
    final toolCall = entry.toolCall;
    return switch (entry.kind) {
      EntryKind.user => _buildUserMessage(entry.text, images: entry.images),
      EntryKind.interrupted => _buildDivider('Interrupted'),
      EntryKind.assistant => AcpAssistantMessage(
        text: entry.text,
        thought: entry.thought,
        isFinal: !entry.thought && isFinal,
        durationMs: !entry.thought && isFinal ? view.lastTurnDurationMs : null,
      ),
      EntryKind.completion => AcpCompletionMessage(
        summary: entry.text,
        status: toolCall?.status ?? ToolStatus.completed,
        isFinal: isFinal,
      ),
      EntryKind.toolCall when toolCall != null => AcpToolCallCard(
        title: toolCall.title,
        kind: toolCall.kind,
        status: toolCall.status,
        output: toolCall.output,
        loadImage: _loadImage,
      ),
      EntryKind.toolCall => const SizedBox.shrink(),
      EntryKind.divider => _buildDivider(entry.text),
      EntryKind.taskNote => _buildTaskNote(entry),
    };
  }

  Future<ImageData> _loadImage(ImageRef image, {int? maxEdge}) =>
      runtimeService.loadImage(widget.session.id, image, maxEdge: maxEdge);

  Widget _buildUserMessage(
    String text, {
    List<ConversationImage> images = const [],
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
                    .map(
                      (image) => AcpImageThumbnail(
                        image: image,
                        loadImage: _loadImage,
                      ),
                    )
                    .toList(),
              ),
            ],
          ],
        ),
      ),
    );
  }

  /// 后台任务 / 子智能体结束的说明。一行说明由 Rust 拼好。
  Widget _buildTaskNote(ConversationEntry entry) {
    final muted = Theme.of(context).colorScheme.onSurfaceVariant;
    final outputTail = entry.taskNote?.outputTail ?? '';
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 6),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text(entry.text, style: TextStyle(color: muted, fontSize: 12)),
          if (outputTail.trim().isNotEmpty)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: SelectableText(
                outputTail,
                style: TextStyle(color: muted, fontSize: 12),
              ),
            ),
        ],
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
    final view = _view;
    final connected = _connectionState == RuntimeConnectionState.connected;
    final acceptsPrompt = view?.phase.acceptsPrompt ?? false;
    final canCompose = connected && !_sendingMessage;
    final hasContent =
        _messageController.text.trim().isNotEmpty || _pendingImages.isNotEmpty;
    final canSend = view != null && acceptsPrompt && canCompose && hasContent;

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
          if (view != null) _buildComposerMetadata(view),
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
                tooltip: view?.supportsImage == false
                    ? 'This agent does not support images'
                    : 'Attach images',
                onPressed: canCompose && (view?.supportsImage ?? false)
                    ? _pickImages
                    : null,
                icon: const Icon(Icons.add_photo_alternate_outlined),
              ),
              if (view != null && view.availableCommands.isNotEmpty)
                PopupMenuButton<SlashCommand>(
                  tooltip: 'Insert command',
                  icon: const Icon(Icons.terminal),
                  onSelected: (command) {
                    _messageController.text = '/${command.name} ';
                    _messageController.selection = TextSelection.collapsed(
                      offset: _messageController.text.length,
                    );
                    _messageFocusNode.requestFocus();
                    setState(() {});
                  },
                  itemBuilder: (context) => view.availableCommands
                      .map(
                        (command) => PopupMenuItem(
                          value: command,
                          child: ListTile(
                            contentPadding: EdgeInsets.zero,
                            title: Text('/${command.name}'),
                            subtitle: command.description.isNotEmpty
                                ? Text(command.description)
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
                    hintText: !connected
                        ? 'Reconnect to send a message'
                        : _sendingMessage
                        ? 'Waiting for desktop confirmation...'
                        : view == null
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

  void _setConfig(String configId, String valueId, {bool? boolean}) => _fire(
    runtimeService.setConfigOption(
      widget.session.id,
      configId,
      valueId,
      boolean: boolean,
    ),
  );

  Widget _buildComposerMetadata(ConversationView view) {
    final items = <Widget>[];
    if (view.usage case final usage? when usage.contextWindow > 0) {
      final percent = (usage.usedTokens / usage.contextWindow * 100)
          .clamp(0, 100)
          .round();
      items.add(Chip(label: Text('Context $percent%')));
    }
    if (view.model case final model? when model.currentName.isNotEmpty) {
      // 协议按 provider 分组时，先切 provider、再在该组内选模型（桌面输入栏同一套）。
      // 平铺协议没有分组，仍然只出一个模型入口。
      final providerIndex = model.currentProvider;
      final provider =
          providerIndex != null && providerIndex < model.providers.length
          ? model.providers[providerIndex]
          : null;
      if (provider != null) {
        items.add(
          PopupMenuButton<String>(
            tooltip: 'Switch provider',
            enabled: model.providers.length > 1,
            onSelected: (value) => _setConfig(model.configId, value),
            itemBuilder: (context) => [
              for (final group in model.providers)
                CheckedPopupMenuItem(
                  // 协议只接受模型值，没有「只换 provider」的写法；给不出目标模型
                  // 的空组只列出来，不假装能点。
                  value: group.switchValue,
                  enabled: group.switchValue != null,
                  checked: group.selected,
                  child: Text(group.name),
                ),
            ],
            child: Chip(
              avatar: const Icon(Icons.hub_outlined, size: 16),
              label: Text(provider.name),
            ),
          ),
        );
      }
      items.add(
        PopupMenuButton<String>(
          tooltip: 'Switch model',
          enabled: model.options.length > 1,
          onSelected: (value) => _setConfig(model.configId, value),
          itemBuilder: (context) => model.options
              .map(
                (option) => CheckedPopupMenuItem(
                  value: option.value,
                  checked: option.selected,
                  child: Text(option.name),
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
    for (final config in view.configOptions) {
      // 开关项：点一下就翻转，写回布尔值。
      if (config.boolean case final enabled?) {
        items.add(
          FilterChip(
            tooltip: config.description ?? config.name,
            label: Text(config.name),
            selected: enabled,
            onSelected: (next) =>
                _setConfig(config.configId, '', boolean: next),
          ),
        );
        continue;
      }
      if (config.options.length < 2) continue;
      items.add(
        PopupMenuButton<String>(
          tooltip: config.description ?? config.name,
          onSelected: (value) => _setConfig(config.configId, value),
          itemBuilder: (context) => config.options
              .map(
                (option) => CheckedPopupMenuItem(
                  value: option.value,
                  checked: option.selected,
                  child: Text(option.name),
                ),
              )
              .toList(),
          child: Chip(label: Text(config.currentName)),
        ),
      );
    }
    // 「技能 N」：只读，和桌面一样回答「这场对话到底装了什么」。手机不改技能，
    // 那是改磁盘上的文件，归电脑端。
    if (_skills case final skills?) {
      items.add(
        PopupMenuButton<void>(
          tooltip: 'Loaded skills',
          itemBuilder: (context) => skills.isEmpty
              ? [
                  const PopupMenuItem<void>(
                    enabled: false,
                    child: Text('这场对话没有加载技能'),
                  ),
                ]
              : [
                  for (final skill in skills)
                    PopupMenuItem<void>(
                      enabled: false,
                      child: ListTile(
                        contentPadding: EdgeInsets.zero,
                        title: Text(skill.name),
                        subtitle: skill.description.isEmpty
                            ? null
                            : Text(
                                skill.description,
                                maxLines: 2,
                                overflow: TextOverflow.ellipsis,
                              ),
                      ),
                    ),
                ],
          child: Chip(
            avatar: const Icon(Icons.auto_awesome_outlined, size: 16),
            label: Text('技能 ${skills.length}'),
          ),
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
              child: AcpImageThumbnail(
                image: ConversationImage(
                  mime: _pendingImages[index].mime,
                  bytes: _pendingImages[index].bytes,
                ),
              ),
            ),
            Positioned(
              top: -6,
              right: -6,
              child: IconButton.filled(
                tooltip: 'Remove image',
                style: IconButton.styleFrom(
                  backgroundColor: context.smeltColors.dangerFill,
                  foregroundColor: context.smeltColors.onDangerFill,
                  minimumSize: const Size(40, 40),
                ),
                onPressed: () {
                  _pendingImages.removeAt(index);
                  _markDraftEdited();
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
      final images = <ImageInput>[];
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
        images.add(ImageInput(mime: mimeType, bytes: bytes));
      }
      if (!mounted) return;
      _pendingImages.addAll(images);
      if (images.isNotEmpty) _markDraftEdited();
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

  /// 上一次可能已经送达时，重发前要用户点头。
  Future<bool> _confirmResend() async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Send again?'),
        content: const Text(
          'The last attempt may already have reached the agent. Check the '
          'conversation before sending it again.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Send again'),
          ),
        ],
      ),
    );
    return confirmed == true;
  }

  Future<void> _sendMessage() async {
    final text = _messageController.text.trim();
    if (_sendingMessage || (text.isEmpty && _pendingImages.isEmpty)) return;
    final unconfirmed = _unconfirmedRequestId;
    if (unconfirmed != null && !await _confirmResend()) return;
    if (!mounted) return;

    _draftSaveTimer?.cancel();
    // 没确认的那次沿用原 id：运行时按它去重，真到过的话不会再发一遍。
    final requestId = unconfirmed ?? runtimeService.newRequestId();
    final images = List<ImageInput>.of(_pendingImages);
    final sessionId = widget.session.id;
    final drafts = widget.messageDraftStore;
    setState(() {
      _sendingMessage = true;
      _sendError = null;
    });
    // 先把草稿连同请求 id 落盘：App 在等确认时被杀，下次打开还知道这条没确认。
    try {
      await _saveDraft(requestId: requestId);
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _sendingMessage = false;
        _sendError = 'Could not save the draft: $error';
      });
      return;
    }

    try {
      await runtimeService.sendMessage(sessionId, requestId, text, images);
    } on SendMessageError catch (error) {
      if (error.kind == SendFailureKind.rejected) {
        // 运行时明确没收下：草稿原样留着，下次是一次新的提交。
        await drafts.resolveRequest(sessionId, requestId, succeeded: false);
        if (!mounted) return;
        setState(() {
          _sendingMessage = false;
          _unconfirmedRequestId = null;
          _sendError = 'Message was not sent: ${error.message}';
        });
      } else {
        _markUnconfirmed(requestId);
      }
      return;
    } catch (_) {
      // 意料之外的失败说不清有没有送到，按「没确认」处理。
      _markUnconfirmed(requestId);
      return;
    }

    final resolved = await drafts.resolveRequest(
      sessionId,
      requestId,
      succeeded: true,
    );
    if (!mounted) return;
    _restoringDraft = true;
    setState(() {
      _sendingMessage = false;
      _unconfirmedRequestId = null;
      _sendError = null;
      if (resolved) {
        _messageController.clear();
        _draftText = '';
        _pendingImages.clear();
      }
    });
    _restoringDraft = false;
  }

  /// 可能已经送达：草稿连同请求 id 留着，重发要先确认。
  void _markUnconfirmed(String requestId) {
    if (!mounted) return;
    setState(() {
      _sendingMessage = false;
      _unconfirmedRequestId = requestId;
      _sendError = _unconfirmedDeliveryHint;
    });
  }

  void _respondApproval(String toolCallId, String optionId) {
    if (_permissionSubmittingToolId != null) return;
    // 触感回执由 ApprovalCard 统一负责，这里再打一次会变成双震。
    setState(() => _permissionSubmittingToolId = toolCallId);
    runtimeService
        .respondApproval(widget.session.id, toolCallId, optionId)
        .catchError((Object error) {
          if (!mounted) return;
          // 没送到就把按钮还给用户。
          setState(() => _permissionSubmittingToolId = null);
          _showError(error);
        });
  }

  @override
  void dispose() {
    _draftSaveTimer?.cancel();
    _connectionSubscription.cancel();
    _watchSubscription.cancel();
    _watch.close();
    _messageController.dispose();
    _messageFocusNode
      ..removeListener(_handleMessageFocus)
      ..dispose();
    _scrollController.dispose();
    super.dispose();
  }
}
