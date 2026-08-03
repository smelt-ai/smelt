import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:image_picker/image_picker.dart';
import 'package:url_launcher/url_launcher.dart';
import 'models/pairing_config.dart';
import 'models/saved_desktop.dart';
import 'pages/qr_scanner_page.dart';
import 'services/gateway_service.dart';
import 'services/message_draft_store.dart';
import 'models/acp_snapshot.dart';
import 'services/pairing_storage.dart';
import 'rust_lib.dart';
import 'src/rust/api_iroh.dart';
import 'utils/image_processing.dart';
import 'widgets/acp_content.dart';

bool isNearMessageBottom(
  double pixels,
  double minScrollExtent, {
  double tolerance = 48,
}) => (pixels - minScrollExtent).abs() <= tolerance;

bool shouldAutoFollowSnapshot({
  required bool initialLoad,
  required bool wasAtBottom,
}) => initialLoad || wasAtBottom;

String sessionListTitle(SessionSummary session) {
  final title = session.title.trim();
  return title.isEmpty ? 'ACP conversation' : title;
}

String? sessionListSubtitle(SessionSummary session) {
  final detail = session.detail?.trim();
  return detail == null || detail.isEmpty ? null : detail;
}

enum SessionListFilter { attention, running, all }

bool sessionNeedsAction(SessionSummary session) {
  if (session.attention?.requiresAction == true) return true;
  return switch (session.status.toLowerCase()) {
    'waiting_approval' || 'needs_attention' => true,
    _ => false,
  };
}

bool sessionIsRunning(SessionSummary session) {
  if (session.status.toLowerCase() == 'running') return true;
  return switch (session.phase.toLowerCase()) {
    'starting' || 'running' => true,
    _ => false,
  };
}

List<SessionSummary> filterSessions(
  Iterable<SessionSummary> sessions,
  SessionListFilter filter,
) => switch (filter) {
  SessionListFilter.attention => sessions.where(sessionNeedsAction).toList(),
  SessionListFilter.running => sessions.where(sessionIsRunning).toList(),
  SessionListFilter.all => sessions.toList(),
};

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

class SessionFilterBar extends StatelessWidget {
  const SessionFilterBar({
    super.key,
    required this.selected,
    required this.attentionCount,
    required this.runningCount,
    required this.allCount,
    required this.onChanged,
  });

  final SessionListFilter selected;
  final int attentionCount;
  final int runningCount;
  final int allCount;
  final ValueChanged<SessionListFilter> onChanged;

  String _count(int value) => value > 99 ? '99+' : '$value';

  Widget _label(String value) {
    return FittedBox(
      fit: BoxFit.scaleDown,
      child: Text(value, maxLines: 1, softWrap: false),
    );
  }

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        final textScale = MediaQuery.textScalerOf(context).scale(1);
        final iconWidthThreshold = 390 * textScale.clamp(1, 1.4);
        final showIcons = constraints.maxWidth >= iconWidthThreshold;
        return SizedBox(
          width: double.infinity,
          child: SegmentedButton<SessionListFilter>(
            showSelectedIcon: false,
            expandedInsets: EdgeInsets.zero,
            style: SegmentedButton.styleFrom(
              padding: const EdgeInsets.symmetric(horizontal: 8),
              visualDensity: VisualDensity.compact,
            ),
            segments: [
              ButtonSegment(
                value: SessionListFilter.attention,
                icon: showIcons
                    ? const Icon(Icons.priority_high, size: 17)
                    : null,
                label: _label('Action ${_count(attentionCount)}'),
              ),
              ButtonSegment(
                value: SessionListFilter.running,
                icon: showIcons ? const Icon(Icons.autorenew, size: 17) : null,
                label: _label('Running ${_count(runningCount)}'),
              ),
              ButtonSegment(
                value: SessionListFilter.all,
                icon: showIcons
                    ? const Icon(Icons.forum_outlined, size: 17)
                    : null,
                label: _label('All ${_count(allCount)}'),
              ),
            ],
            selected: {selected},
            onSelectionChanged: (selection) => onChanged(selection.single),
          ),
        );
      },
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

