import 'package:flutter/material.dart';
import 'services/gateway_service.dart';
import 'models/acp_snapshot.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const SmeltApp());
}

class SmeltApp extends StatelessWidget {
  const SmeltApp({super.key});

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
      home: const HomePage(),
    );
  }
}

class HomePage extends StatefulWidget {
  const HomePage({super.key});

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  WsState _connectionState = WsState.disconnected;
  List<SessionSummary> _sessions = [];
  String? _errorMessage;
  
  // 临时配对信息（后续改为 QR 扫描）
  final _endpointController = TextEditingController(text: 'ws://192.168.1.100:9877');
  final _tokenController = TextEditingController();

  @override
  void initState() {
    super.initState();
    gatewayService.stateStream.listen((state) {
      setState(() => _connectionState = state);
    });
    gatewayService.sessionsStream.listen((sessions) {
      setState(() => _sessions = sessions);
    });
    gatewayService.errorStream.listen((error) {
      setState(() => _errorMessage = error);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(error)),
      );
    });
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
          if (_connectionState == WsState.connected)
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
    switch (_connectionState) {
      case WsState.disconnected:
        return _buildDisconnectedView();
      case WsState.connecting:
        return const Center(child: CircularProgressIndicator());
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
            decoration: const InputDecoration(
              labelText: 'Token',
              hintText: 'Enter token from desktop',
              border: OutlineInputBorder(),
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
        ],
      ),
    );
  }

  Widget _buildSessionList() {
    if (_sessions.isEmpty) {
      return const Center(
        child: Text('No active sessions'),
      );
    }

    return ListView.builder(
      itemCount: _sessions.length,
      itemBuilder: (context, index) {
        final session = _sessions[index];
        return ListTile(
          leading: _getAgentIcon(session.agent),
          title: Text(session.title.isNotEmpty ? session.title : session.id),
          subtitle: Text(session.cwd ?? session.phase),
          trailing: _getPhaseChip(session.phase),
          onTap: () => _openSession(session),
        );
      },
    );
  }

  Widget _buildReconnectingView() {
    return const Center(
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          CircularProgressIndicator(),
          SizedBox(height: 16),
          Text('Reconnecting...'),
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
    return CircleAvatar(
      backgroundColor: color,
      child: Text(letter),
    );
  }

  Widget _getPhaseChip(String phase) {
    final (color, label) = switch (phase.toLowerCase()) {
      'running' => (Colors.green, 'Running'),
      'idle' => (Colors.grey, 'Idle'),
      'ended' => (Colors.red, 'Ended'),
      _ => (Colors.grey, phase),
    };
    return Chip(
      label: Text(label, style: const TextStyle(fontSize: 12)),
      backgroundColor: color.withAlpha(50),
      side: BorderSide.none,
      padding: EdgeInsets.zero,
    );
  }

  void _scanQrCode() {
    ScaffoldMessenger.of(context).showSnackBar(
      const SnackBar(content: Text('QR scanning not yet implemented')),
    );
  }

  void _manualConnect() {
    final endpoint = _endpointController.text.trim();
    final token = _tokenController.text.trim();
    if (endpoint.isEmpty || token.isEmpty) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('Please enter endpoint and token')),
      );
      return;
    }
    gatewayService.connect(endpoint, token);
  }

  void _openSession(SessionSummary session) {
    Navigator.push(
      context,
      MaterialPageRoute(
        builder: (context) => SessionPage(session: session),
      ),
    );
  }
  
  @override
  void dispose() {
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

  @override
  void initState() {
    super.initState();
    _subscribeSession();
  }

  void _subscribeSession() {
    gatewayService.snapshotStream.listen((snapshot) {
      if (snapshot.acpSessionId == widget.session.id) {
        setState(() {
          _snapshot = snapshot;
          _loading = false;
        });
        _scrollToBottom();
      }
    });
    gatewayService.subscribe(widget.session.id);
  }

  void _scrollToBottom() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_scrollController.hasClients) {
        _scrollController.animateTo(
          _scrollController.position.maxScrollExtent,
          duration: const Duration(milliseconds: 300),
          curve: Curves.easeOut,
        );
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.session.title.isNotEmpty 
            ? widget.session.title 
            : widget.session.id),
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
          if (_snapshot?.pendingPermission != null)
            _buildPermissionBanner(_snapshot!.pendingPermission!),
          
          Expanded(
            child: _loading
                ? const Center(child: CircularProgressIndicator())
                : _buildEntryList(),
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
          width: 20, height: 20,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      AcpPhaseRunning() => const SizedBox(
          width: 20, height: 20,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      AcpPhaseAwaitingApproval() => const Icon(Icons.warning_amber, color: Colors.orange),
      AcpPhaseAwaitingChoice() => const Icon(Icons.help_outline, color: Colors.blue),
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
          if (permission.question.isNotEmpty)
            Text(permission.question),
          const SizedBox(height: 8),
          Wrap(
            spacing: 8,
            children: permission.options.map((opt) {
              return ElevatedButton(
                onPressed: () => _respondApproval(opt.optionId),
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

  Widget _buildEntryList() {
    final entries = _snapshot?.entries ?? [];
    if (entries.isEmpty) {
      return const Center(child: Text('No messages yet'));
    }
    
    return ListView.builder(
      controller: _scrollController,
      itemCount: entries.length,
      itemBuilder: (context, index) {
        return _buildEntry(entries[index]);
      },
    );
  }

  Widget _buildEntry(AcpEntry entry) {
    return switch (entry) {
      AcpEntryUser(text: final text) => _buildUserMessage(text),
      AcpEntryAssistant(text: final text, thought: final thought) => 
          _buildAssistantMessage(text, thought),
      AcpEntryToolCall(id: _, title: final title, kind: final kind, 
          status: final status, output: final output) =>
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
          width: 16, height: 16,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      ToolCallStatus.completed => const Icon(Icons.check_circle, 
          color: Colors.green, size: 16),
      ToolCallStatus.failed => const Icon(Icons.error, color: Colors.red, size: 16),
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
                    output.map((p) => switch (p) {
                      ToolOutputText(text: final t) => t,
                      ToolOutputDiff(path: final path, hunks: _) => '[Diff: $path]',
                      ToolOutputImage() => '[Image]',
                    }).join('\n'),
                    style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
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
            child: Text(label, style: TextStyle(color: Colors.grey[500], fontSize: 12)),
          ),
          const Expanded(child: Divider()),
        ],
      ),
    );
  }

  Widget _buildInputBar() {
    final isIdle = _snapshot?.phase is AcpPhaseIdle;
    
    return Container(
      padding: const EdgeInsets.all(8),
      decoration: BoxDecoration(
        color: Theme.of(context).colorScheme.surface,
        border: Border(
          top: BorderSide(color: Colors.grey[800]!),
        ),
      ),
      child: Row(
        children: [
          Expanded(
            child: TextField(
              controller: _messageController,
              enabled: isIdle,
              decoration: InputDecoration(
                hintText: isIdle ? 'Send a message...' : 'Waiting for response...',
                border: const OutlineInputBorder(),
              ),
              onSubmitted: (_) => _sendMessage(),
            ),
          ),
          const SizedBox(width: 8),
          IconButton(
            onPressed: isIdle ? _sendMessage : null,
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

  void _respondApproval(String optionKey) {
    gatewayService.respondApproval(widget.session.id, optionKey);
  }

  @override
  void dispose() {
    gatewayService.unsubscribe();
    _messageController.dispose();
    _scrollController.dispose();
    super.dispose();
  }
}
