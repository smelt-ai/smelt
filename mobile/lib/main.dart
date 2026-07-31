import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:image_picker/image_picker.dart';
import 'package:url_launcher/url_launcher.dart';
import 'models/pairing_config.dart';
import 'pages/qr_scanner_page.dart';
import 'services/gateway_service.dart';
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
  gatewayService.irohTunnelOpener = (endpointId, relayUrl, relayToken) =>
      irohTunnelStart(
        endpointId: endpointId,
        relayUrl: relayUrl,
        relayToken: relayToken,
      );
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
  const SmeltApp({super.key, this.pairingStorage});

  final PairingStorage? pairingStorage;

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
      home: HomePage(pairingStorage: pairingStorage),
    );
  }
}

class HomePage extends StatefulWidget {
  const HomePage({super.key, this.pairingStorage});

  final PairingStorage? pairingStorage;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  WsState _connectionState = WsState.disconnected;
  List<SessionSummary> _sessions = [];
  late final StreamSubscription<WsState> _stateSubscription;
  late final StreamSubscription<List<SessionSummary>> _sessionsSubscription;
  late final StreamSubscription<LifecycleAttention> _attentionSubscription;
  late final StreamSubscription<String> _attentionResolvedSubscription;
  late final StreamSubscription<String> _errorSubscription;
  late final PairingStorage _pairingStorage;
  String? _shownAttentionSessionId;
  PairingConfig? _pendingPairing;
  bool _restoringPairing = true;
  bool _hasSavedPairing = false;
  bool _showPairingCode = false;

  final _pairingCodeController = TextEditingController();

  @override
  void initState() {
    super.initState();
    _pairingStorage = widget.pairingStorage ?? SecurePairingStorage();
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
    });
    _attentionSubscription = gatewayService.attentionStream.listen((item) {
      if (!mounted) return;
      final isCurrent = gatewayService.subscribedSessionId == item.sessionId;
      if (isCurrent && !item.requiresAction) return;
      final session = _sessions
          .where((candidate) => candidate.id == item.sessionId)
          .firstOrNull;
      final messenger = ScaffoldMessenger.of(context);
      messenger.hideCurrentSnackBar();
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
          if (!mounted || _shownAttentionSessionId != sessionId) return;
          _shownAttentionSessionId = null;
          ScaffoldMessenger.of(context).hideCurrentSnackBar();
        });
    _errorSubscription = gatewayService.errorStream.listen((error) {
      if (!mounted) return;
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(error)));
    });
    unawaited(_restorePairing());
  }

  Future<void> _restorePairing() async {
    try {
      final pairing = await _pairingStorage.load();
      if (!mounted) return;
      setState(() {
        _restoringPairing = false;
        _hasSavedPairing = pairing != null;
      });
      if (pairing != null) _connect(pairing, saveWhenConnected: false);
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
      await _pairingStorage.save(pairing);
      if (mounted) setState(() => _hasSavedPairing = true);
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
          IconButton(
            icon: const Icon(Icons.qr_code_scanner),
            onPressed: _scanQrCode,
            tooltip: 'Pair with Desktop',
          ),
          if (_connectionState != WsState.disconnected)
            IconButton(
              icon: const Icon(Icons.logout),
              onPressed: () => gatewayService.disconnect(),
              tooltip: 'Disconnect',
            ),
        ],
      ),
      body: _buildBody(),
      floatingActionButton: _connectionState == WsState.connected
          ? FloatingActionButton(
              onPressed: () => gatewayService.listSessions(),
              child: const Icon(Icons.refresh),
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
        return _buildDisconnectedView();
      case WsState.connecting:
        return _buildConnectingView();
      case WsState.connected:
        return _buildSessionList();
      case WsState.reconnecting:
        return _buildReconnectingView();
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
          if (_hasSavedPairing) ...[
            const SizedBox(height: 12),
            TextButton.icon(
              onPressed: _forgetPairing,
              icon: const Icon(Icons.delete_outline),
              label: const Text('Forget saved pairing'),
            ),
          ],
        ],
      ),
    );
  }

  Widget _buildSessionList() {
    final orderedSessions = List<SessionSummary>.of(_sessions)
      ..sort(compareSessionMenuOrder);
    final projects = <String, List<SessionSummary>>{};
    for (final session in orderedSessions) {
      projects.putIfAbsent(_projectKey(session), () => []).add(session);
    }

    return Column(
      children: [
        const ConnectionStatusBar(),
        Expanded(
          child: _sessions.isEmpty
              ? const Center(child: Text('No active sessions'))
              : ListView(
                  children: projects.entries.map((entry) {
                    final sessions = entry.value;
                    final projectTitle = _projectName(sessions.first);
                    return ExpansionTile(
                      leading: const Icon(Icons.folder_outlined),
                      title: Text(projectTitle),
                      subtitle: Text(
                        '${sessions.length} agent${sessions.length == 1 ? '' : 's'}',
                      ),
                      children: sessions.map((session) {
                        final sessionTitle = session.title.trim();
                        final showTitle =
                            sessionTitle.isNotEmpty &&
                            sessionTitle != projectTitle;
                        return ListTile(
                          contentPadding: const EdgeInsets.only(
                            left: 32,
                            right: 16,
                          ),
                          leading: _getAgentIcon(session.agent),
                          title: Text(_agentLabel(session.agent)),
                          subtitle: Text(
                            session.detail?.trim().isNotEmpty == true
                                ? session.detail!
                                : (showTitle ? sessionTitle : session.phase),
                          ),
                          trailing: Badge(
                            isLabelVisible: session.unread,
                            child: _getStatusChip(session.status),
                          ),
                          onTap: () => _openSession(session),
                        );
                      }).toList(),
                    );
                  }).toList(),
                ),
        ),
      ],
    );
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

  String _agentLabel(String agent) {
    return switch (agent.toLowerCase()) {
      'claude' => 'Claude',
      'codex' => 'Codex',
      'copilot' => 'Copilot',
      'grok' => 'Grok',
      _ => 'Agent',
    };
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
            onPressed: gatewayService.disconnect,
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
            onPressed: gatewayService.disconnect,
            icon: const Icon(Icons.link_off),
            label: const Text("Change pairing"),
          ),
        ],
      ),
    );
  }

  Widget _getAgentIcon(String agent) {
    final (color, letter) = switch (agent.toLowerCase()) {
      'claude' => (Colors.orange, 'C'),
      'codex' => (Colors.green, 'X'),
      'copilot' => (Colors.blue, 'P'),
      'grok' => (Colors.purple, 'G'),
      _ => (Colors.grey, '?'),
    };
    return CircleAvatar(backgroundColor: color, child: Text(letter));
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
    _pendingPairing = saveWhenConnected ? pairing : null;
    gatewayService.connect(pairing.endpoint, pairing.token);
    // 重扫同一台桌面时 connect() 是幂等空操作，不会再有 connected 状态变化来
    // 触发落盘，这里补一次。
    if (saveWhenConnected && gatewayService.state == WsState.connected) {
      unawaited(_savePendingPairing());
    }
  }

  Future<void> _forgetPairing() async {
    try {
      await _pairingStorage.clear();
      if (!mounted) return;
      setState(() {
        _hasSavedPairing = false;
        _pendingPairing = null;
        _pairingCodeController.clear();
      });
    } catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not remove pairing: $error')),
      );
    }
  }

  void _openSession(SessionSummary session) {
    gatewayService.markRead(session.id);
    Navigator.push(
      context,
      MaterialPageRoute(builder: (context) => SessionPage(session: session)),
    );
  }

  @override
  void dispose() {
    _stateSubscription.cancel();
    _sessionsSubscription.cancel();
    _attentionSubscription.cancel();
    _attentionResolvedSubscription.cancel();
    _errorSubscription.cancel();
    _pairingCodeController.dispose();
    super.dispose();
  }
}

