import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart' hide ConnectionState;
import 'package:flutter/services.dart';
import 'package:image_picker/image_picker.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/agent_glyph.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../../ui/station.dart';
import '../../ui/time.dart';
import '../../utils/image_processing.dart';
import 'composer.dart';
import 'conversation_logic.dart';
import 'conversation_sheets.dart';
import 'entry_views.dart';
import 'ticket_dock.dart';
import 'tool_views.dart';

/// 离列表顶端（最早的消息）还剩这么多像素时翻更早的一页。
const _loadOlderThreshold = 240.0;

/// 一条消息最多带几张图、每张多大。和桌面输入栏一致。
const _maxImages = 4;
const _maxImageBytes = 5 * 1024 * 1024;

/// 草稿落盘的防抖。不是动效，只是别每敲一个字就写一次库；离开页面时会立刻补写。
const _draftDebounce = Duration(seconds: 1);

/// 对话页：一段会话的全部内容、停靠在输入框上方的待处理单据、输入胶囊。
///
/// 订阅在 initState 里起、dispose 时停（内核支持同时订阅多个会话，平板上列表和对话
/// 可以同时开着）。快照到达时把条目分好块缓存在状态上，build 只读缓存。
class ConversationPage extends StatefulWidget {
  const ConversationPage({
    super.key,
    required this.runtimeId,
    required this.sessionId,
  });

  final String runtimeId;
  final String sessionId;

  @override
  State<ConversationPage> createState() => _ConversationPageState();
}

/// 列表尾巴（计划 + 当前状态）与「加载更早」两行的占位。
enum _Special { tail, older }

enum _More { settings, skills, copyInfo }

class _ConversationPageState extends State<ConversationPage> {
  late final AppModel _model;
  StreamSubscription<ConversationEvent>? _subscription;

  // 对话 ---------------------------------------------------------------------
  ConversationView? _view;
  bool _cached = false;
  int? _savedAtMs;

  /// 运行时接不上这段对话 / 订阅出错。非空就别再转圈。
  ConversationEventKind? _problemKind;
  String? _problemMessage;

  /// 倒序的列表行：[_Special.tail]、各块从新到旧、[_Special.older]。
  List<Object> _rows = const [];
  Map<String, int> _rowIndex = const {};
  bool _loadingOlder = false;

  /// 这场对话装了哪些技能。null = 引擎没有技能这个概念（或还没问到），不画入口。
  List<SessionSkill>? _skills;

  // 停靠单据 -----------------------------------------------------------------
  /// 在这里批过、内核已收下的审批：新快照到之前先拿下，别让人批两次。
  final Set<String> _answeredApprovals = {};
  String? _answeredElicitation;
  DockItem? _dock;
  bool _dockCollapsed = false;

  // 滚动 ---------------------------------------------------------------------
  final _scroll = ScrollController();

  /// 在不在最新处。只有「回到最新」按钮听它，滚动时不重建整个列表。
  final _atLatest = ValueNotifier<bool>(true);

  // 输入 ---------------------------------------------------------------------
  final _text = TextEditingController();
  final _focus = FocusNode();
  final _picker = ImagePicker();
  final List<ImageInput> _images = [];

  /// 输入框上次的内容。控制器在光标移动时也会通知，只有内容变了才算编辑。
  String _draftText = '';
  bool _restoring = true;
  bool _sending = false;
  Timer? _draftTimer;

  /// 上一次没等到确认的发送。草稿没改过就重发时沿用这个 id，运行时按它去重；改过内容
  /// 就是一次新的提交，id 随之作废。
  String? _unconfirmedRequestId;
  String? _sendError;

  /// 本页关掉的提示。快照里提示没了就忘掉，新提示照常出来。
  int? _dismissedNoticeId;

  // 连接 ---------------------------------------------------------------------
  /// 这台机器的连接停过（disconnected）：内核的订阅随之停了，连回来要重新订。
  bool _linkStopped = false;

  @override
  void initState() {
    super.initState();
    _model = AppScope.of(context);
    _text.addListener(_onTextChanged);
    _scroll.addListener(_onScroll);
    _model.fleet.addListener(_onFleet);
    _linkStopped =
        _model.fleet.runtime(widget.runtimeId)?.status.state ==
        ConnectionState.disconnected;
    _subscribe();
    _model.backend.markRead(widget.runtimeId, widget.sessionId);
    unawaited(_loadSkills());
    unawaited(_restoreDraft());
  }

  @override
  void dispose() {
    // 防抖还没写下去的草稿现在写：控制器马上要释放，先同步取好内容。
    if (_draftTimer?.isActive ?? false) {
      _draftTimer!.cancel();
      unawaited(
        _persistDraft(
          _text.text,
          List.of(_images),
          _unconfirmedRequestId,
        ).catchError((Object _) {}),
      );
    }
    // 取消流就是退订：只退这一页自己的订阅，同一会话的别的页面不受影响。
    _subscription?.cancel();
    // 看着它跑完再离开，也算读过了。
    _model.backend.markRead(widget.runtimeId, widget.sessionId);
    _model.fleet.removeListener(_onFleet);
    _text.dispose();
    _focus.dispose();
    _scroll.dispose();
    _atLatest.dispose();
    super.dispose();
  }