class ConnectionStatusBar extends StatelessWidget {
  const ConnectionStatusBar({super.key});

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<ConnectionMetrics>(
      stream: gatewayService.metricsStream,
      initialData: gatewayService.metrics,
      builder: (context, snapshot) {
        final metrics = snapshot.data ?? const ConnectionMetrics();
        final (icon, label) = switch (metrics.kind) {
          ConnectionPathKind.lan => (Icons.lan_outlined, 'LAN'),
          ConnectionPathKind.p2p => (Icons.swap_horiz, 'P2P'),
          ConnectionPathKind.relay => (Icons.cloud_outlined, 'Relay'),
          ConnectionPathKind.direct => (Icons.public, 'Direct'),
          ConnectionPathKind.unknown => (
            Icons.route_outlined,
            'Detecting path',
          ),
        };
        final latency = metrics.latencyMs == null
            ? '--'
            : '${metrics.latencyMs} ms';
        final colors = Theme.of(context).colorScheme;
        return Container(
          width: double.infinity,
          height: 34,
          padding: const EdgeInsets.symmetric(horizontal: 16),
          color: colors.surfaceContainer,
          child: Row(
            children: [
              Icon(icon, size: 16, color: colors.onSurfaceVariant),
              const SizedBox(width: 7),
              Text(
                '$label · $latency',
                style: TextStyle(color: colors.onSurfaceVariant, fontSize: 12),
              ),
            ],
          ),
        );
      },
    );
  }
}

class CachedConnectionBar extends StatelessWidget {
  const CachedConnectionBar({super.key, required this.state, this.cachedAt});

  final WsState state;
  final DateTime? cachedAt;

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
    return Container(
      width: double.infinity,
      constraints: const BoxConstraints(minHeight: 36),
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
      color: colors.tertiaryContainer,
      child: Row(
        children: [
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

class SmeltApp extends StatelessWidget {
  const SmeltApp({super.key, this.pairingStorage, this.messageDraftStore});

  final PairingStorage? pairingStorage;
  final MessageDraftStore? messageDraftStore;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Smelt',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: Colors.deepPurple,
          brightness: Brightness.dark,
        ),
        useMaterial3: true,
      ),
      home: HomePage(
        pairingStorage: pairingStorage,
        messageDraftStore: messageDraftStore,
      ),
    );
  }
}

class HomePage extends StatefulWidget {
  const HomePage({super.key, this.pairingStorage, this.messageDraftStore});

  final PairingStorage? pairingStorage;
  final MessageDraftStore? messageDraftStore;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
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
  SessionListFilter _sessionFilter = SessionListFilter.all;
  bool _acceptConnectionNotifications = true;
  String? _pendingOpenSessionId;

  final _pairingCodeController = TextEditingController();

  @override
  void initState() {
    super.initState();
    _pairingStorage = widget.pairingStorage ?? SecurePairingStorage();
    _messageDraftStore = widget.messageDraftStore ?? FileMessageDraftStore();
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
      final isCurrent = gatewayService.subscribedSessionId == item.sessionId;
      if (isCurrent && !item.requiresAction) return;
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
    return Scaffold(
      appBar: AppBar(
        title: const Text('Smelt'),
        actions: [
          if (_savedDesktops.desktops.isNotEmpty)
            IconButton(
              icon: Badge.count(
                count: _savedDesktops.desktops.length,
                isLabelVisible: _savedDesktops.desktops.length > 1,
                child: const Icon(Icons.desktop_mac_outlined),
              ),
              onPressed: _showDesktopSwitcher,
              tooltip: 'Desktops',
            ),
          IconButton(
            icon: const Icon(Icons.qr_code_scanner),
            onPressed: _scanQrCode,
            tooltip: 'Pair with Desktop',
          ),
          if (_connectionState != WsState.disconnected)
            IconButton(
              icon: const Icon(Icons.logout),
              onPressed: _disconnect,
              tooltip: 'Disconnect',
            ),
        ],
      ),
      body: _buildBody(),
      floatingActionButton: _connectionState == WsState.connected
          ? FloatingActionButton(
              onPressed: () {
                gatewayService.listSessions();
                gatewayService.listWorkspace();
              },
              child: const Icon(Icons.refresh),
            )
          : _connectionState == WsState.disconnected && _sessions.isNotEmpty
          ? FloatingActionButton(
              tooltip: 'Retry connection',
              onPressed: gatewayService.retryCurrentConnection,
              child: const Icon(Icons.sync),
            )
          : null,
    );
  }

