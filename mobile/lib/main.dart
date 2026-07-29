import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'models/pairing_config.dart';
import 'pages/qr_scanner_page.dart';
import 'services/gateway_service.dart';
import 'models/acp_snapshot.dart';
import 'services/pairing_storage.dart';
import 'rust_lib.dart';
import 'src/rust/api_iroh.dart';

bool isNearMessageBottom(
  double pixels,
  double minScrollExtent, {
  double tolerance = 48,
}) => (pixels - minScrollExtent).abs() <= tolerance;

bool shouldAutoFollowSnapshot({
  required bool initialLoad,
  required bool wasAtBottom,
}) => initialLoad || wasAtBottom;

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await initRustLib();
  // 在组装根接线，而不是让 GatewayService 直接依赖 FFI：服务层保持纯 Dart，
  // 单测才能不带动态库地跑。
  gatewayService.irohTunnelOpener = (endpointId) =>
      irohTunnelStart(endpointId: endpointId);
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
  bool _showToken = false;

  final _endpointController = TextEditingController();
  final _tokenController = TextEditingController();

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

          // 临时手动连接 UI（后续改为 QR）
          TextField(
            controller: _endpointController,
            decoration: const InputDecoration(
              labelText: 'Gateway Endpoint',
              hintText: 'ws://192.168.1.x:9877',
              border: OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _tokenController,
            obscureText: !_showToken,
            enableSuggestions: false,
            autocorrect: false,
            decoration:
                const InputDecoration(
                  labelText: 'Token',
                  hintText: 'Enter token from desktop',
                  border: OutlineInputBorder(),
                ).copyWith(
                  suffixIcon: IconButton(
                    tooltip: _showToken ? 'Hide token' : 'Show token',
                    onPressed: () => setState(() => _showToken = !_showToken),
                    icon: Icon(
                      _showToken ? Icons.visibility_off : Icons.visibility,
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
    if (_sessions.isEmpty) {
      return const Center(child: Text('No active sessions'));
    }

    final orderedSessions = List<SessionSummary>.of(_sessions)
      ..sort(compareSessionMenuOrder);
    final projects = <String, List<SessionSummary>>{};
    for (final session in orderedSessions) {
      projects.putIfAbsent(_projectKey(session), () => []).add(session);
    }

    return ListView(
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
                sessionTitle.isNotEmpty && sessionTitle != projectTitle;
            return ListTile(
              contentPadding: const EdgeInsets.only(left: 32, right: 16),
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
          Text(
            'Connecting to ${_endpointController.text}...',
            textAlign: TextAlign.center,
          ),
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
    _endpointController.text = pairing.endpoint;
    _tokenController.text = pairing.token;
    _connect(pairing);
  }

  void _manualConnect() {
    try {
      final pairing = PairingConfig.fromFields(
        _endpointController.text,
        _tokenController.text,
      );
      _connect(pairing);
    } on FormatException catch (error) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(error.message.toString())));
    }
  }

  void _connect(PairingConfig pairing, {bool saveWhenConnected = true}) {
    _endpointController.text = pairing.endpoint;
    _tokenController.text = pairing.token;
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
        _endpointController.clear();
        _tokenController.clear();
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
    _endpointController.dispose();
    _tokenController.dispose();
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
  final ScrollController _scrollController = ScrollController();
  AcpSnapshot? _snapshot;
  bool _loading = true;
  bool _isAtBottom = true;
  final Map<int, String> _elicitationTextValues = {};
  late final StreamSubscription<AcpSnapshot> _snapshotSubscription;
  late final StreamSubscription<String> _attentionResolvedSubscription;

  @override
  void initState() {
    super.initState();
    _scrollController.addListener(_handleScrollPosition);
    _attentionResolvedSubscription = gatewayService.attentionResolvedStream
        .listen((sessionId) {
          if (!mounted || sessionId != widget.session.id) return;
          // 重新挂载 watcher 获取完整权威快照，避免本地根据 phase 猜哪张卡已解决。
          gatewayService.subscribe(widget.session.id);
        });
    _subscribeSession();
  }

  void _handleScrollPosition() {
    if (!_scrollController.hasClients) return;
    final position = _scrollController.position;
    final isAtBottom = isNearMessageBottom(
      position.pixels,
      position.minScrollExtent,
    );
    if (isAtBottom == _isAtBottom || !mounted) return;
    setState(() => _isAtBottom = isAtBottom);
  }

  void _subscribeSession() {
    _snapshotSubscription = gatewayService.snapshotStream.listen((snapshot) {
      if (!mounted || gatewayService.subscribedSessionId != widget.session.id) {
        return;
      }
      final initialLoad = _snapshot == null;
      final shouldFollowLatest = shouldAutoFollowSnapshot(
        initialLoad: initialLoad,
        wasAtBottom: _isAtBottom,
      );
      setState(() {
        _snapshot = _snapshot?.merge(snapshot) ?? snapshot;
        final elicitation = _snapshot?.pendingElicitation;
        if (elicitation == null) {
          _elicitationTextValues.clear();
        } else {
          for (final entry in elicitation.textValues.entries) {
            _elicitationTextValues.putIfAbsent(entry.key, () => entry.value);
          }
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
          // 权限请求横幅
          if (_snapshot?.pendingPermissions.isNotEmpty == true)
            ..._snapshot!.pendingPermissions.map(_buildPermissionBanner),
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

  Widget _buildPermissionBanner(PendingPermission permission) {
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.all(12),
      color: Colors.orange.withAlpha(40),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Text(
            'Permission Required',
            style: TextStyle(fontWeight: FontWeight.bold),
          ),
          if (permission.question.isNotEmpty) Text(permission.question),
          const SizedBox(height: 8),
          Wrap(
            spacing: 8,
            children: permission.options.map((opt) {
              return ElevatedButton(
                onPressed: () =>
                    _respondApproval(permission.toolCallId, opt.optionId),
                style: opt.isAllow
                    ? ElevatedButton.styleFrom(backgroundColor: Colors.green)
                    : null,
                child: Text(opt.name),
              );
            }).toList(),
          ),
        ],
      ),
    );
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
            tooltip: 'Copy link',
            onPressed: () {
              Clipboard.setData(ClipboardData(text: url));
              ScaffoldMessenger.of(
                context,
              ).showSnackBar(const SnackBar(content: Text('Link copied')));
            },
            icon: const Icon(Icons.copy),
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
      itemCount: entries.length,
      itemBuilder: (context, index) {
        return _buildEntry(entries[entries.length - 1 - index]);
      },
    );
  }

  Widget _buildEntry(AcpEntry entry) {
    return switch (entry) {
      AcpEntryUser(text: final text) => _buildUserMessage(text),
      AcpEntryAssistant(text: final text, thought: final thought) =>
        _buildAssistantMessage(text, thought),
      AcpEntryToolCall(
        id: _,
        title: final title,
        kind: final kind,
        status: final status,
        output: final output,
      ) =>
        _buildToolCall(title, kind, status, output),
      AcpEntryDivider(label: final label) => _buildDivider(label),
      AcpEntryUnknown() => const SizedBox.shrink(),
    };
  }

  Widget _buildUserMessage(String text) {
    return Align(
      alignment: Alignment.centerRight,
      child: Container(
        margin: const EdgeInsets.all(8),
        padding: const EdgeInsets.all(12),
        constraints: BoxConstraints(
          maxWidth: MediaQuery.of(context).size.width * 0.8,
        ),
        decoration: BoxDecoration(
          color: Colors.blue,
          borderRadius: BorderRadius.circular(12),
        ),
        child: Text(text, style: const TextStyle(color: Colors.white)),
      ),
    );
  }

  Widget _buildAssistantMessage(String text, bool thought) {
    if (text.isEmpty) return const SizedBox.shrink();

    return Align(
      alignment: Alignment.centerLeft,
      child: Container(
        margin: const EdgeInsets.all(8),
        padding: const EdgeInsets.all(12),
        constraints: BoxConstraints(
          maxWidth: MediaQuery.of(context).size.width * 0.8,
        ),
        decoration: BoxDecoration(
          color: thought ? Colors.grey[800] : Colors.grey[700],
          borderRadius: BorderRadius.circular(12),
        ),
        child: Text(
          text,
          style: TextStyle(
            color: Colors.white,
            fontStyle: thought ? FontStyle.italic : FontStyle.normal,
          ),
        ),
      ),
    );
  }

  Widget _buildToolCall(
    String title,
    ToolKind kind,
    ToolCallStatus status,
    List<ToolOutputPart> output,
  ) {
    final statusIcon = switch (status) {
      ToolCallStatus.pending || ToolCallStatus.inProgress => const SizedBox(
        width: 16,
        height: 16,
        child: CircularProgressIndicator(strokeWidth: 2),
      ),
      ToolCallStatus.completed => const Icon(
        Icons.check_circle,
        color: Colors.green,
        size: 16,
      ),
      ToolCallStatus.failed => const Icon(
        Icons.error,
        color: Colors.red,
        size: 16,
      ),
    };

    final kindIcon = switch (kind) {
      ToolKind.read => Icons.visibility,
      ToolKind.edit => Icons.edit,
      ToolKind.execute => Icons.terminal,
      ToolKind.other => Icons.build,
    };

    return Card(
      margin: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
      child: ExpansionTile(
        leading: Icon(kindIcon, size: 20),
        title: Text(title, style: const TextStyle(fontSize: 14)),
        trailing: statusIcon,
        children: output.isEmpty
            ? []
            : [
                Container(
                  width: double.infinity,
                  padding: const EdgeInsets.all(8),
                  color: Colors.black26,
                  child: Text(
                    output
                        .map(
                          (p) => switch (p) {
                            ToolOutputText(text: final t) => t,
                            ToolOutputDiff(path: final path) => '[Diff: $path]',
                            ToolOutputImage() => '[Image]',
                          },
                        )
                        .join('\n'),
                    style: const TextStyle(
                      fontFamily: 'monospace',
                      fontSize: 12,
                    ),
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
    final isIdle = _snapshot?.phase is AcpPhaseIdle;
    final canCompose = gatewayService.writeEnabled;
    final canSend = hasSnapshot && isIdle && gatewayService.writeEnabled;

    return Container(
      padding: const EdgeInsets.all(8),
      decoration: BoxDecoration(
        color: Theme.of(context).colorScheme.surface,
        border: Border(top: BorderSide(color: Colors.grey[800]!)),
      ),
      child: Row(
        children: [
          Expanded(
            child: TextField(
              controller: _messageController,
              enabled: canCompose,
              decoration: InputDecoration(
                hintText: !gatewayService.writeEnabled
                    ? 'Desktop connection is read-only'
                    : !hasSnapshot
                    ? 'Loading session...'
                    : isIdle
                    ? 'Send a message...'
                    : 'Agent is running - draft message...',
                border: const OutlineInputBorder(),
              ),
              onSubmitted: canSend ? (_) => _sendMessage() : null,
            ),
          ),
          const SizedBox(width: 8),
          IconButton(
            onPressed: canSend ? _sendMessage : null,
            icon: const Icon(Icons.send),
          ),
        ],
      ),
    );
  }

  void _sendMessage() {
    final text = _messageController.text.trim();
    if (text.isEmpty) return;

    _messageController.clear();
    gatewayService.sendMessage(widget.session.id, text);
  }

  void _respondApproval(String toolCallId, String optionKey) {
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
    _scrollController.dispose();
    super.dispose();
  }
}
