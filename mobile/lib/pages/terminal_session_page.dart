import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:xterm/xterm.dart';

import '../services/gateway_service.dart';
import '../services/terminal_prefs_store.dart';
import '../services/terminal_stream_service.dart';
import '../theme/terminal_theme_wire.dart';
import '../utils/xterm_input_filter.dart';
import '../widgets/pending_action_badge.dart';
import '../theme/smelt_theme.dart';

class TerminalSessionPage extends StatefulWidget {
  const TerminalSessionPage({
    super.key,
    required this.session,
    this.stream,
    this.onShowPendingActions,
    this.prefsStore,
  });

  final SessionSummary session;
  final TerminalStreamClient? stream;

  /// 测试注入用；默认落到应用支持目录下的一个 json。
  final TerminalPrefsStore? prefsStore;

  /// 点全局待办徽标时调用。终端会话尤其需要：盯着一个 shell 跑的时候，别的
  /// 会话在等审批同样得看得见。
  final VoidCallback? onShowPendingActions;

  @override
  State<TerminalSessionPage> createState() => _TerminalSessionPageState();
}

class _TerminalSessionPageState extends State<TerminalSessionPage>
    with WidgetsBindingObserver {
  late final TerminalStreamClient _stream;
  late Terminal _terminal;
  late GlobalKey<TerminalViewState> _terminalViewKey;
  late Sink<List<int>> _byteSink;
  late XtermInputFilter _inputFilter;
  late final StreamSubscription<TerminalStreamEvent> _eventSubscription;
  late final StreamSubscription<TerminalStreamState> _stateSubscription;
  final FocusNode _terminalFocusNode = FocusNode();
  final ScrollController _terminalScrollController = ScrollController();

  TerminalStreamState _streamState = TerminalStreamState.waitingForGateway;
  String? _error;

  /// 终端配色：连上后由那台设备下发（见 `terminalReady.theme`），在此之前用兜底深色。
  /// PC 主题可配置，而且一部手机可以连多台设备，客户端不能写死色板。
  TerminalTheme _theme = SmeltTerminalTheme.fallbackDark;
  bool _themeIsDark = true;
  bool _writeEnabled = false;
  bool _softwareKeyboardEnabled = false;
  bool _softwareKeyboardWasVisible = false;
  bool _terminalGeometryLocked = false;
  bool _replayGeometryLocked = false;
  late final TerminalPrefsStore _prefsStore;
  TerminalPrefs _prefs = const TerminalPrefs();
  int _decoderGeneration = 0;

  /// 正在按更大的预算重取快照（用户滚到顶触发）。
  bool _loadingMoreHistory = false;

  /// 重取前光标停在「离底多远」的位置。历史是往上长的，底部不动，所以这个距离
  /// 在新快照里仍然指着同一段内容——按它恢复，用户不会被弹走。
  double? _pendingScrollDistanceFromBottom;

  /// 视图已经离开最新输出（用户往上翻了，或者刚补完历史落在中间）。实时输出仍在
  /// 底部推进，得给用户一个回得去的出口。
  bool _awayFromTail = false;

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
    _terminalScrollController.addListener(_handleTerminalScroll);
    _prefsStore = widget.prefsStore ?? FileTerminalPrefsStore();
    unawaited(_loadPrefs());
    _eventSubscription = _stream.events.listen(_handleTerminalEvent);
    _stateSubscription = _stream.stateStream.listen((state) {
      if (!mounted) return;
      if (state != TerminalStreamState.connected) {
        _closeSoftwareKeyboard();
      }
      setState(() {
        _streamState = state;
        if (state != TerminalStreamState.connected) {
          _writeEnabled = false;
        }
      });
    });
  }

  Terminal _newTerminal({int cols = 80, int rows = 24}) {
    final terminal = Terminal(
      // 和 attach 时声明的 scrollback 上限同源：daemon 发多了，超出的部分解码完
      // 就会被这个环形缓冲丢掉。
      maxLines: kMaxTerminalScrollbackLines,
      onOutput: _stream.sendInput,
    );
    // A daemon snapshot contains cursor-addressed output for the dimensions in
    // terminalReady. Establish that grid before any replay byte is decoded.
    terminal.resize(cols.clamp(1, 300), rows.clamp(1, 200));
    terminal.onResize = _handleTerminalResize;
    return terminal;
  }

  void _handleTerminalResize(
    int cols,
    int rows,
    int cellWidth,
    int cellHeight,
  ) {
    if (cols <= 0 || rows <= 0) return;
    _stream.updateGeometry(
      TerminalGeometry(
        cols: cols,
        rows: rows,
        cellWidth: cellWidth.clamp(1, 256),
        cellHeight: cellHeight.clamp(1, 256),
      ),
    );
  }

  void _resetDecoder() {
    final generation = ++_decoderGeneration;
    if (generation > 1) {
      _byteSink.close();
    }
    _inputFilter = XtermInputFilter();
    final terminal = _terminal;
    _byteSink = const Utf8Decoder(allowMalformed: true).startChunkedConversion(
      _CallbackSink<String>((text) {
        if (generation != _decoderGeneration) return;
        try {
          terminal.write(text);
        } catch (error, stack) {
          // A failure inside xterm must not tear down the byte pipeline: the
          // page would then be frozen on whatever was decoded so far, with no
          // way back short of leaving and re-entering the session.
          FlutterError.reportError(
            FlutterErrorDetails(
              exception: error,
              stack: stack,
              library: 'smelt terminal',
              context: ErrorDescription('writing terminal output'),
            ),
          );
        }
      }),
    );
  }

  void _handleTerminalEvent(TerminalStreamEvent event) {
    switch (event) {
      case TerminalConnectedEvent():
        // attach 还没发。先把快捷键栏摆上去，让终端视口在 attach 之前就收敛到最终
        // 高度——否则回放完成后栏子上屏又要改一次行数，那就是一次多余的 PTY
        // resize，而主屏 CLI 每收一次 SIGWINCH 就把整段对话重印一遍。
        if (!mounted || _writeEnabled == event.writeEnabled) return;
        setState(() => _writeEnabled = event.writeEnabled);
      case TerminalReadyEvent():
        _closeSoftwareKeyboard();
        _terminal = _newTerminal(cols: event.cols, rows: event.rows);
        _terminalViewKey = GlobalKey<TerminalViewState>();
        _resetDecoder();
        if (!mounted) return;
        setState(() {
          _writeEnabled = event.writeEnabled;
          _softwareKeyboardEnabled = false;
          _replayGeometryLocked = true;
          // 快照换了一整帧，上一帧的「离开尾巴」结论跟着作废。
          _awayFromTail = false;
          _error = null;
          // 配色由这台设备下发（可能深色也可能浅色，还可能是用户自选底色）；
          // 老网关不下发时沿用兜底深色。
          _theme = event.theme ?? SmeltTerminalTheme.fallbackDark;
          _themeIsDark = event.themeIsDark;
        });
      case TerminalDataEvent():
        final bytes = _inputFilter.add(event.bytes);
        if (bytes.isNotEmpty) _byteSink.add(bytes);
      case TerminalReplayCompleteEvent():
        if (mounted) {
          setState(() {
            _replayGeometryLocked = false;
            // 重建视图，逼 xterm 重新量一次视口。锁着的这段时间里视口很可能已经
            // 变矮了——`terminalReady` 一到就把快捷键栏(48px)推上屏，错误条也可
            // 能出现——而 RenderTerminal 只在「量出来的格子数和上次不同」时才通
            // 知终端，那次不同恰好落在锁里被丢掉，解锁后就再也不会重算。终端于
            // 是停在偏高的行数上：全屏 TUI（alt buffer）没有回滚，多出来的行直
            // 接变成外层滚动量，手势全被它吃掉，TUI 内部就再也滚不动了。
            _terminalViewKey = GlobalKey<TerminalViewState>();
          });
        }
        if (mounted && _loadingMoreHistory) {
          setState(() => _loadingMoreHistory = false);
        }
        final restore = _pendingScrollDistanceFromBottom;
        _pendingScrollDistanceFromBottom = null;
        if (restore != null) {
          _restoreScrollAfterLayout(restore);
        } else {
          _scrollToLatestAfterReplay();
        }
      case TerminalErrorEvent():
        if (!mounted) return;
        if (event.fatal) _closeSoftwareKeyboard();
        setState(() {
          _error = event.message;
          if (event.fatal) {
            _writeEnabled = false;
            _replayGeometryLocked = false;
            _loadingMoreHistory = false;
            _pendingScrollDistanceFromBottom = null;
          }
        });
      case TerminalClosedEvent():
        if (!mounted) return;
        _closeSoftwareKeyboard();
        setState(() {
          _writeEnabled = false;
          _replayGeometryLocked = false;
          _loadingMoreHistory = false;
          _pendingScrollDistanceFromBottom = null;
          _error = 'Terminal session ended';
        });
    }
  }

  void _handleTerminalScroll() {
    _updateAwayFromTail();
    _maybeLoadMoreHistory();
  }

  /// xterm 贴底时是用 `correctBy` 跟随的，不会通知监听者；所以这里只会在偏移真的
  /// 变了的时候被叫到，贴底状态不会被实时输出误判成「离开了尾巴」。
  void _updateAwayFromTail() {
    if (!mounted || !_terminalScrollController.hasClients) return;
    final position = _terminalScrollController.position;
    final away = position.maxScrollExtent - position.pixels > 1.0;
    if (away == _awayFromTail) return;
    setState(() => _awayFromTail = away);
  }

  void _jumpToTail() {
    if (!_terminalScrollController.hasClients) return;
    final position = _terminalScrollController.position;
    // 隔着几千行做动画既慢又没意义，直接落到最新输出。
    position.jumpTo(position.maxScrollExtent);
    _updateAwayFromTail();
  }

  /// 滚到顶就去要更老的一段。首屏只带最新的几百行，剩下的按需补。
  void _maybeLoadMoreHistory() {
    if (!mounted ||
        _loadingMoreHistory ||
        _replayGeometryLocked ||
        !_terminalScrollController.hasClients ||
        !_stream.canLoadMoreScrollback) {
      return;
    }
    final position = _terminalScrollController.position;
    // 只认用户自己滚上来的那一次。补加载会整帧重取快照、重建终端，代价大到不能
    // 由「偏移量恰好是 0」这种巧合触发：新建的 ScrollPosition 从 0 起步，回放后
    // 重建视图、视口变化后的夹取、我们自己的 jumpTo，都会在没人碰屏幕的时候把
    // 偏移送到顶部——那时候补历史纯属误伤，用户一进页面就被卷进一次重载，还会被
    // 按「离底距离」按在半中间，看不到最新输出。
    if (position.userScrollDirection == ScrollDirection.idle) return;
    // maxScrollExtent 为 0 时整段内容没占满一屏，此刻的 pixels==0 不是用户滚上来的。
    if (position.maxScrollExtent <= 0 || position.pixels > 0) return;
    final distanceFromBottom = position.maxScrollExtent - position.pixels;
    if (!_stream.loadMoreScrollback()) return;
    setState(() {
      _loadingMoreHistory = true;
      _pendingScrollDistanceFromBottom = distanceFromBottom;
    });
  }

  void _restoreScrollAfterLayout(double distanceFromBottom) {
    final generation = _decoderGeneration;
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
        position.jumpTo(
          (position.maxScrollExtent - distanceFromBottom).clamp(
            0.0,
            position.maxScrollExtent,
          ),
        );
      });
    });
  }

  void _scrollToLatestAfterReplay() {
    final generation = _decoderGeneration;
    // The decoder has synchronously applied the replay. Give RenderTerminal a
    // layout to publish the resized buffer's scroll extent before following it.
    _scrollToTerminalTailAfterLayout(decoderGeneration: generation);
  }

  void _scrollToTerminalTailAfterLayout({int? decoderGeneration}) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted ||
          (decoderGeneration != null &&
              decoderGeneration != _decoderGeneration)) {
        return;
      }
      _terminalViewKey.currentState?.renderTerminal.markNeedsLayout();
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (!mounted ||
            (decoderGeneration != null &&
                decoderGeneration != _decoderGeneration) ||
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
      _terminalGeometryLocked = true;
    });
    _scrollToTerminalTailAfterLayout();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || !_softwareKeyboardEnabled) return;
      _terminalViewKey.currentState?.requestKeyboard();
    });
  }

  void _closeSoftwareKeyboard() {
    _closeKeyboardAndReleaseFocus();
    if (!mounted || (!_softwareKeyboardEnabled && !_terminalGeometryLocked)) {
      return;
    }

    final keyboardVisible = View.of(context).viewInsets.bottom > 0;
    if (!keyboardVisible) {
      _finishSoftwareKeyboardCycle();
      return;
    }

    setState(() {
      _softwareKeyboardEnabled = false;
      _softwareKeyboardWasVisible = true;
      _terminalGeometryLocked = true;
      _terminalViewKey = GlobalKey<TerminalViewState>();
    });
    _scrollToTerminalTailAfterLayout();
  }

  void _finishSoftwareKeyboardCycle() {
    if (!mounted ||
        (!_softwareKeyboardEnabled &&
            !_softwareKeyboardWasVisible &&
            !_terminalGeometryLocked)) {
      return;
    }
    setState(() {
      _softwareKeyboardEnabled = false;
      _softwareKeyboardWasVisible = false;
      _terminalGeometryLocked = false;
      // xterm 4.0 keeps IME composing text after closeKeyboard(). Recreating
      // the view clears that local render state without replacing the PTY.
      _terminalViewKey = GlobalKey<TerminalViewState>();
    });
    _scrollToTerminalTailAfterLayout();
  }

  void _closeKeyboardAndReleaseFocus() {
    _terminalViewKey.currentState?.closeKeyboard();
    _terminalFocusNode.unfocus();
  }

  @override
  void didChangeMetrics() {
    super.didChangeMetrics();
    if (!mounted || (!_softwareKeyboardEnabled && !_terminalGeometryLocked)) {
      return;
    }
    if (View.of(context).viewInsets.bottom > 0) {
      if (!_softwareKeyboardWasVisible) {
        _softwareKeyboardWasVisible = true;
        _scrollToTerminalTailAfterLayout();
      }
      return;
    }
    if (!_softwareKeyboardWasVisible) return;
    _closeKeyboardAndReleaseFocus();
    _finishSoftwareKeyboardCycle();
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
      // 页面底色跟着设备的终端配色走：PC 切成浅色主题后，浅色终端嵌在纯黑页面里
      // 会在边缘炸出刺眼的对比。
      backgroundColor: _themeIsDark
          ? const Color(0xff0b0d0f)
          : _theme.background,
      appBar: AppBar(
        title: Text(title),
        actions: [
          if (widget.onShowPendingActions case final show?)
            PendingActionBadge(onPressed: show),
          _buildFontSizeButton(),
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
              child: Stack(
                children: [
                  Positioned.fill(child: _buildTerminalView()),
                  // 补历史的进度条只能浮在上面：放进 Column 会把终端视口压矮几像素，
                  // 行数变了就是一次 PTY resize，主屏 CLI 会因此重印整段对话。
                  if (_loadingMoreHistory)
                    const Positioned(
                      top: 0,
                      left: 0,
                      right: 0,
                      child: LinearProgressIndicator(minHeight: 2),
                    ),
                  // 补完历史后用户停在几千行之外，实时输出还在底部继续跑。没有这
                  // 个出口，回到最新内容只能一路手动拖。
                  if (_awayFromTail)
                    Positioned(
                      right: 12,
                      bottom: 12,
                      child: _BackToTailButton(onPressed: _jumpToTail),
                    ),
                ],
              ),
            ),
            // 快捷键栏只要能写就常驻：最高频的动作是「看着 agent 跑、按 ^C 打断」，
            // 那时并不需要软键盘。挂在键盘上会逼用户先唤起键盘遮掉半屏，而且键盘
            // 开合还会连带改变终端视口高度，白白触发一轮 cols/rows 重算。
            if (_writeEnabled) TerminalShortcutBar(onKey: _sendKey),
          ],
        ),
      ),
    );
  }

  Widget _buildTerminalView() {
    return TerminalView(
      _terminal,
      key: _terminalViewKey,
      focusNode: _terminalFocusNode,
      scrollController: _terminalScrollController,
      autofocus: false,
      readOnly: !_writeEnabled,
      hardwareKeyboardOnly: !_softwareKeyboardEnabled,
      autoResize: !_terminalGeometryLocked && !_replayGeometryLocked,
      deleteDetection: true,
      simulateScroll: true,
      onTapUp: (_, _) => _enableSoftwareKeyboard(),
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
      theme: _theme,
      textStyle: TerminalStyle(
        fontSize: _prefs.fontSize,
        height: 1.15,
        fontFamily: 'monospace',
      ),
    );
  }

  /// 快捷键栏统一出口。编码交给终端本身，它知道对端有没有开 kitty keyboard
  /// protocol（决定 Shift+Tab 发 `ESC[Z` 还是 `ESC[9;2u`）和 application
  /// cursor 模式（决定方向键发 CSI 还是 SS3），也和蓝牙键盘走的是同一条路径，
  /// 两边不会各编各的。`onOutput` 已经接到守护，所以这里不用再自己发。
  void _sendKey(
    TerminalKey key, {
    bool shift = false,
    bool alt = false,
    bool ctrl = false,
  }) {
    // 快捷键栏是纯视觉按钮，没有实体键的落键感；不给触感用户无法确认按没按上。
    HapticFeedback.selectionClick();
    _terminal.keyInput(key, shift: shift, alt: alt, ctrl: ctrl);
  }

  Future<void> _loadPrefs() async {
    final TerminalPrefs prefs;
    try {
      prefs = await _prefsStore.load();
    } catch (_) {
      return;
    }
    if (!mounted || prefs == _prefs) return;
    setState(() => _prefs = prefs);
  }

  void _setFontSize(double size) {
    if (size == _prefs.fontSize) return;
    HapticFeedback.selectionClick();
    // 先落到 UI 再写盘：字号是本地显示偏好，存盘失败不该拦着用户看清屏幕。
    setState(() => _prefs = _prefs.copyWith(fontSize: size));
    unawaited(_prefsStore.save(_prefs).catchError((Object _) {}));
  }

  Widget _buildFontSizeButton() {
    return PopupMenuButton<double>(
      tooltip: 'Text size',
      icon: const Text(
        'Aa',
        style: TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
      ),
      initialValue: _prefs.fontSize,
      onSelected: _setFontSize,
      itemBuilder: (context) => [
        for (final size in TerminalPrefs.steps)
          PopupMenuItem<double>(
            value: size,
            child: Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              children: [
                // 每一档用它自己的字号渲染，选之前就能看出差别。
                Text(
                  '${size.toStringAsFixed(0)} pt',
                  style: TextStyle(fontSize: size),
                ),
                if (size == _prefs.fontSize) const Icon(Icons.check, size: 18),
              ],
            ),
          ),
      ],
    );
  }

  Widget _buildConnectionIndicator() {
    // 这个指示器原来只有颜色/形状，没有任何文字：读屏用户读不到，色盲用户也分不清
    // 绿点和灰点。包一层 Semantics + Tooltip 把状态说出来。
    final label = switch (_streamState) {
      TerminalStreamState.connected => 'Connected',
      TerminalStreamState.ended => 'Session ended',
      TerminalStreamState.connecting => 'Connecting',
      TerminalStreamState.waitingForGateway => 'Waiting for the desktop',
    };
    return Tooltip(
      message: label,
      child: Semantics(label: label, child: _connectionIcon()),
    );
  }

  Widget _connectionIcon() {
    return switch (_streamState) {
      TerminalStreamState.connected => Icon(
        Icons.circle,
        size: 10,
        color: context.smeltColors.done,
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
    _terminalScrollController.removeListener(_handleTerminalScroll);
    _terminalScrollController.dispose();
    _decoderGeneration++;
    _byteSink.close();
    unawaited(_eventSubscription.cancel());
    unawaited(_stateSubscription.cancel());
    unawaited(_stream.dispose());
    super.dispose();
  }
}

class _CallbackSink<T> implements Sink<T> {
  _CallbackSink(this.onData);

  final ValueChanged<T> onData;

  @override
  void add(T data) => onData(data);

  @override
  void close() {}
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

/// 「回到最新输出」。补完历史后视图停在几千行之外，手动拖回去不现实。
class _BackToTailButton extends StatelessWidget {
  const _BackToTailButton({required this.onPressed});

  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return Material(
      color: colors.secondaryContainer.withValues(alpha: 0.92),
      shape: const CircleBorder(),
      clipBehavior: Clip.antiAlias,
      child: InkWell(
        onTap: () {
          HapticFeedback.selectionClick();
          onPressed();
        },
        child: Padding(
          padding: const EdgeInsets.all(8),
          child: Icon(
            Icons.arrow_downward,
            size: 20,
            color: colors.onSecondaryContainer,
          ),
        ),
      ),
    );
  }
}

/// 快捷键栏。键位顺序按 agent 场景排：最左边是打断/切模式这类高频键，方向键
/// 居中，翻页在最右——横向滚动时右侧先被截掉，低频的放那边。
class TerminalShortcutBar extends StatelessWidget {
  const TerminalShortcutBar({super.key, required this.onKey});

  /// 所有键都走这里：编码由终端按其当前模式决定。
  final TerminalKeyHandler onKey;

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 48,
      color: Theme.of(context).colorScheme.surfaceContainer,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
        children: [
          _TerminalTextKey(
            label: 'Esc',
            onPressed: () => onKey(TerminalKey.escape),
          ),
          _TerminalTextKey(
            label: 'Tab',
            onPressed: () => onKey(TerminalKey.tab),
          ),
          // Claude Code 用它切换模式；手机软键盘上没有 Shift+Tab 可按，这一条
          // 只能靠快捷键栏给。
          _TerminalTextKey(
            label: '⇧Tab',
            onPressed: () => onKey(TerminalKey.tab, shift: true),
          ),
          // 多行输入：Enter 提交、Shift+Enter 换行。同样是软键盘按不出来的组合。
          _TerminalTextKey(
            label: '⇧↵',
            tooltip: 'Shift+Enter (new line)',
            onPressed: () => onKey(TerminalKey.enter, shift: true),
          ),
          _TerminalTextKey(
            label: '^C',
            onPressed: () => onKey(TerminalKey.keyC, ctrl: true),
          ),
          _TerminalIconKey(
            icon: Icons.keyboard_arrow_left,
            tooltip: 'Left',
            onPressed: () => onKey(TerminalKey.arrowLeft),
          ),
          _TerminalIconKey(
            icon: Icons.keyboard_arrow_down,
            tooltip: 'Down',
            onPressed: () => onKey(TerminalKey.arrowDown),
          ),
          _TerminalIconKey(
            icon: Icons.keyboard_arrow_up,
            tooltip: 'Up',
            onPressed: () => onKey(TerminalKey.arrowUp),
          ),
          _TerminalIconKey(
            icon: Icons.keyboard_arrow_right,
            tooltip: 'Right',
            onPressed: () => onKey(TerminalKey.arrowRight),
          ),
          _TerminalTextKey(
            label: 'PgUp',
            onPressed: () => onKey(TerminalKey.pageUp),
          ),
          _TerminalTextKey(
            label: 'PgDn',
            onPressed: () => onKey(TerminalKey.pageDown),
          ),
        ],
      ),
    );
  }
}

typedef TerminalKeyHandler =
    void Function(TerminalKey key, {bool shift, bool alt, bool ctrl});

class _TerminalTextKey extends StatelessWidget {
  const _TerminalTextKey({
    required this.label,
    required this.onPressed,
    this.tooltip,
  });

  final String label;
  final String? tooltip;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final button = TextButton(onPressed: onPressed, child: Text(label));
    final message = tooltip;
    return message == null ? button : Tooltip(message: message, child: button);
  }
}

class _TerminalIconKey extends StatelessWidget {
  const _TerminalIconKey({
    required this.icon,
    required this.tooltip,
    required this.onPressed,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    return IconButton(icon: Icon(icon), tooltip: tooltip, onPressed: onPressed);
  }
}