  // 订阅 ---------------------------------------------------------------------

  void _subscribe() {
    _subscription?.cancel();
    _subscription = _model.backend
        .watchConversation(widget.runtimeId, widget.sessionId)
        .listen(
          _onEvent,
          onError: (Object error) {
            if (!mounted) return;
            setState(() {
              _problemKind = ConversationEventKind.error;
              _problemMessage = describeError(error);
              _loadingOlder = false;
            });
          },
        );
  }

  void _retry() {
    setState(() {
      _problemKind = null;
      _problemMessage = null;
    });
    _subscribe();
  }

  void _onFleet() {
    final state = _model.fleet.runtime(widget.runtimeId)?.status.state;
    if (state == ConnectionState.disconnected) _linkStopped = true;
    if (state == ConnectionState.connected && _linkStopped) {
      // 只是后台重连（reconnecting）的话内核自己会续订；停过就要再订一次。
      _linkStopped = false;
      _subscribe();
      unawaited(_loadSkills());
    }
  }

  void _onEvent(ConversationEvent event) {
    if (!mounted) return;
    switch (event.kind) {
      case ConversationEventKind.snapshot:
        final view = event.view;
        if (view == null) return;
        final previousOffset = _view?.entriesOffset;
        setState(() {
          _view = view;
          _cached = event.cached;
          _savedAtMs = event.savedAtMs;
          _problemKind = null;
          _problemMessage = null;
          if (previousOffset == null ||
              view.entriesOffset < previousOffset ||
              !view.hasMoreBefore) {
            _loadingOlder = false;
          }
          // 审批从视图里消失了，就不用再记着「在这里批过」。
          _answeredApprovals.removeWhere(
            (id) => !view.pendingPermissions.any((p) => p.toolCallId == id),
          );
          if (view.pendingElicitation == null) _answeredElicitation = null;
          if (view.notice == null) _dismissedNoticeId = null;
          _rebuildRows();
          _refreshDock();
        });
        WidgetsBinding.instance.addPostFrameCallback((_) => _afterSnapshot());
      case ConversationEventKind.unavailable:
      case ConversationEventKind.error:
        setState(() {
          _problemKind = event.kind;
          _problemMessage = event.message;
          _loadingOlder = false;
        });
    }
  }

  void _rebuildRows() {
    final view = _view!;
    final blocks = buildBlocks(view);
    final rows = <Object>[
      if (view.plan.isNotEmpty || TurnStatusLine.shows(view)) _Special.tail,
      ...blocks.reversed,
      if (view.hasMoreBefore) _Special.older,
    ];
    _rows = rows;
    _rowIndex = {for (final (i, row) in rows.indexed) _rowKey(row): i};
  }

  static String _rowKey(Object row) => switch (row) {
    ConversationBlock b => b.key,
    _Special s => s.name,
    _ => '$row',
  };

  /// 停靠位上该放哪件事。快照来自离线缓存时不放：上面的事可能早处理完了，
  /// 拿旧数据去批是错的。
  void _refreshDock() {
    final view = _view;
    DockItem? next;
    if (view != null && !_cached) {
      final pending = view.pendingPermissions;
      for (final (i, p) in pending.indexed) {
        if (_answeredApprovals.contains(p.toolCallId)) continue;
        next = DockItem.approval(p, position: i + 1, count: pending.length);
        break;
      }
      final e = view.pendingElicitation;
      if (next == null && e != null && _answeredElicitation != e.message) {
        next = DockItem.question(e);
      }
    }
    if (next?.key != _dock?.key) _dockCollapsed = false;
    _dock = next;
  }

  void _onDockAnswered(DockItem item) {
    if (!mounted) return;
    setState(() {
      if (item.permission case final p?) {
        _answeredApprovals.add(p.toolCallId);
      } else if (item.elicitation case final e?) {
        _answeredElicitation = e.message;
      }
      _refreshDock();
    });
  }

  Future<void> _loadSkills() async {
    try {
      final skills = await _model.backend.listSkills(
        widget.runtimeId,
        widget.sessionId,
      );
      if (!mounted) return;
      setState(() => _skills = skills.supported ? skills.skills : null);
    } catch (_) {
      // 问不到就不画入口：技能只是辅助信息，不该为它打扰用户。
    }
  }

  // 滚动 ---------------------------------------------------------------------

  void _onScroll() {
    if (!_scroll.hasClients) return;
    final p = _scroll.position;
    _atLatest.value = isNearLatest(p.pixels, p.minScrollExtent);
    if (p.maxScrollExtent - p.pixels <= _loadOlderThreshold) _loadOlder();
  }

