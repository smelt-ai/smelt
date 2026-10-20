import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/adaptive.dart';
import '../../ui/agent_glyph.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../../ui/shelf.dart';
import '../../ui/time.dart';
import '../launch/launch_flow.dart';

/// 一个项目的历史会话：选 agent，列出它在这个项目里能续接的对话；可续接、可改名。
class HistoryPage extends StatefulWidget {
  const HistoryPage({
    super.key,
    required this.runtimeId,
    required this.projectRoot,
    required this.projectTitle,
  });

  final String runtimeId;
  final String projectRoot;
  final String projectTitle;

  @override
  State<HistoryPage> createState() => _HistoryPageState();
}

class _HistoryPageState extends State<HistoryPage> {
  late final AppModel _model = AppScope.of(context);

  List<AcpAgentOption>? _agents;
  String? _agentsError;
  AcpAgentOption? _agent;

  List<HistorySessionSummary>? _sessions;
  String? _error;

  /// 切 agent 时旧请求的结果要丢掉。
  int _generation = 0;

  /// 正在续接的那一条（行尾转圈，防止连点开出两个）。
  String? _resuming;

  @override
  void initState() {
    super.initState();
    unawaited(_loadAgents());
  }

  Future<void> _loadAgents() async {
    try {
      final catalog = await _model.backend.workspaceCatalog(widget.runtimeId);
      if (!mounted) return;
      final agents = catalog.agents;
      // 默认选第一个裸引擎：项目里的历史多半是它们的；产品智能体通常开在自己的 space。
      final first =
          agents.where((a) => a.agentDefinitionId == null).firstOrNull ??
          agents.firstOrNull;
      setState(() {
        _agents = agents;
        _agent = first;
      });
      if (first != null) await _loadHistory();
    } catch (error) {
      if (mounted) setState(() => _agentsError = describeError(error));
    }
  }

  Future<void> _loadHistory() async {
    final agent = _agent;
    if (agent == null) return;
    final generation = ++_generation;
    setState(() {
      _sessions = null;
      _error = null;
    });
    try {
      final sessions = await _model.backend.listHistory(
        widget.runtimeId,
        widget.projectRoot,
        agent.id,
      );
      if (!mounted || generation != _generation) return;
      setState(() => _sessions = sessions);
    } catch (error) {
      if (!mounted || generation != _generation) return;
      setState(() => _error = describeError(error));
    }
  }

  void _selectAgent(AcpAgentOption agent) {
    if (agent.id == _agent?.id) return;
    setState(() => _agent = agent);
    unawaited(_loadHistory());
  }

  Future<void> _resume(
    HistorySessionSummary item, [
    LaunchIntent? retry,
  ]) async {
    final agent = _agent;
    if (agent == null || _resuming != null) return;
    final l10n = L10n.of(context);
    final host = DetailHost.of(context);
    final messenger = ScaffoldMessenger.of(context);
    final intent =
        retry ??
        LaunchIntent(
          runtimeId: widget.runtimeId,
          projectRoot: widget.projectRoot,
          agentOptionId: agent.id,
          resumeId: item.resumeId,
          label: item.displayTitle,
          requestId: _model.backend.newRequestId(),
        );
    setState(() => _resuming = item.resumeId);
    final String sessionId;
    try {
      sessionId = await intent.send(_model.backend);
    } catch (error) {
      if (mounted) setState(() => _resuming = null);
      messenger.showSnackBar(
        SnackBar(
          content: Text(l10n.sessionsHistoryResumeFailed(describeError(error))),
          // 重试沿用同一个 requestId：上次其实成功了的话落到同一个会话。
          action: SnackBarAction(
            label: l10n.commonRetry,
            onPressed: () {
              if (mounted) _resume(item, intent);
            },
          ),
        ),
      );
      return;
    }
    final opened = await openWhenListed(
      _model,
      host,
      widget.runtimeId,
      sessionId,
    );
    if (mounted) setState(() => _resuming = null);
    if (!opened) {
      final machine = _model.fleet.runtime(widget.runtimeId)?.name ?? '';
      messenger.showSnackBar(
        SnackBar(content: Text(l10n.launchNotListed(machine))),
      );
    }
  }

