import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:xterm/xterm.dart';

import '../services/gateway_service.dart';
import '../services/terminal_stream_service.dart';

class TerminalSessionPage extends StatefulWidget {
  const TerminalSessionPage({super.key, required this.session});

  final SessionSummary session;

  @override
  State<TerminalSessionPage> createState() => _TerminalSessionPageState();
}

class _TerminalSessionPageState extends State<TerminalSessionPage>
    with WidgetsBindingObserver {
  late final TerminalStreamService _stream;
  late Terminal _terminal;
  late StreamController<List<int>> _byteController;
  late StreamSubscription<String> _decodedSubscription;
  late final StreamSubscription<TerminalStreamEvent> _eventSubscription;
  late final StreamSubscription<TerminalStreamState> _stateSubscription;

  TerminalStreamState _streamState = TerminalStreamState.waitingForGateway;
  String? _error;
  bool _writeEnabled = false;
  int _decoderGeneration = 0;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _stream = TerminalStreamService(
      gateway: gatewayService,
      sessionId: widget.session.id,
    );
    _terminal = _newTerminal();
    _resetDecoder();
    _eventSubscription = _stream.events.listen(_handleTerminalEvent);
    _stateSubscription = _stream.stateStream.listen((state) {
      if (!mounted) return;
      setState(() {
        _streamState = state;
        if (state != TerminalStreamState.connected) _writeEnabled = false;
      });
    });
  }

  Terminal _newTerminal() => Terminal(
    maxLines: 5000,
    onOutput: _stream.sendInput,
    onResize: (cols, rows, cellWidth, cellHeight) {
      if (cols <= 0 || rows <= 0) return;
      final geometry = TerminalGeometry(
        cols: cols,
        rows: rows,
        cellWidth: cellWidth.clamp(1, 256),
        cellHeight: cellHeight.clamp(1, 256),
      );
      _stream.updateGeometry(geometry);
    },
  );

  void _resetDecoder() {
    final generation = ++_decoderGeneration;
    if (generation > 1) {
      unawaited(_byteController.close());
      unawaited(_decodedSubscription.cancel());
    }
    _byteController = StreamController<List<int>>();
    _decodedSubscription = _byteController.stream
        .transform(const Utf8Decoder(allowMalformed: true))
        .listen((text) {
          if (generation == _decoderGeneration) _terminal.write(text);
        });
  }

  void _handleTerminalEvent(TerminalStreamEvent event) {
    switch (event) {
      case TerminalReadyEvent():
        _terminal = _newTerminal();
        _resetDecoder();
        if (!mounted) return;
        setState(() {
          _writeEnabled = event.writeEnabled;
          _error = null;
        });
      case TerminalDataEvent():
        _byteController.add(event.bytes);
      case TerminalErrorEvent():
        if (!mounted) return;
        setState(() {
          _error = event.message;
          if (event.fatal) _writeEnabled = false;
        });
      case TerminalClosedEvent():
        if (!mounted) return;
        setState(() {
          _writeEnabled = false;
          _error = 'Terminal session ended';
        });
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        _stream.resume();
      case AppLifecycleState.inactive:
      case AppLifecycleState.hidden:
      case AppLifecycleState.paused:
      case AppLifecycleState.detached:
        _stream.suspend();
    }
  }

  @override
  Widget build(BuildContext context) {
    final title = widget.session.title.trim().isEmpty
        ? 'Terminal'
        : widget.session.title;
    return Scaffold(
      backgroundColor: const Color(0xff0b0d0f),
      appBar: AppBar(
        title: Text(title),
        actions: [
          Padding(
            padding: const EdgeInsets.only(right: 12),
            child: Center(child: _buildConnectionIndicator()),
          ),
        ],
      ),
      body: SafeArea(
        top: false,
        child: Column(
          children: [
            if (_error != null) _TerminalErrorBar(message: _error!),
            Expanded(
              child: TerminalView(
                _terminal,
                key: ValueKey(_terminal),
                autofocus: _writeEnabled,
                readOnly: !_writeEnabled,
                deleteDetection: true,
                simulateScroll: true,
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
                theme: TerminalThemes.defaultTheme,
                textStyle: const TerminalStyle(
                  fontSize: 13,
                  height: 1.15,
                  fontFamily: 'monospace',
                ),
              ),
            ),
            if (_writeEnabled) TerminalShortcutBar(onInput: _stream.sendInput),
          ],
        ),
      ),
    );
  }

  Widget _buildConnectionIndicator() {
    return switch (_streamState) {
      TerminalStreamState.connected => const Icon(
        Icons.circle,
        size: 10,
        color: Colors.green,
      ),
      TerminalStreamState.ended => const Icon(
        Icons.stop_circle_outlined,
        size: 18,
      ),
      TerminalStreamState.connecting => const SizedBox.square(
        dimension: 16,
        child: CircularProgressIndicator(strokeWidth: 2),
      ),
      TerminalStreamState.waitingForGateway => const Icon(
        Icons.cloud_off_outlined,
        size: 18,
      ),
    };
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _decoderGeneration++;
    unawaited(_byteController.close());
    unawaited(_decodedSubscription.cancel());
    unawaited(_eventSubscription.cancel());
    unawaited(_stateSubscription.cancel());
    unawaited(_stream.dispose());
    super.dispose();
  }
}