  /// 新快照画出来之后：在最新处就跟着走；内容不够一屏、翻不动时直接往前翻一页。
  void _afterSnapshot() {
    if (!mounted || !_scroll.hasClients) return;
    final p = _scroll.position;
    if (_atLatest.value && p.pixels != p.minScrollExtent) _jumpToLatest();
    if (p.maxScrollExtent - p.pixels <= _loadOlderThreshold) _loadOlder();
  }

  void _loadOlder() {
    if (_loadingOlder || _view?.hasMoreBefore != true) return;
    setState(() => _loadingOlder = true);
    // 翻到的页从订阅流推回来；没翻（已在翻、前面没了）就把加载圈收掉。
    _model.backend
        .loadOlder(widget.runtimeId, widget.sessionId)
        .then(
          (started) {
            if (!started && mounted) setState(() => _loadingOlder = false);
          },
          onError: (Object _) {
            if (mounted) setState(() => _loadingOlder = false);
          },
        );
  }

  void _jumpToLatest() {
    if (!_scroll.hasClients) return;
    final target = _scroll.position.minScrollExtent;
    final duration = Motion.resolve(context, Motion.standard);
    if (duration == Duration.zero) {
      _scroll.jumpTo(target);
    } else {
      _scroll.animateTo(target, duration: duration, curve: Motion.easeOut);
    }
  }

  // 动作 ---------------------------------------------------------------------

  void _showError(Object error) {
    if (!mounted) return;
    ScaffoldMessenger.maybeOf(context)?.showSnackBar(
      SnackBar(
        content: Text(
          L10n.of(context).conversationActionFailed(describeError(error)),
        ),
      ),
    );
  }

  /// 不需要结果的动作（选模型、停止……）：成功的效果从视图流回来，失败才提示。
  void _fire(Future<void> request) {
    request.catchError(_showError);
  }

  void _setConfig(String configId, String valueId, {bool? boolean}) => _fire(
    _model.backend.setConfigOption(
      widget.runtimeId,
      widget.sessionId,
      configId: configId,
      valueId: valueId,
      boolean: boolean,
    ),
  );

  void _stop() {
    HapticFeedback.mediumImpact();
    _fire(_model.backend.cancelTurn(widget.runtimeId, widget.sessionId));
  }

  Future<ImageData> _loadImage(ImageRef image, {int? maxEdge}) =>
      _model.backend.loadImage(
        widget.runtimeId,
        widget.sessionId,
        entry: image.entry,
        digest: image.digest,
        maxEdge: maxEdge,
      );

  Future<void> _openCommands() async {
    final view = _view;
    if (view == null) return;
    final name = await showCommandsSheet(
      context,
      commands: view.availableCommands,
      skills: _skills,
    );
    if (name == null || !mounted) return;
    _text.text = '/$name ';
    _text.selection = TextSelection.collapsed(offset: _text.text.length);
    _focus.requestFocus();
  }

  void _copyInfo() {
    final l10n = L10n.of(context);
    final runtime = _model.fleet.runtime(widget.runtimeId);
    final session = _model.catalog.session(widget.runtimeId, widget.sessionId);
    final lines = [
      _title(l10n),
      [
        ?runtime?.name,
        if (session != null) agentDisplayName(session.agent),
        ?session?.placeLabel,
      ].join(' · '),
      ?session?.cwd,
      l10n.conversationInfoSession(widget.sessionId),
    ];
    Clipboard.setData(ClipboardData(text: lines.join('\n')));
    ScaffoldMessenger.maybeOf(
      context,
    )?.showSnackBar(SnackBar(content: Text(l10n.commonCopied)));
  }

  // 草稿与发送 ---------------------------------------------------------------

  Future<void> _restoreDraft() async {
    MessageDraft? draft;
    try {
      draft = await _model.backend.loadDraft(
        widget.runtimeId,
        widget.sessionId,
      );
    } catch (_) {
      draft = null;
    }
    if (!mounted) return;
    // 草稿还没读回来用户就开始打字了：以手上的为准。
    if (draft != null && _text.text.isEmpty && _images.isEmpty) {
      _text.text = draft.content;
      _images.addAll(draft.images);
      // 草稿上还挂着请求 id：上次发出去没等到确认（可能 App 被杀了）。
      if (draft.requestId case final id?) {
        _unconfirmedRequestId = id;
        _sendError = L10n.of(context).conversationUnconfirmed;
      }
    }
    _draftText = _text.text;
    _restoring = false;
    setState(() {});
  }

  void _onTextChanged() {
    if (_restoring || _text.text == _draftText) return;
    _draftText = _text.text;
    _markEdited();
  }

  /// 草稿被改过：之前的错误提示作废，没确认的请求 id 也不再适用。
  void _markEdited() {
    if (_sendError != null || _unconfirmedRequestId != null) {
      setState(() {
        _sendError = null;
        _unconfirmedRequestId = null;
      });
    }
    if (_restoring || _sending) return;
    _draftTimer?.cancel();
    _draftTimer = Timer(_draftDebounce, () {
      unawaited(
        _persistDraft(
          _text.text,
          List.of(_images),
          _unconfirmedRequestId,
        ).catchError((Object _) {}),
      );
    });
  }