  Future<void> _rename(HistorySessionSummary item) async {
    final title = await showDialog<String>(
      context: context,
      builder: (_) => _RenameDialog(initial: item.displayTitle),
    );
    if (title == null || !mounted) return;
    // 清空 = 恢复 agent 原始标题，与桌面「恢复默认名称」同一条路。
    await _applyRename(item, title.trim().isEmpty ? null : title.trim());
  }

  /// 改名结果由运行时算好回传（恢复默认时要拿回原始标题），就地替换这一行。
  Future<void> _applyRename(HistorySessionSummary item, String? title) async {
    final agent = _agent;
    if (agent == null) return;
    final l10n = L10n.of(context);
    final messenger = ScaffoldMessenger.of(context);
    try {
      final result = await _model.backend.renameHistory(
        widget.runtimeId,
        projectRoot: widget.projectRoot,
        agentOptionId: agent.id,
        resumeId: item.resumeId,
        title: title,
      );
      if (!mounted || agent.id != _agent?.id) return;
      setState(() {
        _sessions = [
          for (final s in _sessions ?? const <HistorySessionSummary>[])
            s.resumeId == item.resumeId
                ? HistorySessionSummary(
                    resumeId: s.resumeId,
                    // 有自定义名时 result.title 就是它，原始标题不变；恢复默认时
                    // result.title 才是原始标题。
                    title: result.customTitle == null ? result.title : s.title,
                    customTitle: result.customTitle,
                    lastActiveAtMs: s.lastActiveAtMs,
                    messageCount: s.messageCount,
                  )
                : s,
        ];
      });
    } catch (error) {
      messenger.showSnackBar(
        SnackBar(
          content: Text(l10n.sessionsHistoryRenameFailed(describeError(error))),
        ),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    return Scaffold(
      appBar: AppBar(
        title: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(l10n.sessionsHistoryTitle),
            Text(
              widget.projectTitle,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: t.caption.copyWith(color: c.inkMuted),
            ),
          ],
        ),
      ),
      body: RefreshIndicator.adaptive(
        onRefresh: _agent == null ? _loadAgents : _loadHistory,
        child: ListView(
          physics: const AlwaysScrollableScrollPhysics(),
          padding: EdgeInsets.only(
            bottom: MediaQuery.paddingOf(context).bottom + Space.xxl,
          ),
          children: _body(context),
        ),
      ),
    );
  }

  List<Widget> _body(BuildContext context) {
    final l10n = L10n.of(context);
    final agents = _agents;
    if (_agentsError != null) {
      return [
        EmptyNote(
          icon: LucideIcons.cloudOff,
          title: l10n.sessionsHistoryLoadFailed,
          body: _agentsError,
          action: GalleyButton.solid(
            label: l10n.commonRetry,
            onPressed: () {
              setState(() => _agentsError = null);
              _loadAgents();
            },
          ),
        ),
      ];
    }
    if (agents == null) return const [_Spinner()];
    if (agents.isEmpty) {
      return [
        EmptyNote(icon: LucideIcons.bot, title: l10n.sessionsHistoryNoAgents),
      ];
    }
    return [
      _AgentPicker(agents: agents, selected: _agent, onSelect: _selectAgent),
      ..._list(context),
    ];
  }

  List<Widget> _list(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final sessions = _sessions;
    if (_error != null) {
      return [
        EmptyNote(
          icon: LucideIcons.cloudOff,
          title: l10n.sessionsHistoryLoadFailed,
          body: _error,
          action: GalleyButton.solid(
            label: l10n.commonRetry,
            onPressed: _loadHistory,
          ),
        ),
      ];
    }
    if (sessions == null) return const [_Spinner()];
    if (sessions.isEmpty) {
      return [
        EmptyNote(
          icon: LucideIcons.history,
          title: l10n.sessionsHistoryEmptyTitle,
          body: l10n.sessionsHistoryEmptyBody(
            _agent?.label ?? '',
            widget.projectTitle,
          ),
        ),
      ];
    }
    final now = DateTime.now();
    return [
      const SizedBox(height: Space.md),
      Shelf(
        children: [
          for (final item in sessions)
            ShelfRow(
              key: ValueKey(item.resumeId),
              title: Text(item.displayTitle),
              subtitle: Text(
                [
                  // 改过名的，补一句原名：认得出这条本来是什么。
                  if (item.customTitle?.trim().isNotEmpty == true &&
                      item.title.trim().isNotEmpty)
                    l10n.sessionsHistoryOriginal(item.title),
                  ?formatAgo(
                    l10n,
                    item.lastActiveAtMs == null
                        ? null
                        : DateTime.fromMillisecondsSinceEpoch(
                            item.lastActiveAtMs!,
                          ),
                    now,
                  ),
                  l10n.sessionsHistoryMessages(item.messageCount),
                ].join(' · '),
              ),
              trailing: _resuming == item.resumeId
                  ? Semantics(
                      label: l10n.sessionsHistoryResuming,
                      child: const SizedBox.square(
                        dimension: Strokes.icon,
                        child: CircularProgressIndicator.adaptive(
                          strokeWidth: 2,
                        ),
                      ),
                    )
                  : _RowMenu(
                      hasCustomTitle:
                          item.customTitle?.trim().isNotEmpty == true,
                      onRename: () => _rename(item),
                      onReset: () => _applyRename(item, null),
                    ),
              titleStyle: t.headline.copyWith(color: c.ink),
              onTap: _resuming == null ? () => _resume(item) : null,
              onLongPress: () => _rename(item),
            ),
        ],
      ),
    ];
  }
}