class SessionPage extends StatefulWidget {
  final SessionSummary session;

  const SessionPage({super.key, required this.session});

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
  String? _permissionSubmittingToolId;
  final List<AcpImageData> _pendingImages = [];
  final Map<int, String> _elicitationTextValues = {};
  late final StreamSubscription<AcpSnapshot> _snapshotSubscription;
  late final StreamSubscription<String> _attentionResolvedSubscription;

  @override
  void initState() {
    super.initState();
    _snapshot = gatewayService.cachedSnapshot(widget.session.id);
    _loading = _snapshot == null;
    _syncSnapshotControls();
    _messageFocusNode.addListener(_handleMessageFocus);
    _scrollController.addListener(_handleScrollPosition);
    _attentionResolvedSubscription = gatewayService.attentionResolvedStream
        .listen((sessionId) {
          if (!mounted || sessionId != widget.session.id) return;
          // 重新挂载 watcher 获取完整权威快照，避免本地根据 phase 猜哪张卡已解决。
          gatewayService.subscribe(widget.session.id);
        });
    _subscribeSession();
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
          const ConnectionStatusBar(),
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
    final ready = elicitation.fields.asMap().entries.every((entry) {
      return switch (entry.value.kind) {
        ElicitationSelect() || ElicitationMultiSelect() =>
          elicitation.chosen[entry.key]?.isNotEmpty == true,
        ElicitationText() =>
          _elicitationTextValues[entry.key]?.trim().isNotEmpty == true,
        ElicitationExternalUrl() => true,
      };
    });
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
    final canCompose = gatewayService.writeEnabled;
    final hasContent =
        _messageController.text.trim().isNotEmpty || _pendingImages.isNotEmpty;
    final canSend =
        hasSnapshot &&
        acceptsPrompt &&
        gatewayService.writeEnabled &&
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
                        ? 'Desktop connection is read-only'
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
                  onChanged: (_) => setState(() {}),
                ),
              ),
              const SizedBox(width: 4),
              IconButton.filled(
                tooltip: 'Send message',
                onPressed: canSend ? _sendMessage : null,
                icon: const Icon(Icons.send),
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
                onPressed: () => setState(() => _pendingImages.removeAt(index)),
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

  void _sendMessage() {
    final text = _messageController.text.trim();
    if (text.isEmpty && _pendingImages.isEmpty) return;

    _messageController.clear();
    final images = List<AcpImageData>.of(_pendingImages);
    setState(_pendingImages.clear);
    gatewayService.sendMessage(widget.session.id, text, images: images);
  }

  void _respondApproval(String toolCallId, String optionKey) {
    if (_permissionSubmittingToolId != null) return;
    setState(() => _permissionSubmittingToolId = toolCallId);
    gatewayService.respondApproval(widget.session.id, toolCallId, optionKey);
  }

  @override
  void dispose() {
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