  /// 存草稿：内容、图、请求 id 都由调用方取好（页面可能正在释放）。空草稿就是删掉。
  Future<void> _persistDraft(
    String content,
    List<ImageInput> images,
    String? requestId,
  ) {
    final backend = _model.backend;
    if (content.trim().isEmpty && images.isEmpty) {
      return backend.clearDraft(widget.runtimeId, widget.sessionId);
    }
    return backend.saveDraft(
      widget.runtimeId,
      widget.sessionId,
      MessageDraft(content: content, images: images, requestId: requestId),
    );
  }

  Future<bool> _confirmResend() async {
    final l10n = L10n.of(context);
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.conversationResendTitle),
        content: Text(l10n.conversationResendBody),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.commonCancel),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: Text(l10n.conversationResendConfirm),
          ),
        ],
      ),
    );
    return ok == true;
  }

  Future<void> _send() async {
    final content = _text.text;
    final text = content.trim();
    if (_sending || (text.isEmpty && _images.isEmpty)) return;
    final unconfirmed = _unconfirmedRequestId;
    if (unconfirmed != null && !await _confirmResend()) return;
    if (!mounted) return;

    final l10n = L10n.of(context);
    final backend = _model.backend;
    _draftTimer?.cancel();
    // 没确认的那次沿用原 id：运行时按它去重，真到过的话不会再发一遍。
    final requestId = unconfirmed ?? backend.newRequestId();
    final images = List<ImageInput>.of(_images);
    setState(() {
      _sending = true;
      _sendError = null;
    });
    HapticFeedback.lightImpact();

    // 先把草稿连同请求 id 落盘：App 在等确认时被杀，下次打开还知道这条没确认。
    try {
      await _persistDraft(content, images, requestId);
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _sending = false;
        _sendError = l10n.conversationDraftSaveFailed(describeError(error));
      });
      return;
    }

    try {
      await backend.sendMessage(
        widget.runtimeId,
        widget.sessionId,
        requestId: requestId,
        text: text,
        images: images,
      );
    } on SendMessageError catch (error) {
      if (error.kind == SendFailureKind.rejected) {
        // 运行时明确没收下：草稿原样留着、去掉请求 id，下次是一次新的提交。
        await _persistDraft(content, images, null).catchError((Object _) {});
        if (!mounted) return;
        setState(() {
          _sending = false;
          _unconfirmedRequestId = null;
          _sendError = l10n.conversationRejected(error.message);
        });
      } else {
        _markUnconfirmed(requestId, l10n);
      }
      return;
    } catch (_) {
      // 意料之外的失败说不清有没有送到，按「没确认」处理。
      _markUnconfirmed(requestId, l10n);
      return;
    }

    // 送到了：草稿删掉（页面关了也要删）。发送途中输入框是只读的，草稿就是刚发的这条。
    await backend
        .clearDraft(widget.runtimeId, widget.sessionId)
        .catchError((Object _) {});
    if (!mounted) return;
    _restoring = true;
    _text.clear();
    _draftText = '';
    _restoring = false;
    setState(() {
      _sending = false;
      _unconfirmedRequestId = null;
      _sendError = null;
      _images.clear();
    });
    _atLatest.value = true;
    WidgetsBinding.instance.addPostFrameCallback((_) => _jumpToLatest());
  }

  /// 可能已经送达：草稿连同请求 id 留着，重发要先确认。
  void _markUnconfirmed(String requestId, L10n l10n) {
    if (!mounted) return;
    setState(() {
      _sending = false;
      _unconfirmedRequestId = requestId;
      _sendError = l10n.conversationUnconfirmed;
    });
  }

  Future<void> _pickImages() async {
    final l10n = L10n.of(context);
    final messenger = ScaffoldMessenger.maybeOf(context);
    final remaining = _maxImages - _images.length;
    if (remaining <= 0) return;
    try {
      final files = await _picker.pickMultiImage(
        limit: remaining,
        maxWidth: 2048,
        maxHeight: 2048,
        imageQuality: 85,
        requestFullMetadata: false,
      );
      var skipped = 0;
      final picked = <ImageInput>[];
      for (final file in files.take(remaining)) {
        var bytes = await file.readAsBytes();
        var mime = file.mimeType ?? _mimeFromName(file.name);
        // 相机照片带 EXIF 方向，运行时那边不一定认：在手机上摆正并去掉 EXIF。
        if (mime == 'image/jpeg' || mime == 'image/heic') {
          final normalized = await compute(normalizeJpegOrientation, bytes);
          if (normalized == null) {
            skipped++;
            continue;
          }
          bytes = normalized;
          mime = 'image/jpeg';
        }
        if (bytes.length > _maxImageBytes) {
          skipped++;
          continue;
        }
        picked.add(ImageInput(mime: mime, bytes: bytes));
      }
      if (!mounted) return;
      if (picked.isNotEmpty) {
        setState(() => _images.addAll(picked));
        _markEdited();
      }
      if (skipped > 0) {
        messenger?.showSnackBar(
          SnackBar(content: Text(l10n.conversationImagesSkipped(skipped))),
        );
      }
    } catch (error) {
      messenger?.showSnackBar(
        SnackBar(
          content: Text(l10n.conversationAttachFailed(describeError(error))),
        ),
      );
    }
  }

  static String _mimeFromName(String name) {
    final lower = name.toLowerCase();
    if (lower.endsWith('.png')) return 'image/png';
    if (lower.endsWith('.gif')) return 'image/gif';
    if (lower.endsWith('.webp')) return 'image/webp';
    if (lower.endsWith('.heic') || lower.endsWith('.heif')) return 'image/heic';
    return 'image/jpeg';
  }

  void _removeImage(int index) {
    setState(() => _images.removeAt(index));
    _markEdited();
  }

  // 界面 ---------------------------------------------------------------------

  String _title(L10n l10n) {
    final fromView = _view?.sessionTitle?.trim();
    if (fromView != null && fromView.isNotEmpty) return fromView;
    final fromList = _model.catalog
        .session(widget.runtimeId, widget.sessionId)
        ?.title
        .trim();
    if (fromList != null && fromList.isNotEmpty) return fromList;
    return l10n.conversationUntitled;
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final view = _view;
    final hasSettings =
        view != null &&
        ((view.model?.currentName.isNotEmpty ?? false) ||
            view.configOptions.isNotEmpty);
    return ConversationIdentity(
      runtimeId: widget.runtimeId,
      sessionId: widget.sessionId,
      child: Scaffold(
        backgroundColor: c.ground,
        appBar: AppBar(
          toolbarHeight: _titleBarHeight(context),
          title: ListenableBuilder(
            listenable: Listenable.merge([_model.fleet, _model.catalog]),
            builder: (context, _) => _TitleBlock(
              title: _title(l10n),
              runtime: _model.fleet.runtime(widget.runtimeId),
              runtimeId: widget.runtimeId,
              session: _model.catalog.session(
                widget.runtimeId,
                widget.sessionId,
              ),
            ),
          ),
          actions: [
            PopupMenuButton<_More>(
              tooltip: l10n.commonMore,
              icon: const Icon(LucideIcons.ellipsis),
              onSelected: (action) => switch (action) {
                _More.settings => showSettingsSheet(
                  context,
                  view: _view!,
                  set: _setConfig,
                ),
                _More.skills => showSkillsSheet(context, skills: _skills ?? []),
                _More.copyInfo => _copyInfo(),
              },
              itemBuilder: (context) => [
                if (hasSettings)
                  _menuItem(
                    _More.settings,
                    LucideIcons.slidersHorizontal,
                    l10n.conversationModelAndSettings,
                  ),
                if (_skills != null)
                  _menuItem(
                    _More.skills,
                    LucideIcons.sparkles,
                    l10n.conversationSkills,
                  ),
                _menuItem(
                  _More.copyInfo,
                  LucideIcons.copy,
                  l10n.conversationCopyInfo,
                ),
              ],
            ),
            const SizedBox(width: Space.xs),
          ],
        ),
        body: Column(
          children: [
            _banner(context),
            Expanded(child: _body(context)),
            _bottom(context),
          ],
        ),
      ),
    );
  }

  /// 标题两行（会话标题 + 机器 · agent · 项目），跟随系统字号长高，大字号下不截断。
  double _titleBarHeight(BuildContext context) {
    final t = GalleyText.of(context);
    final scaler = MediaQuery.textScalerOf(context);
    final titleStyle =
        Theme.of(context).appBarTheme.titleTextStyle ?? t.headline;
    final title =
        scaler.scale(titleStyle.fontSize ?? t.headline.fontSize!) *
        (titleStyle.height ?? t.headline.height ?? 1.3);
    final sub =
        scaler.scale(t.caption.fontSize ?? 12) * (t.caption.height ?? 1.3);
    final needed = title + sub + Space.lg;
    return needed > kToolbarHeight ? needed : kToolbarHeight;
  }

  PopupMenuItem<_More> _menuItem(_More value, IconData icon, String label) =>
      PopupMenuItem(
        value: value,
        child: Row(
          children: [
            Icon(icon, size: Strokes.icon),
            const SizedBox(width: Space.md),
            Flexible(child: Text(label)),
          ],
        ),
      );

  /// 顶部一条诚实的说明：快照来自缓存、机器没连上、或订阅出了问题。
  Widget _banner(BuildContext context) {
    return ListenableBuilder(
      listenable: _model.fleet,
      builder: (context, _) {
        final c = GalleyColors.of(context);
        final t = GalleyText.of(context);
        final l10n = L10n.of(context);
        final runtime = _model.fleet.runtime(widget.runtimeId);
        String? text;
        var icon = LucideIcons.cloudOff;
        var retry = false;
        if (_problemKind != null && _view != null) {
          text = _problemText(l10n);
          icon = LucideIcons.circleAlert;
          retry = true;
        } else if (_cached) {
          final saved = _savedAtMs;
          text = saved == null
              ? l10n.conversationCachedSaved
              : l10n.machineStatusCached(
                  cachedStatusLabel(l10n, runtime?.status),
                  formatClock(DateTime.fromMillisecondsSinceEpoch(saved)),
                );
        } else if (runtime != null && !runtime.online) {
          text = linkLabel(l10n, runtime.status);
        }
        if (text == null) return const SizedBox.shrink();
        return Semantics(
          liveRegion: true,
          child: DecoratedBox(
            decoration: BoxDecoration(
              color: c.shelf,
              border: Border(bottom: BorderSide(color: c.hairline)),
            ),
            child: ConstrainedBox(
              constraints: const BoxConstraints(minHeight: Space.touch),
              child: Padding(
                padding: const EdgeInsets.only(
                  left: Space.page,
                  right: Space.xs,
                ),
                child: Row(
                  children: [
                    Icon(icon, size: Strokes.iconSmall, color: c.inkMuted),
                    const SizedBox(width: Space.sm),
                    Expanded(
                      child: Text(
                        text,
                        style: t.note.copyWith(color: c.inkMuted),
                      ),
                    ),
                    if (retry)
                      TextButton(
                        onPressed: _retry,
                        child: Text(l10n.commonRetry),
                      ),
                  ],
                ),
              ),
            ),
          ),
        );
      },
    );
  }

  String _problemText(L10n l10n) {
    final message = _problemMessage?.trim();
    if (message != null && message.isNotEmpty) return message;
    return _problemKind == ConversationEventKind.unavailable
        ? l10n.conversationUnavailableDefault
        : l10n.conversationErrorDefault;
  }

  Widget _body(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final view = _view;
    if (view == null) {
      if (_problemKind != null) {
        return Center(
          child: SingleChildScrollView(
            child: EmptyNote(
              icon: LucideIcons.cloudOff,
              title: l10n.conversationUnavailableTitle,
              body: _problemText(l10n),
              action: GalleyButton.solid(
                label: l10n.commonRetry,
                onPressed: _retry,
              ),
            ),
          ),
        );
      }
      return Center(
        child: Semantics(
          liveRegion: true,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const CircularProgressIndicator.adaptive(),
              const SizedBox(height: Space.md),
              Text(
                l10n.conversationLoading,
                style: t.note.copyWith(color: c.inkMuted),
              ),
            ],
          ),
        ),
      );
    }

    final empty =
        !_rows.any((r) => r is ConversationBlock) && !view.hasMoreBefore;
    if (empty) {
      return Column(
        children: [
          Expanded(
            child: Center(
              child: SingleChildScrollView(
                child: EmptyNote(
                  icon: LucideIcons.messageSquare,
                  title: l10n.conversationEmptyTitle,
                  body: l10n.conversationEmptyBody,
                ),
              ),
            ),
          ),
          if (_rows.contains(_Special.tail))
            _Reading(
              padding: const EdgeInsets.only(bottom: Space.sm),
              child: _tail(view),
            ),
        ],
      );
    }

    return Stack(
      children: [
        Positioned.fill(
          child: ListView.builder(
            controller: _scroll,
            reverse: true,
            keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
            padding: const EdgeInsets.symmetric(vertical: Space.md),
            itemCount: _rows.length,
            findChildIndexCallback: (key) =>
                key is ValueKey<String> ? _rowIndex[key.value] : null,
            itemBuilder: (context, index) {
              final row = _rows[index];
              return KeyedSubtree(
                key: ValueKey(_rowKey(row)),
                child: _row(view, row),
              );
            },
          ),
        ),
        Positioned(
          left: 0,
          right: 0,
          bottom: Space.md,
          child: ValueListenableBuilder<bool>(
            valueListenable: _atLatest,
            builder: (context, atLatest, _) => IgnorePointer(
              ignoring: atLatest,
              child: AnimatedOpacity(
                opacity: atLatest ? 0 : 1,
                duration: Motion.resolve(context, Motion.standard),
                curve: Motion.easeOut,
                child: Center(
                  child: _JumpToLatest(
                    key: const ValueKey('jump-to-latest'),
                    onPressed: _jumpToLatest,
                    visible: !atLatest,
                  ),
                ),
              ),
            ),
          ),
        ),
      ],
    );
  }

  Widget _row(ConversationView view, Object row) {
    final l10n = L10n.of(context);
    return switch (row) {
      _Special.tail => _Reading(
        padding: const EdgeInsets.only(top: Space.sm),
        child: _tail(view),
      ),
      _Special.older => _Reading(
        padding: const EdgeInsets.symmetric(vertical: Space.sm),
        child: Center(
          child: _loadingOlder
              ? const SizedBox.square(
                  dimension: Space.touch,
                  child: Center(child: CircularProgressIndicator.adaptive()),
                )
              : TextButton.icon(
                  onPressed: _loadOlder,
                  icon: const Icon(
                    LucideIcons.history,
                    size: Strokes.iconSmall,
                  ),
                  label: Text(l10n.conversationLoadEarlier),
                ),
        ),
      ),
      ToolGroupBlock(:final calls) => _Reading(
        child: calls.length == 1
            ? ToolCallRow(call: calls.single, loadImage: _loadImage)
            : ToolGroup(calls: calls, loadImage: _loadImage),
      ),
      EntryBlock(:final entry, :final isFinal) => _entry(view, entry, isFinal),
      _ => const SizedBox.shrink(),
    };
  }

  Widget _entry(ConversationView view, ConversationEntry entry, bool isFinal) {
    final l10n = L10n.of(context);
    return switch (entry.kind) {
      EntryKind.user => _Reading(
        padding: const EdgeInsets.only(top: Space.xl, bottom: Space.md),
        child: UserMessage(entry: entry, loadImage: _loadImage),
      ),
      EntryKind.assistant when entry.thought => _Reading(
        child: ThoughtBlock(text: entry.text),
      ),
      EntryKind.assistant => _Reading(
        padding: const EdgeInsets.symmetric(vertical: Space.sm),
        child: AssistantMessage(
          text: entry.text,
          isFinal: isFinal,
          durationMs: isFinal ? view.lastTurnDurationMs : null,
        ),
      ),
      EntryKind.completion => _Reading(
        padding: const EdgeInsets.symmetric(vertical: Space.sm),
        child: CompletionMessage(
          summary: entry.text,
          status: entry.toolCall?.status ?? ToolStatus.completed,
          isFinal: isFinal,
        ),
      ),
      EntryKind.interrupted => _Reading(
        child: ConversationDivider(label: l10n.conversationInterrupted),
      ),
      EntryKind.divider => _Reading(
        child: ConversationDivider(label: entry.text),
      ),
      EntryKind.taskNote => _Reading(child: TaskNoteRow(entry: entry)),
      EntryKind.toolCall => const SizedBox.shrink(),
    };
  }

  Widget _tail(ConversationView view) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    mainAxisSize: MainAxisSize.min,
    children: [
      if (view.plan.isNotEmpty) ...[
        PlanChecklist(plan: view.plan),
        const SizedBox(height: Space.xs),
      ],
      if (TurnStatusLine.shows(view)) TurnStatusLine(view: view),
    ],
  );

  Widget _bottom(BuildContext context) {
    final l10n = L10n.of(context);
    final view = _view;
    return SafeArea(
      top: false,
      minimum: const EdgeInsets.only(bottom: Space.sm),
      child: _Reading(
        padding: const EdgeInsets.only(top: Space.xs),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            TicketDock(
              runtimeId: widget.runtimeId,
              sessionId: widget.sessionId,
              item: _dock,
              collapsed: _dockCollapsed,
              onToggleCollapsed: () =>
                  setState(() => _dockCollapsed = !_dockCollapsed),
              onAnswered: _onDockAnswered,
            ),
            ListenableBuilder(
              listenable: Listenable.merge([_model.fleet, _model.catalog]),
              builder: (context, _) {
                final runtime = _model.fleet.runtime(widget.runtimeId);
                final online = runtime?.online ?? false;
                final session = _model.catalog.session(
                  widget.runtimeId,
                  widget.sessionId,
                );
                final phase = view?.phase;
                final accepts = phase?.acceptsPrompt ?? false;
                final waiting =
                    _dock != null ||
                    phase == ConversationPhase.awaitingApproval ||
                    phase == ConversationPhase.waitingForUser;
                final hint = !online
                    ? l10n.conversationHintOffline
                    : _sending
                    ? l10n.conversationHintSending
                    : view == null
                    ? l10n.commonLoading
                    : phase == ConversationPhase.dead
                    ? l10n.conversationHintEnded
                    : waiting
                    ? l10n.conversationHintTicket
                    : l10n.conversationHint(
                        agentDisplayName(session?.agent ?? ''),
                      );
                return Composer(
                  controller: _text,
                  focusNode: _focus,
                  hint: hint,
                  canSend: online && view != null && accepts && !_sending,
                  working: online && (phase?.isWorking ?? false),
                  sending: _sending,
                  readOnly: _sending,
                  images: _images,
                  onSend: _send,
                  onStop: _stop,
                  onRemoveImage: _removeImage,
                  onAttach: view?.supportsImage == true ? _pickImages : null,
                  onCommands:
                      view != null &&
                          (view.availableCommands.isNotEmpty || _skills != null)
                      ? _openCommands
                      : null,
                  chips: view == null ? const [] : _chips(view, l10n),
                  error: _sendError,
                  notice: _visibleNotice(view)?.text,
                  onDismissNotice: () => setState(
                    () => _dismissedNoticeId = _visibleNotice(view)?.id,
                  ),
                  footnote: _usage(view, l10n),
                );
              },
            ),
          ],
        ),
      ),
    );
  }

  ConversationNotice? _visibleNotice(ConversationView? view) {
    final notice = view?.notice;
    return notice == null || notice.id == _dismissedNoticeId ? null : notice;
  }

  List<Widget> _chips(ConversationView view, L10n l10n) {
    final model = view.model;
    final provider = model == null ? null : currentProvider(model);
    return [
      if (model != null && provider != null)
        ConfigChip(
          icon: LucideIcons.network,
          label: provider.name,
          semanticLabel: '${l10n.conversationProvider}: ${provider.name}',
          onTap: model.providers.length > 1
              ? () => pickProvider(context, model, _setConfig)
              : null,
        ),
      if (model != null && model.currentName.isNotEmpty)
        ConfigChip(
          icon: LucideIcons.cpu,
          label: model.currentName,
          semanticLabel: '${l10n.conversationModel}: ${model.currentName}',
          onTap: model.options.length > 1
              ? () => pickModel(context, model, _setConfig)
              : null,
        ),
      for (final config in view.configOptions)
        if (config.boolean case final on?)
          ConfigChip(
            icon: on ? LucideIcons.toggleRight : LucideIcons.toggleLeft,
            label:
                '${config.name} · ${on ? l10n.conversationOn : l10n.conversationOff}',
            semanticLabel:
                '${config.name}: ${on ? l10n.conversationOn : l10n.conversationOff}',
            // 开关项点一下就翻转，结果从视图流回来。
            onTap: () {
              HapticFeedback.selectionClick();
              _setConfig(config.configId, '', boolean: !on);
            },
          )
        else if (config.options.length > 1)
          ConfigChip(
            label: config.currentName,
            semanticLabel: '${config.name}: ${config.currentName}',
            onTap: () => pickConfig(context, config, _setConfig),
          ),
    ];
  }

  String? _usage(ConversationView? view, L10n l10n) {
    final usage = view?.usage;
    if (usage == null || usage.contextWindow <= 0) return null;
    final percent = (usage.usedTokens / usage.contextWindow * 100)
        .clamp(0, 100)
        .round();
    return l10n.conversationContextUsage(
      percent,
      compactCount(usage.usedTokens),
      compactCount(usage.contextWindow),
    );
  }
}