/// agent 选择：一排芯片，带 agent 图标。
class _AgentPicker extends StatelessWidget {
  const _AgentPicker({
    required this.agents,
    required this.selected,
    required this.onSelect,
  });

  final List<AcpAgentOption> agents;
  final AcpAgentOption? selected;
  final ValueChanged<AcpAgentOption> onSelect;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      padding: const EdgeInsets.fromLTRB(Space.page, Space.sm, Space.page, 0),
      child: Row(
        children: [
          for (final agent in agents)
            Padding(
              padding: const EdgeInsetsDirectional.only(end: Space.sm),
              child: Builder(
                builder: (context) {
                  final on = agent.id == selected?.id;
                  final ink = on ? c.onSolid : c.ink;
                  return ChoiceChip(
                    selected: on,
                    onSelected: (_) => onSelect(agent),
                    // 选中底是实心墨，字和图标跟着反白（主题的 labelStyle 只管未选中）。
                    labelStyle: t.label.copyWith(color: ink),
                    avatar: AgentGlyph(
                      agent: agent.kind,
                      color: ink,
                      size: Strokes.iconSmall,
                    ),
                    label: Text(agent.label),
                  );
                },
              ),
            ),
        ],
      ),
    );
  }
}

class _RowMenu extends StatelessWidget {
  const _RowMenu({
    required this.hasCustomTitle,
    required this.onRename,
    required this.onReset,
  });

  final bool hasCustomTitle;
  final VoidCallback onRename;
  final VoidCallback onReset;

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final c = GalleyColors.of(context);
    return PopupMenuButton<VoidCallback>(
      tooltip: l10n.commonMore,
      icon: Icon(LucideIcons.ellipsis, size: Strokes.icon, color: c.inkMuted),
      onSelected: (action) => action(),
      itemBuilder: (context) => [
        PopupMenuItem(value: onRename, child: Text(l10n.commonRename)),
        if (hasCustomTitle)
          PopupMenuItem(
            value: onReset,
            child: Text(l10n.sessionsHistoryResetName),
          ),
      ],
    );
  }
}

/// 改名对话框：自己持有输入框控制器，关闭动画结束后才释放。
class _RenameDialog extends StatefulWidget {
  const _RenameDialog({required this.initial});

  final String initial;

  @override
  State<_RenameDialog> createState() => _RenameDialogState();
}

class _RenameDialogState extends State<_RenameDialog> {
  late final _controller = TextEditingController(text: widget.initial);

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    return AlertDialog(
      title: Text(l10n.sessionsHistoryRenameTitle),
      content: TextField(
        controller: _controller,
        autofocus: true,
        textInputAction: TextInputAction.done,
        decoration: InputDecoration(
          labelText: l10n.commonName,
          helperText: l10n.sessionsHistoryRenameHelp,
          helperMaxLines: 2,
        ),
        onSubmitted: (value) => Navigator.pop(context, value),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: Text(l10n.commonCancel),
        ),
        GalleyButton.solid(
          label: l10n.commonSave,
          onPressed: () => Navigator.pop(context, _controller.text),
        ),
      ],
    );
  }
}

class _Spinner extends StatelessWidget {
  const _Spinner();

  @override
  Widget build(BuildContext context) => const Padding(
    padding: EdgeInsets.all(Space.xxl),
    child: Center(child: CircularProgressIndicator.adaptive()),
  );
}