  Widget _buildBody() {
    if (_restoringPairing) {
      return const Center(child: CircularProgressIndicator());
    }
    switch (_connectionState) {
      case WsState.disconnected:
        return _sessions.isEmpty
            ? _buildDisconnectedView()
            : _buildSessionList();
      case WsState.connecting:
        return _sessions.isEmpty ? _buildConnectingView() : _buildSessionList();
      case WsState.connected:
        return _buildSessionList();
      case WsState.reconnecting:
        return _sessions.isEmpty
            ? _buildReconnectingView()
            : _buildSessionList();
    }
  }

  Widget _buildDisconnectedView() {
    return SingleChildScrollView(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          const Icon(Icons.link_off, size: 64, color: Colors.grey),
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
    final orderedSessions = List<SessionSummary>.of(_sessions)
      ..sort(compareSessionMenuOrder);
    final visibleSessions = filterSessions(orderedSessions, _sessionFilter);
    final attentionCount = orderedSessions.where(sessionNeedsAction).length;
    final runningCount = orderedSessions.where(sessionIsRunning).length;

    return Column(
      children: [
        if (_connectionState == WsState.connected &&
            !gatewayService.sessionsAreCached)
          const ConnectionStatusBar()
        else
          CachedConnectionBar(
            state: _connectionState,
            cachedAt: gatewayService.cachedAt,
          ),
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 10, 12, 8),
          child: SessionFilterBar(
            selected: _sessionFilter,
            attentionCount: attentionCount,
            runningCount: runningCount,
            allCount: orderedSessions.length,
            onChanged: (filter) {
              setState(() => _sessionFilter = filter);
            },
          ),
        ),
        Expanded(
          child: _sessionFilter == SessionListFilter.all
              ? _buildProjectSessionList(orderedSessions)
              : _buildFocusedSessionList(visibleSessions),
        ),
      ],
    );
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
      return _buildEmptySessions(
        icon: Icons.folder_off_outlined,
        title: 'No projects',
      );
    }
    return ListView(
      children: projects.entries.map((entry) {
        final project = entry.value.project;
        final sessions = entry.value.sessions;
        return ExpansionTile(
          controlAffinity: ListTileControlAffinity.leading,
          title: Text(project.title),
          subtitle: Text(
            '${sessions.length} conversation${sessions.length == 1 ? '' : 's'}',
          ),
          trailing: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              IconButton(
                icon: const Icon(Icons.history),
                tooltip: 'Conversation history',
                onPressed: _workspace.agents.isEmpty
                    ? null
                    : () => _openHistory(project),
              ),
              IconButton(
                icon: const Icon(Icons.add),
                tooltip: 'New conversation',
                onPressed:
                    !gatewayService.writeEnabled || _workspace.agents.isEmpty
                    ? null
                    : () => _createSession(project),
              ),
            ],
          ),
          children: sessions
              .map(
                (session) => _buildSessionTile(
                  session,
                  contentPadding: const EdgeInsets.only(left: 32, right: 16),
                  showActions: true,
                ),
              )
              .toList(),
        );
      }).toList(),
    );
  }

  Widget _buildFocusedSessionList(List<SessionSummary> sessions) {
    if (sessions.isEmpty) {
      return _buildEmptySessions(
        icon: _sessionFilter == SessionListFilter.attention
            ? Icons.check_circle_outline
            : Icons.pause_circle_outline,
        title: _sessionFilter == SessionListFilter.attention
            ? 'Nothing needs attention'
            : 'No conversations are running',
      );
    }
    return ListView.separated(
      itemCount: sessions.length,
      separatorBuilder: (_, _) => const Divider(height: 1, indent: 56),
      itemBuilder: (context, index) {
        final session = sessions[index];
        final project = _projectName(session);
        final detail = _sessionFilter == SessionListFilter.attention
            ? session.attention?.message.trim()
            : sessionListSubtitle(session);
        final subtitle = detail == null || detail.isEmpty
            ? project
            : '$project · $detail';
        return _buildSessionTile(
          session,
          subtitle: subtitle,
          showActions: true,
          attentionStyle: _sessionFilter == SessionListFilter.attention,
        );
      },
    );
  }

  Widget _buildSessionTile(
    SessionSummary session, {
    EdgeInsetsGeometry? contentPadding,
    String? subtitle,
    bool showActions = false,
    bool attentionStyle = false,
  }) {
    subtitle ??= sessionListSubtitle(session);
    return ListTile(
      contentPadding: contentPadding,
      leading: Icon(
        attentionStyle
            ? Icons.notification_important_outlined
            : Icons.chat_bubble_outline,
      ),
      title: Text(sessionListTitle(session)),
      subtitle: subtitle == null
          ? null
          : Text(subtitle, maxLines: 2, overflow: TextOverflow.ellipsis),
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Badge(
            isLabelVisible: session.unread,
            child: _getStatusChip(session.status),
          ),
          if (showActions)
            PopupMenuButton<String>(
              tooltip: 'Conversation actions',
              onSelected: (action) {
                if (action == 'delete') _deleteSession(session);
              },
              itemBuilder: (context) => [
                PopupMenuItem(
                  value: 'delete',
                  enabled: gatewayService.writeEnabled,
                  child: const ListTile(
                    contentPadding: EdgeInsets.zero,
                    leading: Icon(Icons.delete_outline),
                    title: Text('Delete'),
                  ),
                ),
              ],
            ),
        ],
      ),
      onTap: () => _openSession(session),
    );
  }

  Widget _buildEmptySessions({required IconData icon, required String title}) {
    final colors = Theme.of(context).colorScheme;
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 42, color: colors.onSurfaceVariant),
          const SizedBox(height: 12),
          Text(title, style: TextStyle(color: colors.onSurfaceVariant)),
        ],
      ),
    );
  }

  Future<AcpAgentOption?> _chooseAgent({String title = 'New conversation'}) {
    return showModalBottomSheet<AcpAgentOption>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: ConstrainedBox(
          constraints: BoxConstraints(
            maxHeight: MediaQuery.sizeOf(context).height * 0.7,
          ),
          child: ListView(
            shrinkWrap: true,
            children: [
              ListTile(title: Text(title)),
              ..._workspace.agents.map(
                (agent) => ListTile(
                  leading: const Icon(Icons.smart_toy_outlined),
                  title: Text(agent.label),
                  subtitle: agent.profile
                      ? const Text('Custom workspace')
                      : null,
                  onTap: () => Navigator.pop(context, agent),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Future<void> _createSession(WorkspaceProject project) async {
    final agent = await _chooseAgent();
    if (!mounted || agent == null) return;
    gatewayService.createSession(project.root, agent.id);
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
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Delete conversation?'),
        content: Text(
          'This stops ${sessionListTitle(session)} and removes it from the active list. The agent transcript remains available in History.',
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

  Widget _getStatusChip(String status) {
    final (color, label) = switch (status.toLowerCase()) {
      'waiting_approval' => (Colors.red, 'Approve'),
      'needs_attention' => (Colors.orange, 'Attention'),
      'running' => (Colors.blue, 'Running'),
      'done' => (Colors.green, 'Done'),
      _ => (Colors.grey, 'Idle'),
    };
    return Chip(
      label: Text(label, style: const TextStyle(fontSize: 12)),
      backgroundColor: color.withAlpha(50),
      side: BorderSide.none,
      padding: EdgeInsets.zero,
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

  void _openSession(SessionSummary session) {
    gatewayService.markRead(session.id);
    Navigator.push(
      context,
      MaterialPageRoute(
        builder: (context) => SessionPage(
          session: session,
          messageDraftStore: _messageDraftStore,
        ),
      ),
    );
  }

  @override
  void dispose() {
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
    _load();
  }

  void _load() {
    setState(() {
      _loading = true;
      _sessions = const [];
    });
    gatewayService.listSessionHistory(widget.project.root, _agent.id);
  }

  String _historySubtitle(HistorySessionSummary session) {
    final active = session.lastActiveAt?.toLocal();
    final date = active == null
        ? null
        : '${active.month.toString().padLeft(2, '0')}-${active.day.toString().padLeft(2, '0')} '
              '${active.hour.toString().padLeft(2, '0')}:${active.minute.toString().padLeft(2, '0')}';
    final messages =
        '${session.messageCount} message${session.messageCount == 1 ? '' : 's'}';
    return date == null ? messages : '$date · $messages';
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
                            session.title,
                            maxLines: 2,
                            overflow: TextOverflow.ellipsis,
                          ),
                          subtitle: Text(_historySubtitle(session)),
                          trailing: IconButton(
                            tooltip: 'Resume conversation',
                            icon: const Icon(Icons.play_arrow),
                            onPressed: gatewayService.writeEnabled
                                ? () => _resume(session)
                                : null,
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
    super.dispose();
  }
}

class SessionPage extends StatefulWidget {
  final SessionSummary session;
  final MessageDraftStore messageDraftStore;

  const SessionPage({
    super.key,
    required this.session,
    required this.messageDraftStore,
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
          if (_snapshot != null)
            Padding(
              padding: const EdgeInsets.only(right: 16),
              child: Center(child: _buildPhaseIndicator()),
            ),
        ],
      ),
      body: Column(
        children: [
          if (_connectionState == WsState.connected &&
              !gatewayService.snapshotIsCached(widget.session.id))
            const ConnectionStatusBar()
          else
            CachedConnectionBar(
              state: _connectionState,
              cachedAt: gatewayService.cachedAt,
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
                              icon: const Icon(Icons.arrow_downward, size: 18),
                              label: const Text('滚动到底部'),
                            ),
                          ),
                        ),
                    ],
                  ),
          ),
          _buildInputBar(),
        ],
      ),
    );
  }

  Widget _buildPhaseIndicator() {
    final phase = _snapshot!.phase;
    return switch (phase) {
      AcpPhaseIdle() => const Icon(Icons.pause_circle, color: Colors.grey),
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
      AcpPhaseAwaitingApproval() => const Icon(
        Icons.warning_amber,
        color: Colors.orange,
      ),
      AcpPhaseAwaitingChoice() => const Icon(
        Icons.help_outline,
        color: Colors.blue,
      ),
      AcpPhaseEnded(reason: final r) => Tooltip(
        message: r,
        child: const Icon(Icons.stop_circle, color: Colors.red),
      ),
    };
  }

  Widget _buildSessionStatus(AcpSnapshot snapshot) {
    final colors = Theme.of(context).colorScheme;
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
        colors.primary,
      ),
      AcpPhaseRunning() => (
        Icons.auto_awesome,
        snapshot.statusLine ?? 'Agent is working',
        colors.primary,
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
      color: color.withAlpha(18),
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
            ? (Icons.check_circle, Colors.green)
            : step.isInProgress
            ? (
                Icons.radio_button_checked,
                Theme.of(context).colorScheme.primary,
              )
            : (Icons.radio_button_unchecked, Colors.grey);
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
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.all(12),
      color: Colors.orange.withAlpha(40),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              const Icon(Icons.gpp_maybe_outlined, size: 19),
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
          const SizedBox(height: 8),
          _buildPermissionDetails(permission),
          const SizedBox(height: 8),
          if (_permissionSubmittingToolId == permission.toolCallId)
            const Row(
              children: [
                SizedBox(
                  width: 16,
                  height: 16,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
                SizedBox(width: 8),
                Text('Submitting...'),
              ],
            )
          else
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: permission.options.map((opt) {
                return opt.isAllow
                    ? FilledButton(
                        onPressed: () => _respondApproval(
                          permission.toolCallId,
                          opt.optionId,
                        ),
                        style: FilledButton.styleFrom(
                          backgroundColor: Colors.green.shade700,
                        ),
                        child: Text(opt.name),
                      )
                    : OutlinedButton(
                        onPressed: () => _respondApproval(
                          permission.toolCallId,
                          opt.optionId,
                        ),
                        style: opt.isReject
                            ? OutlinedButton.styleFrom(
                                foregroundColor: Colors.red.shade300,
                              )
                            : null,
                        child: Text(opt.name),
                      );
              }).toList(),
            ),
        ],
      ),
    );
  }

  Widget _buildPermissionDetails(PendingPermission permission) {
    final muted = Theme.of(context).colorScheme.onSurfaceVariant;
    return switch (permission.details) {
      ApprovalDetailsCommand(
        command: final command,
        cwd: final cwd,
        reason: final reason,
      ) =>
        Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            SelectableText(
              command,
              style: const TextStyle(fontFamily: 'monospace', fontSize: 13),
            ),
            if (reason?.isNotEmpty == true) Text(reason!),
            if (cwd?.isNotEmpty == true)
              Text(
                'Working directory: $cwd',
                style: TextStyle(color: muted, fontSize: 12),
              ),
          ],
        ),
      ApprovalDetailsFileChange(reason: final reason, grantRoot: final root) =>
        Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(reason?.isNotEmpty == true ? reason! : permission.question),
            if (root?.isNotEmpty == true)
              Text(
                'Authorized path: $root',
                style: TextStyle(color: muted, fontSize: 12),
              ),
          ],
        ),
      ApprovalDetailsPermissions(summary: final summary) => Text(summary),
      ApprovalDetailsGeneric() => Text(permission.question),
    };
  }

  Widget _buildElicitationCard(PendingElicitation elicitation) {
    final ready = elicitation.isReady(localTextValues: _elicitationTextValues);
    final singleSelect =
        elicitation.fields.length == 1 &&
        elicitation.fields.first.kind is ElicitationSelect;

    return Container(
      width: double.infinity,
      constraints: const BoxConstraints(maxHeight: 360),
      margin: const EdgeInsets.fromLTRB(12, 8, 12, 4),
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: Colors.amber.withAlpha(20),
        border: Border.all(color: Colors.amber.shade700),
        borderRadius: BorderRadius.circular(8),
      ),
      child: SingleChildScrollView(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                const Icon(Icons.help_outline, color: Colors.amber, size: 20),
                const SizedBox(width: 8),
                const Text(
                  'Your input is needed',
                  style: TextStyle(fontWeight: FontWeight.bold),
                ),
              ],
            ),
            if (elicitation.message.isNotEmpty) ...[
              const SizedBox(height: 6),
              Text(elicitation.message),
            ],
            const SizedBox(height: 10),
            ...elicitation.fields.asMap().entries.map(
              (entry) =>
                  _buildElicitationField(elicitation, entry.key, entry.value),
            ),
            if (!singleSelect)
              Row(
                children: [
                  FilledButton(
                    onPressed: ready ? _submitElicitation : null,
                    child: const Text('Submit'),
                  ),
                  const SizedBox(width: 8),
                  TextButton(
                    onPressed: () =>
                        gatewayService.dismissElicitation(widget.session.id),
                    child: const Text('Answer in text instead'),
                  ),
                ],
              ),
          ],
        ),
      ),
    );
  }

  Widget _buildElicitationField(
    PendingElicitation elicitation,
    int fieldIndex,
    ElicitationField field,
  ) {
    final title = Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: Text(field.title, style: const TextStyle(fontSize: 13)),
    );
    final input = switch (field.kind) {
      ElicitationSelect(options: final options) ||
      ElicitationMultiSelect(options: final options) => Wrap(
        spacing: 8,
        runSpacing: 6,
        children: options.asMap().entries.map((entry) {
          final selected =
              elicitation.chosen[fieldIndex]?.contains(entry.key) == true;
          return ChoiceChip(
            label: Text(entry.value.label),
            selected: selected,
            onSelected: (_) => gatewayService.chooseElicitation(
              widget.session.id,
              fieldIndex,
              entry.key,
            ),
          );
        }).toList(),
      ),
      ElicitationText(secret: final secret) => TextFormField(
        initialValue:
            _elicitationTextValues[fieldIndex] ??
            elicitation.textValues[fieldIndex] ??
            '',
        obscureText: secret,
        decoration: const InputDecoration(border: OutlineInputBorder()),
        onChanged: (value) => _elicitationTextValues[fieldIndex] = value,
      ),
      ElicitationExternalUrl(url: final url) => Row(
        children: [
          Expanded(child: SelectableText(url)),
          IconButton(
            tooltip: 'Open link',
            onPressed: () {
              final uri = Uri.tryParse(url);
              if (uri != null) {
                launchUrl(uri, mode: LaunchMode.externalApplication);
              }
            },
            icon: const Icon(Icons.open_in_new),
          ),
        ],
      ),
    };
    return Padding(
      padding: const EdgeInsets.only(bottom: 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [title, input],
      ),
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
              style: TextStyle(color: Colors.grey[500], fontSize: 12),
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
        color: Theme.of(context).colorScheme.surface,
        border: Border(top: BorderSide(color: Colors.grey[800]!)),
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
