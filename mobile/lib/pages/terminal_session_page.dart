import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:xterm/xterm.dart';

import '../services/gateway_service.dart';
import '../services/terminal_stream_service.dart';
import '../utils/xterm_input_filter.dart';

class TerminalSessionPage extends StatefulWidget {
  const TerminalSessionPage({super.key, required this.session, this.stream});

  final SessionSummary session;
  final TerminalStreamClient? stream;

  @override
  State<TerminalSessionPage> createState() => _TerminalSessionPageState();
}

class _TerminalSessionPageState extends State<TerminalSessionPage>
    with WidgetsBindingObserver {
  late final TerminalStreamClient _stream;
  late Terminal _terminal;
  late GlobalKey<TerminalViewState> _terminalViewKey;
  late StreamController<List<int>> _byteController;
  late StreamSubscription<String> _decodedSubscription;
  late XtermInputFilter _inputFilter;
  late final StreamSubscription<TerminalStreamEvent> _eventSubscription;
  late final StreamSubscription<TerminalStreamState> _stateSubscription;
  final FocusNode _terminalFocusNode = FocusNode();
  final ScrollController _terminalScrollController = ScrollController();

  TerminalStreamState _streamState = TerminalStreamState.waitingForGateway;
  String? _error;
  bool _writeEnabled = false;
  bool _softwareKeyboardEnabled = false;
  bool _softwareKeyboardWasVisible = false;
  int _decoderGeneration = 0;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _stream =
        widget.stream ??
        TerminalStreamService(
          gateway: gatewayService,
          sessionId: widget.session.id,
        );
    _terminal = _newTerminal();
    _terminalViewKey = GlobalKey<TerminalViewState>();
    _resetDecoder();
    _eventSubscription = _stream.events.listen(_handleTerminalEvent);
    _stateSubscription = _stream.stateStream.listen((state) {
      if (!mounted) return;
      if (state != TerminalStreamState.connected) {
        _closeKeyboardAndReleaseFocus();
      }
      setState(() {
        _streamState = state;
        if (state != TerminalStreamState.connected) {
          _writeEnabled = false;
          _softwareKeyboardEnabled = false;
          _softwareKeyboardWasVisible = false;
        }
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
    _inputFilter = XtermInputFilter();
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
        _closeKeyboardAndReleaseFocus();
        _terminal = _newTerminal();
        _terminalViewKey = GlobalKey<TerminalViewState>();
        _resetDecoder();
        if (!mounted) return;
        setState(() {
          _writeEnabled = event.writeEnabled;
          _softwareKeyboardEnabled = false;
          _softwareKeyboardWasVisible = false;
          _error = null;
        });
      case TerminalDataEvent():
        final bytes = _inputFilter.add(event.bytes);
        if (bytes.isNotEmpty) _byteController.add(bytes);
      case TerminalReplayCompleteEvent():
        _scrollToLatestAfterReplay();
      case TerminalErrorEvent():
        if (!mounted) return;
        if (event.fatal) _closeKeyboardAndReleaseFocus();
        setState(() {
          _error = event.message;
          if (event.fatal) {
            _writeEnabled = false;
            _softwareKeyboardEnabled = false;
            _softwareKeyboardWasVisible = false;
          }
        });
      case TerminalClosedEvent():
        if (!mounted) return;
        _closeKeyboardAndReleaseFocus();
        setState(() {
          _writeEnabled = false;
          _softwareKeyboardEnabled = false;
          _softwareKeyboardWasVisible = false;
          _error = 'Terminal session ended';
        });
    }
  }

  void _scrollToLatestAfterReplay() {
    final generation = _decoderGeneration;
    // Replay data passes through an asynchronous UTF-8 decoder. Wait for it
    // to update xterm, then give RenderTerminal one layout to publish its new
    // scroll extent before moving to the live tail.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || generation != _decoderGeneration) return;
      _terminalViewKey.currentState?.renderTerminal.markNeedsLayout();
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (!mounted ||
            generation != _decoderGeneration ||
            !_terminalScrollController.hasClients) {
          return;
        }
        final position = _terminalScrollController.position;
        position.jumpTo(position.maxScrollExtent);
      });
    });
  }

  void _toggleSoftwareKeyboard() {
    if (!_writeEnabled) return;
    if (_softwareKeyboardEnabled) {
      _closeSoftwareKeyboard();
      return;
    }

    _enableSoftwareKeyboard();
  }

  void _enableSoftwareKeyboard() {
    if (!_writeEnabled || _softwareKeyboardEnabled) return;

    setState(() {
      _softwareKeyboardEnabled = true;
      _softwareKeyboardWasVisible = false;
    });
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || !_softwareKeyboardEnabled) return;
      _terminalViewKey.currentState?.requestKeyboard();
    });
  }

  void _syncGeometryAfterLayout() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _terminalViewKey.currentState?.renderTerminal.markNeedsLayout();
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _stream.forceGeometry();
      });
    });
  }

  void _closeSoftwareKeyboard() {
    _closeKeyboardAndReleaseFocus();
    if (mounted && _softwareKeyboardEnabled) {
      setState(() {
        _softwareKeyboardEnabled = false;
        _softwareKeyboardWasVisible = false;
      });
    }
  }

  void _closeKeyboardAndReleaseFocus() {
    _terminalViewKey.currentState?.closeKeyboard();
    _terminalFocusNode.unfocus();
  }

  @override
  void didChangeMetrics() {
    super.didChangeMetrics();
    if (!mounted || !_softwareKeyboardEnabled) return;
    if (View.of(context).viewInsets.bottom > 0) {
      _softwareKeyboardWasVisible = true;
      _syncGeometryAfterLayout();
      return;
    }
    if (!_softwareKeyboardWasVisible) return;
    setState(() {
      _softwareKeyboardEnabled = false;
      _softwareKeyboardWasVisible = false;
    });
    _syncGeometryAfterLayout();
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
        _closeSoftwareKeyboard();
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
          IconButton(
            tooltip: !_writeEnabled
                ? 'Keyboard unavailable'
                : _softwareKeyboardEnabled
                ? 'Hide keyboard'
                : 'Show keyboard',
            onPressed: _writeEnabled ? _toggleSoftwareKeyboard : null,
            icon: Icon(
              _softwareKeyboardEnabled
                  ? Icons.keyboard_hide_outlined
                  : Icons.keyboard_outlined,
            ),
          ),
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
                key: _terminalViewKey,
                focusNode: _terminalFocusNode,
                scrollController: _terminalScrollController,
                autofocus: false,
                readOnly: !_writeEnabled,
                hardwareKeyboardOnly: !_softwareKeyboardEnabled,
                deleteDetection: true,
                simulateScroll: true,
                onTapUp: (_, _) => _enableSoftwareKeyboard(),
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
                theme: TerminalThemes.defaultTheme,
                textStyle: const TerminalStyle(
                  fontSize: 13,
                  height: 1.15,
                  fontFamily: 'monospace',
                ),
              ),
            ),
            if (_writeEnabled && _softwareKeyboardEnabled)
              TerminalShortcutBar(onInput: _stream.sendInput),
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
    _terminalViewKey.currentState?.closeKeyboard();
    _terminalFocusNode.dispose();
    _terminalScrollController.dispose();
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