/// 阅读栏：最宽 [Space.readingMax]，居中。平板上页面在右侧详情栏里也一样。
class _Reading extends StatelessWidget {
  const _Reading({required this.child, this.padding = EdgeInsets.zero});

  final Widget child;
  final EdgeInsets padding;

  @override
  Widget build(BuildContext context) => Center(
    child: ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: Space.readingMax),
      child: Padding(
        padding: padding.add(
          const EdgeInsets.symmetric(horizontal: Space.page),
        ),
        child: child,
      ),
    ),
  );
}

class _TitleBlock extends StatelessWidget {
  const _TitleBlock({
    required this.title,
    required this.runtime,
    required this.runtimeId,
    required this.session,
  });

  final String title;
  final FleetRuntime? runtime;
  final String runtimeId;
  final SessionSummary? session;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final s = session;
    final line = [
      runtime?.name ?? runtimeId,
      if (s != null && s.agent.trim().isNotEmpty) agentDisplayName(s.agent),
      ?s?.placeLabel,
    ].join(' · ');
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(title, maxLines: 1, overflow: TextOverflow.ellipsis),
        const SizedBox(height: Space.xxs),
        StationLabel(
          name: line,
          color: runtime == null
              ? c.inkFaint
              : c.station(runtime!.record.accent),
          style: t.caption,
        ),
      ],
    );
  }
}

/// 「回到最新」：浮在列表底部，不在最新处时才出现。
class _JumpToLatest extends StatelessWidget {
  const _JumpToLatest({
    super.key,
    required this.onPressed,
    required this.visible,
  });

  final VoidCallback onPressed;
  final bool visible;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    return ExcludeSemantics(
      excluding: !visible,
      child: DecoratedBox(
        decoration: BoxDecoration(
          borderRadius: const BorderRadius.all(Radii.pill),
          boxShadow: [
            BoxShadow(
              color: c.shadow,
              offset: const Offset(0, 2),
              blurRadius: 10,
            ),
          ],
        ),
        child: FilledButton.icon(
          onPressed: onPressed,
          style: FilledButton.styleFrom(
            backgroundColor: c.raised,
            foregroundColor: c.ink,
            shape: const StadiumBorder(),
            side: BorderSide(color: c.hairline),
          ),
          icon: const Icon(LucideIcons.arrowDown, size: Strokes.iconSmall),
          label: Text(l10n.conversationJumpToLatest),
        ),
      ),
    );
  }
}
