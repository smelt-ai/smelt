import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:xterm/xterm.dart';

import '../../data/models.dart' show TerminalGeometry;
import '../../data/preferences.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../theme/terminal_theme.dart';
import '../../utils/terminal_links.dart';
import '../../utils/terminal_paste.dart';
import '../../utils/xterm_input_filter.dart';
import 'terminal_chrome.dart';
import 'terminal_shortcut_bar.dart';
import 'terminal_stream.dart';

/// 终端页：运行时转发的 PTY 字节流，xterm 渲染。
///
/// 导航栏是 App 的钢色；终端区域全宽、固定深色（不跟明暗走，见 [SmeltTerminalTheme]）；
/// 底下一排快捷键。连接、重连都在内核里，本页只把流上的事件画出来、把按键送回去。
class TerminalPage extends StatefulWidget {
  const TerminalPage({
    super.key,
    required this.runtimeId,
    required this.sessionId,
  });

  final String runtimeId;
  final String sessionId;

  @override
  State<TerminalPage> createState() => _TerminalPageState();
}

class _TerminalPageState extends State<TerminalPage>
    with WidgetsBindingObserver {
  late final TerminalStream _stream;
  late final PreferencesStore _prefs;
  late Terminal _terminal;
  late final TerminalController _terminalController;
  final TerminalLinkIndex _links = TerminalLinkIndex();
  late GlobalKey<TerminalViewState> _terminalViewKey;
  late Sink<List<int>> _byteSink;
  late XtermInputFilter _inputFilter;
  late final StreamSubscription<TerminalStreamEvent> _eventSubscription;
  late final StreamSubscription<TerminalStreamState> _stateSubscription;
  final FocusNode _terminalFocusNode = FocusNode();
  final ScrollController _terminalScrollController = ScrollController();

  TerminalStreamState _streamState = TerminalStreamState.connecting;
  TerminalNotice? _notice;
  bool _softwareKeyboardEnabled = false;
  bool _softwareKeyboardWasVisible = false;
  bool _terminalGeometryLocked = false;
  bool _replayGeometryLocked = false;
  late double _fontSize;
  int _decoderGeneration = 0;

  /// 快捷键栏上的 Ctrl 已按下：下一个键（栏上的键，或软键盘打的一个字）带上 Ctrl。
  bool _ctrlLatched = false;

  /// 正在按更大的预算重取快照（用户滚到顶触发）。
  bool _loadingMoreHistory = false;

  /// 重取前光标停在「离底多远」的位置。历史是往上长的，底部不动，所以这个距离
  /// 在新快照里仍然指着同一段内容——按它恢复，用户不会被弹走。
  double? _pendingScrollDistanceFromBottom;

  /// 视图已经离开最新输出（用户往上翻了，或者刚补完历史落在中间）。实时输出仍在
  /// 底部推进，得给用户一个回得去的出口。
  bool _awayFromTail = false;
  bool _hasSelection = false;

  /// 当前选区正好是一个可打开的 http(s) 链接时的目标。随选区变化同步更新，
  /// 选区条的「打开链接」只认这个字段，不在点击时重新取选区。
  String? _selectionOpenTarget;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    final model = AppScope.of(context);
    _prefs = model.prefs;
    // 偏好在 runApp 之前就读好了，第一帧就是对的字号：附着前行列就能落定。
    _fontSize = _prefs.value.terminalFontSize;
    _prefs.addListener(_onPrefsChanged);
    _terminalController = TerminalController();
    _terminalController.addListener(_onSelectionChanged);
    _stream = TerminalStream(
      backend: model.backend,
      runtimeId: widget.runtimeId,
      sessionId: widget.sessionId,
    );
    _terminal = _newTerminal();
    _terminalViewKey = GlobalKey<TerminalViewState>();
    _resetDecoder();
    _terminalScrollController.addListener(_handleTerminalScroll);
    _eventSubscription = _stream.events.listen(_handleTerminalEvent);
    _streamState = _stream.state;
    _stateSubscription = _stream.stateStream.listen((state) {
      if (!mounted) return;
      if (!_canWriteIn(state)) _closeSoftwareKeyboard();
      setState(() {
        _streamState = state;
        if (state == TerminalStreamState.ended) _ctrlLatched = false;
      });
    });
  }

  /// 已配对的设备都能写。重连期间也放行：非 fatal 错误也可能只是一次输入没发出去，
  /// 附着本身还在，不能因此把键盘收掉、把人锁在外面；真重连上了会来 `Ready`，那时
  /// 页面本来就要重建终端、收键盘。
  static bool _canWriteIn(TerminalStreamState state) =>
      state == TerminalStreamState.connected ||
      state == TerminalStreamState.reconnecting;

  bool get _canWrite => _canWriteIn(_streamState);

  Terminal _newTerminal({int cols = 80, int rows = 24}) {
    late final Terminal terminal;
    terminal = Terminal(
      // 和 attach 时声明的 scrollback 上限同源：运行时发多了，超出的部分解码完
      // 就会被这个环形缓冲丢掉。
      maxLines: kMaxTerminalScrollbackLines,
      onOutput: _handleTerminalOutput,
      onPrivateOSC: (code, args) => _links.onOsc(terminal, code, args),
      wordSeparators: cliWordSeparators,
    );
    // 关键帧是按 Ready 里的行列做光标寻址的，喂第一个字节之前先把网格定成那个尺寸。
    terminal.resize(cols.clamp(1, 300), rows.clamp(1, 200));
    terminal.onResize = _handleTerminalResize;
    return terminal;
  }

  /// 终端要发给 PTY 的一切（软键盘、蓝牙键盘、快捷键栏、鼠标上报）都从这里出去。
  ///
  /// Ctrl 锁存只作用于「一个字」：软键盘打的单个字符在这里才第一次露面（xterm 直接
  /// `textInput`），只能在出口处补上 Ctrl。字母走 `keyInput`，编码仍由终端按当前模式
  /// 决定；`[\]^_` 走 `charInput`；其余字符没有 Ctrl 形式，原样发出。多字节的输出
  /// （鼠标上报、转义序列）不吃锁存。
  void _handleTerminalOutput(String data) {
    if (_ctrlLatched && data.length == 1) {
      _setCtrlLatched(false);
      final code = data.toLowerCase().codeUnitAt(0);
      const a = 0x61, z = 0x7a;
      if (code >= a && code <= z) {
        final key = TerminalKey.values[TerminalKey.keyA.index + code - a];
        _terminal.keyInput(key, ctrl: true);
        return;
      }
      if (_terminal.charInput(code, ctrl: true)) return;
    }
    _stream.sendInput(data);
  }

  void _setCtrlLatched(bool latched) {
    if (_ctrlLatched == latched || !mounted) return;
    setState(() => _ctrlLatched = latched);
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
          // xterm 里一处异常不能把字节管道拆掉：那样页面就冻在已经解出来的那一段上，
          // 只能退出重进。
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
      case TerminalReadyEvent():
        _closeSoftwareKeyboard();
        // 快照换了一整帧，旧缓冲上的 OSC 8 锚点和选区都不再指向这帧。
        _links.clear();
        _terminalController.clearSelection();
        _terminal = _newTerminal(cols: event.cols, rows: event.rows);
        _terminalViewKey = GlobalKey<TerminalViewState>();
        _resetDecoder();
        if (!mounted) return;
        setState(() {
          _softwareKeyboardEnabled = false;
          _replayGeometryLocked = true;
          // 快照换了一整帧，上一帧的「离开尾巴」结论跟着作废。
          _awayFromTail = false;
          _notice = null;
        });
      case TerminalDataEvent():
        final bytes = _inputFilter.add(event.bytes);
        if (bytes.isNotEmpty) _byteSink.add(bytes);
      case TerminalReplayCompleteEvent():
        if (mounted) {
          setState(() {
            _replayGeometryLocked = false;
            _loadingMoreHistory = false;
            // 重建视图，逼 xterm 重新量一次视口。锁着的这段时间里视口很可能已经
            // 变了——`Ready` 一到就收掉说明条，回放中途也可能弹出说明条——而
            // RenderTerminal 只在「量出来的格子数和上次不同」时才通知终端，那次
            // 不同恰好落在锁里被丢掉，解锁后就再也不会重算。终端于是停在不对的
            // 行数上：全屏 TUI（alt buffer）没有回滚，多出来的行直接变成外层滚动
            // 量，手势全被它吃掉，TUI 内部就再也滚不动了。
            _terminalViewKey = GlobalKey<TerminalViewState>();
          });
        }
        final restore = _pendingScrollDistanceFromBottom;
        _pendingScrollDistanceFromBottom = null;
        if (restore != null) {
          _restoreScrollAfterLayout(restore);
        } else {
          _scrollToTerminalTailAfterLayout(
            decoderGeneration: _decoderGeneration,
          );
        }
      case TerminalErrorEvent():
        if (!mounted) return;
        if (event.fatal) _closeSoftwareKeyboard();
        setState(() {
          _notice = TerminalNotice.error(event.message, fatal: event.fatal);
          if (event.fatal) {
            _replayGeometryLocked = false;
            _loadingMoreHistory = false;
            _pendingScrollDistanceFromBottom = null;
          }
        });
      case TerminalClosedEvent():
        if (!mounted) return;
        _closeSoftwareKeyboard();
        setState(() {
          _replayGeometryLocked = false;
          _loadingMoreHistory = false;
          _pendingScrollDistanceFromBottom = null;
          _notice = const TerminalNotice.closed();
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

  /// 等 RenderTerminal 先布局一次、把新缓冲的滚动范围发布出来，再跟到最底。解码器
  /// 是同步把回放写进去的，但滚动范围要到下一次布局才知道。
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

  // 软键盘 -------------------------------------------------------------------
  //
  // 进页面是「浏览态」：不弹键盘，点终端也不弹（hardwareKeyboardOnly）。开键盘时锁住
  // 终端几何：键盘把视口压矮只是暂时的，不该变成一次 PTY resize（主屏 CLI 会整段重印）；
  // 键盘收起、视口回到原样再解锁，并重建视图清掉 xterm 留下的输入法组字状态。

  void _toggleSoftwareKeyboard() {
    if (!_canWrite) return;
    if (_softwareKeyboardEnabled) {
      _closeSoftwareKeyboard();
      return;
    }
    _enableSoftwareKeyboard();
  }

  void _enableSoftwareKeyboard() {
    if (!_canWrite || _softwareKeyboardEnabled) return;
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
      // xterm 4.0 在 closeKeyboard() 之后还留着输入法组字文本；重建视图清掉这份本地
      // 渲染状态，PTY 不受影响。
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

  // 选区、复制、粘贴、链接 ----------------------------------------------------

  /// 长按拖动会连续改选区但不会经过「无选区」，所以每次通知都要重算。
  void _onSelectionChanged() {
    if (!mounted) return;
    final selection = _terminalController.selection;
    final hasSelection = selection != null;
    final openTarget = selection == null
        ? null
        : openableHttpUrl(_terminal.buffer.getText(selection));
    if (hasSelection == _hasSelection && openTarget == _selectionOpenTarget) {
      return;
    }
    setState(() {
      _hasSelection = hasSelection;
      _selectionOpenTarget = openTarget;
    });
  }

  /// 按下在链接上就把这次点按认领走：xterm 不再弹键盘、不把它当鼠标点击发给 TUI，
  /// 单击完成才打开；变成长按选词就什么都不做。
  VoidCallback? _claimLinkTap(CellOffset cell) {
    // 有选区时这一点是在取消选区，留给 xterm。
    if (_terminalController.selection != null) return null;
    final url = _links.targetAt(_terminal, cell);
    if (url == null) return null;
    return () => unawaited(_openExternalLink(url));
  }

  Future<void> _copyOutput() async {
    final l10n = L10n.of(context);
    final selection = _terminalController.selection;
    final text = terminalClipboardText(_terminal, selection);
    if (text.trim().isEmpty) {
      _showMessage(l10n.terminalNothingToCopy);
      return;
    }
    try {
      await Clipboard.setData(ClipboardData(text: text));
    } catch (_) {
      if (mounted) _showMessage(l10n.terminalCopyFailed);
      return;
    }
    if (!mounted) return;
    HapticFeedback.selectionClick();
    if (selection != null) _terminalController.clearSelection();
    _showMessage(l10n.commonCopied);
  }

  void _selectAll() {
    final buffer = _terminal.buffer;
    if (buffer.height <= 0) return;
    _terminalController.setSelection(
      buffer.createAnchor(0, 0),
      buffer.createAnchor(buffer.viewWidth, buffer.height - 1),
      mode: SelectionMode.line,
    );
  }

  Future<void> _openExternalLink(String url) async {
    final l10n = L10n.of(context);
    final uri = Uri.tryParse(url);
    if (uri == null || openableHttpUrl(url) == null) return;
    try {
      final opened = await launchUrl(uri, mode: LaunchMode.externalApplication);
      if (!opened && mounted) _showMessage(l10n.terminalOpenLinkFailed);
    } catch (_) {
      if (mounted) _showMessage(l10n.terminalOpenLinkFailed);
    }
  }

  void _showMessage(String message) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(message)));
  }

  /// 不走 keyInput(Ctrl+V)：那是 0x16，shell 和 TUI 都不会把它当粘贴。
  Future<void> _pasteClipboard() async {
    if (!_canWrite) return;
    final l10n = L10n.of(context);
    final String? text;
    try {
      text = (await Clipboard.getData(Clipboard.kTextPlain))?.text;
    } catch (_) {
      if (mounted) _showMessage(l10n.terminalPasteFailed);
      return;
    }
    if (!mounted || !_canWrite) return;
    if (text == null || text.isEmpty) {
      _showMessage(l10n.terminalNothingToPaste);
      return;
    }
    _stream.sendInput(
      encodeTerminalPaste(text, bracketed: _terminal.bracketedPasteMode),
    );
  }

  // 快捷键栏 -------------------------------------------------------------------

  /// 编码交给终端本身：它知道对端有没有开 kitty keyboard protocol（决定 Shift+Tab 发
  /// `ESC[Z` 还是 `ESC[9;2u`）和 application cursor 模式（决定方向键发 CSI 还是 SS3），
  /// 也和蓝牙键盘走的是同一条路径，两边不会各编各的。`onOutput` 已经接到流上。
  void _sendKey(
    TerminalKey key, {
    bool shift = false,
    bool alt = false,
    bool ctrl = false,
  }) {
    final latched = _ctrlLatched;
    _setCtrlLatched(false);
    _terminal.keyInput(key, shift: shift, alt: alt, ctrl: ctrl || latched);
  }

  /// 符号键当作打了一个字：Ctrl 锁存在 [_handleTerminalOutput] 里统一处理。
  void _sendText(String text) => _terminal.textInput(text);

  // 字号 ---------------------------------------------------------------------

  void _onPrefsChanged() {
    final size = _prefs.value.terminalFontSize;
    if (!mounted || size == _fontSize) return;
    setState(() => _fontSize = size);
  }

  void _setFontSize(double size) {
    if (size == _fontSize) return;
    HapticFeedback.selectionClick();
    // 偏好先在内存里生效（本页经监听拿到新字号），落盘由偏好自己排队。
    _prefs.update(_prefs.value.copyWith(terminalFontSize: size));
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final theme = SmeltTerminalTheme.dark;
    final notice = _notice;
    return Scaffold(
      backgroundColor: c.ground,
      appBar: AppBar(
        // 深色模式下钢底和终端底几乎同色，一根细线把导航栏和终端分开。
        shape: Border(bottom: BorderSide(color: c.hairline)),
        title: TerminalTitle(
          runtimeId: widget.runtimeId,
          sessionId: widget.sessionId,
          state: _streamState,
        ),
        actions: [
          IconButton(
            tooltip: _hasSelection
                ? l10n.terminalCopySelection
                : l10n.terminalCopyOutput,
            onPressed: _copyOutput,
            icon: const Icon(LucideIcons.copy),
          ),
          TerminalFontSizeButton(
            value: _fontSize,
            steps: Preferences.terminalFontSteps,
            onSelected: _setFontSize,
          ),
          IconButton(
            tooltip: !_canWrite
                ? l10n.terminalKeyboardUnavailable
                : _softwareKeyboardEnabled
                ? l10n.terminalHideKeyboard
                : l10n.terminalShowKeyboard,
            onPressed: _canWrite ? _toggleSoftwareKeyboard : null,
            icon: Icon(
              _softwareKeyboardEnabled
                  ? LucideIcons.keyboardOff
                  : LucideIcons.keyboard,
            ),
          ),
          const SizedBox(width: Space.xs),
        ],
      ),
      body: SafeArea(
        top: false,
        child: Column(
          children: [
            if (notice != null) TerminalNoticeBar(notice: notice),
            Expanded(
              // 终端视口按整格排，边上会留下几像素：那几像素也得是终端底色。
              child: ColoredBox(
                color: theme.background,
                child: Stack(
                  children: [
                    Positioned.fill(child: _buildTerminalView(theme)),
                    // 以下都浮在终端上面，不能进 Column：多出来的高度会改行数，
                    // 主屏 CLI 会因此重印整段对话。
                    ..._buildOverlays(l10n),
                  ],
                ),
              ),
            ),
            // 快捷键栏从进页面起就常驻，直到会话结束：最高频的动作是「看着 agent 跑、
            // 按 ^C 打断」，那时并不需要软键盘。挂在键盘上会逼用户先唤起键盘遮掉半屏，
            // 而且键盘开合还会连带改变终端视口高度，白白触发一轮行列重算。
            // 也不能等连上才出现：它得在 attach 之前就把终端视口压到最终高度，否则回放
            // 完成后栏子上屏又要改一次行数，那就是一次多余的 PTY resize。重连期间同理。
            if (_streamState != TerminalStreamState.ended)
              TerminalShortcutBar(
                onKey: _sendKey,
                onText: _sendText,
                onPaste: _pasteClipboard,
                ctrlLatched: _ctrlLatched,
                onToggleCtrl: () => _setCtrlLatched(!_ctrlLatched),
              ),
          ],
        ),
      ),
    );
  }

  List<Widget> _buildOverlays(L10n l10n) {
    final openTarget = _selectionOpenTarget;
    final bottom = <Widget>[
      if (_awayFromTail)
        Align(
          alignment: Alignment.centerRight,
          child: TerminalJumpToLatest(
            onPressed: () {
              HapticFeedback.selectionClick();
              _jumpToTail();
            },
          ),
        ),
      if (_awayFromTail && _hasSelection) const SizedBox(height: Space.sm),
      if (_hasSelection)
        Align(
          alignment: Alignment.centerLeft,
          child: TerminalSelectionBar(
            onCopy: _copyOutput,
            onSelectAll: _selectAll,
            onPaste: _canWrite ? _pasteClipboard : null,
            onOpen: openTarget == null
                ? null
                : () => unawaited(_openExternalLink(openTarget)),
          ),
        ),
    ];
    return [
      if (_loadingMoreHistory)
        Positioned(
          top: 0,
          left: 0,
          right: 0,
          child: Theme(
            data: terminalOverlayTheme,
            child: Semantics(
              label: l10n.terminalLoadingHistory,
              child: const LinearProgressIndicator(minHeight: Space.xxs),
            ),
          ),
        ),
      if (bottom.isNotEmpty)
        Positioned(
          left: Space.md,
          right: Space.md,
          bottom: Space.md,
          child: Theme(
            data: terminalOverlayTheme,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: bottom,
            ),
          ),
        ),
    ];
  }

  Widget _buildTerminalView(TerminalTheme theme) {
    return TerminalView(
      _terminal,
      key: _terminalViewKey,
      controller: _terminalController,
      focusNode: _terminalFocusNode,
      scrollController: _terminalScrollController,
      autofocus: false,
      readOnly: !_canWrite,
      hardwareKeyboardOnly: !_softwareKeyboardEnabled,
      autoResize: !_terminalGeometryLocked && !_replayGeometryLocked,
      deleteDetection: true,
      simulateScroll: true,
      claimTap: _claimLinkTap,
      padding: SmeltTerminalTheme.padding,
      theme: theme,
      textStyle: SmeltTerminalTheme.style(_fontSize),
      keyboardAppearance: Brightness.dark,
    );
  }

  @override
  void dispose() {
    _prefs.removeListener(_onPrefsChanged);
    _terminalController.removeListener(_onSelectionChanged);
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
    _terminalController.dispose();
    _links.clear();
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