class _TerminalErrorBar extends StatelessWidget {
  const _TerminalErrorBar({required this.message});

  final String message;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      color: colors.errorContainer,
      child: Text(
        message,
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
        style: TextStyle(color: colors.onErrorContainer, fontSize: 12),
      ),
    );
  }
}

class TerminalShortcutBar extends StatelessWidget {
  const TerminalShortcutBar({super.key, required this.onInput});

  final ValueChanged<String> onInput;

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 48,
      color: Theme.of(context).colorScheme.surfaceContainer,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
        children: [
          _TerminalTextKey(label: 'Esc', value: '\x1b', onInput: onInput),
          _TerminalTextKey(label: 'Tab', value: '\t', onInput: onInput),
          _TerminalTextKey(label: '^C', value: '\x03', onInput: onInput),
          _TerminalIconKey(
            icon: Icons.keyboard_arrow_left,
            tooltip: 'Left',
            value: '\x1b[D',
            onInput: onInput,
          ),
          _TerminalIconKey(
            icon: Icons.keyboard_arrow_down,
            tooltip: 'Down',
            value: '\x1b[B',
            onInput: onInput,
          ),
          _TerminalIconKey(
            icon: Icons.keyboard_arrow_up,
            tooltip: 'Up',
            value: '\x1b[A',
            onInput: onInput,
          ),
          _TerminalIconKey(
            icon: Icons.keyboard_arrow_right,
            tooltip: 'Right',
            value: '\x1b[C',
            onInput: onInput,
          ),
          _TerminalTextKey(label: 'PgUp', value: '\x1b[5~', onInput: onInput),
          _TerminalTextKey(label: 'PgDn', value: '\x1b[6~', onInput: onInput),
        ],
      ),
    );
  }
}

class _TerminalTextKey extends StatelessWidget {
  const _TerminalTextKey({
    required this.label,
    required this.value,
    required this.onInput,
  });

  final String label;
  final String value;
  final ValueChanged<String> onInput;

  @override
  Widget build(BuildContext context) {
    return TextButton(onPressed: () => onInput(value), child: Text(label));
  }
}

class _TerminalIconKey extends StatelessWidget {
  const _TerminalIconKey({
    required this.icon,
    required this.tooltip,
    required this.value,
    required this.onInput,
  });

  final IconData icon;
  final String tooltip;
  final String value;
  final ValueChanged<String> onInput;

  @override
  Widget build(BuildContext context) {
    return IconButton(
      icon: Icon(icon),
      tooltip: tooltip,
      onPressed: () => onInput(value),
    );
  }
}
