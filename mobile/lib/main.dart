import 'package:flutter/material.dart';
import 'src/rust/api.dart' as api;
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
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
  api.ConnectionState _connectionState = api.ConnectionState.disconnected;
  List<api.SessionSummary> _sessions = [];

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
        ],
      ),
      body: _buildBody(),
      floatingActionButton: _connectionState == api.ConnectionState.connected
          ? FloatingActionButton(
              onPressed: _refreshSessions,
              child: const Icon(Icons.refresh),
            )
          : null,
    );
  }

  Widget _buildBody() {
    switch (_connectionState) {
      case api.ConnectionState.disconnected:
        return _buildDisconnectedView();
      case api.ConnectionState.connecting:
      case api.ConnectionState.handshaking:
        return const Center(child: CircularProgressIndicator());
      case api.ConnectionState.connected:
        return _buildSessionList();
      case api.ConnectionState.authFailed:
        return _buildErrorView('Authentication failed');
      case api.ConnectionState.reconnecting:
        return _buildReconnectingView();
    }
  }

  Widget _buildDisconnectedView() {
    return Center(
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          const Icon(Icons.link_off, size: 64, color: Colors.grey),
          const SizedBox(height: 16),
          const Text('Not connected'),
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
          leading: _getAgentIcon(session.agentKind),
          title: Text(session.title),
          subtitle: Text(session.lastMessage ?? ''),
          trailing: session.unread
              ? Container(
                  width: 12,
                  height: 12,
                  decoration: const BoxDecoration(
                    color: Colors.blue,
                    shape: BoxShape.circle,
                  ),
                )
              : null,
          onTap: () => _openSession(session),
        );
      },
    );
  }

  Widget _buildErrorView(String message) {
    return Center(
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          const Icon(Icons.error_outline, size: 64, color: Colors.red),
          const SizedBox(height: 16),
          Text(message),
          const SizedBox(height: 24),
          ElevatedButton(
            onPressed: _scanQrCode,
            child: const Text('Try Again'),
          ),
        ],
      ),
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

  Widget _getAgentIcon(api.AgentKind kind) {
    switch (kind) {
      case api.AgentKind.claude:
        return const CircleAvatar(
          backgroundColor: Colors.orange,
          child: Text('C'),
        );
      case api.AgentKind.codex:
        return const CircleAvatar(
          backgroundColor: Colors.green,
          child: Text('X'),
        );
      case api.AgentKind.copilot:
        return const CircleAvatar(
          backgroundColor: Colors.blue,
          child: Text('P'),
        );
      case api.AgentKind.grok:
        return const CircleAvatar(
          backgroundColor: Colors.purple,
          child: Text('G'),
        );
      case api.AgentKind.other:
        return const CircleAvatar(child: Text('?'));
    }
  }

  void _scanQrCode() {
    // TODO: Implement QR code scanning
    ScaffoldMessenger.of(context).showSnackBar(
      const SnackBar(content: Text('QR scanning not yet implemented')),
    );
  }

  Future<void> _refreshSessions() async {
    try {
      final sessions = await api.listSessions();
      setState(() {
        _sessions = sessions;
      });
    } catch (e) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Failed to load sessions: $e')),
      );
    }
  }

  void _openSession(api.SessionSummary session) {
    Navigator.push(
      context,
      MaterialPageRoute(
        builder: (context) => SessionPage(session: session),
      ),
    );
  }
}

class SessionPage extends StatefulWidget {
  final api.SessionSummary session;

  const SessionPage({super.key, required this.session});

  @override
  State<SessionPage> createState() => _SessionPageState();
}

class _SessionPageState extends State<SessionPage> {
  final TextEditingController _messageController = TextEditingController();
  final List<api.AcpEntry> _entries = [];
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    _loadSession();
  }

  Future<void> _loadSession() async {
    try {
      final entries = await api.subscribeSession(sessionId: widget.session.id);
      setState(() {
        _entries.addAll(entries);
        _loading = false;
      });
    } catch (e) {
      setState(() => _loading = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.session.title),
      ),
      body: Column(
        children: [
          Expanded(
            child: _loading
                ? const Center(child: CircularProgressIndicator())
                : ListView.builder(
                    itemCount: _entries.length,
                    itemBuilder: (context, index) {
                      return _buildEntry(_entries[index]);
                    },
                  ),
          ),
          _buildInputBar(),
        ],
      ),
    );
  }

  Widget _buildEntry(api.AcpEntry entry) {
    return entry.when(
      user: (text) => _buildUserMessage(text),
      assistant: (text, thought) => _buildAssistantMessage(text, thought),
      toolCall: (id, title, kind, status, output) =>
          _buildToolCall(id, title, kind, status, output),
    );
  }

  Widget _buildUserMessage(String text) {
    return Align(
      alignment: Alignment.centerRight,
      child: Container(
        margin: const EdgeInsets.all(8),
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: Colors.blue,
          borderRadius: BorderRadius.circular(12),
        ),
        child: Text(text, style: const TextStyle(color: Colors.white)),
      ),
    );
  }

  Widget _buildAssistantMessage(String text, bool thought) {
    return Align(
      alignment: Alignment.centerLeft,
      child: Container(
        margin: const EdgeInsets.all(8),
        padding: const EdgeInsets.all(12),
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
    String id,
    String title,
    api.ToolKind kind,
    api.ToolStatus status,
    List<String> output,
  ) {
    return Card(
      margin: const EdgeInsets.all(8),
      child: ListTile(
        leading: _getToolIcon(kind, status),
        title: Text(title),
        subtitle: output.isNotEmpty ? Text(output.last) : null,
      ),
    );
  }

  Widget _getToolIcon(api.ToolKind kind, api.ToolStatus status) {
    final color = switch (status) {
      api.ToolStatus.running => Colors.yellow,
      api.ToolStatus.completed => Colors.green,
      api.ToolStatus.failed => Colors.red,
    };

    final icon = switch (kind) {
      api.ToolKind.read => Icons.visibility,
      api.ToolKind.write => Icons.create,
      api.ToolKind.edit => Icons.edit,
      api.ToolKind.bash => Icons.terminal,
      api.ToolKind.other => Icons.build,
    };

    return Icon(icon, color: color);
  }

  Widget _buildInputBar() {
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
              decoration: const InputDecoration(
                hintText: 'Send a message...',
                border: OutlineInputBorder(),
              ),
              onSubmitted: (_) => _sendMessage(),
            ),
          ),
          const SizedBox(width: 8),
          IconButton(
            onPressed: _sendMessage,
            icon: const Icon(Icons.send),
          ),
        ],
      ),
    );
  }

  Future<void> _sendMessage() async {
    final text = _messageController.text.trim();
    if (text.isEmpty) return;

    _messageController.clear();

    try {
      await api.sendMessage(sessionId: widget.session.id, text: text);
    } catch (e) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Failed to send: $e')),
      );
    }
  }

  @override
  void dispose() {
    _messageController.dispose();
    super.dispose();
  }
}
