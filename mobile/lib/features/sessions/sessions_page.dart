import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/semantics.dart';
import 'package:flutter_slidable/flutter_slidable.dart';
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
import '../../ui/station.dart';
import '../../ui/status.dart';
import '../../ui/time.dart';
import '../launch/new_session_sheet.dart';
import '../session/open_session.dart';
import 'history_page.dart';
import 'session_groups.dart';

/// 「会话」页：所有机器上的全部会话，按机器 → 项目分组，可按机器筛、可搜索。
///
/// 指挥台只放有事的会话；这里是「我昨天那条在哪」的去处，闲置的也在。
class SessionsPage extends StatefulWidget {
  const SessionsPage({super.key, required this.filter});

  /// 筛到哪台机器（运行时 id），null 是全部。指挥台点某台机器的「全部会话」会设它。
  final ValueNotifier<String?> filter;

  @override
  State<SessionsPage> createState() => _SessionsPageState();
}

class _SessionsPageState extends State<SessionsPage> {
  final _search = TextEditingController();
  final _scroll = ScrollController();
  final _machineKeys = <String, GlobalKey>{};
  String _query = '';
  String? _guide;

  /// 各台机器的工作区目录：补上还没有会话的项目、给智能体对话配名字。
  /// 机器上线时取一次，下拉刷新再取；只是锦上添花，取不到不影响列表。
  final _workspaces = <String, WorkspaceCatalog>{};
  final _loading = <String>{};
  Set<String> _online = const {};
  AppModel? _model;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_model != null) return;
    _model = AppScope.of(context)..fleet.addListener(_onFleet);
    _scroll.addListener(_updateGuide);
    _onFleet();
  }

  @override
  void dispose() {
    _model?.fleet.removeListener(_onFleet);
    _search.dispose();
    _scroll.dispose();
    super.dispose();
  }

  /// 机器刚连上时取它的工作区目录；忘掉的机器丢掉缓存。
  void _onFleet() {
    final runtimes = _model!.fleet.value.runtimes;
    final online = {
      for (final r in runtimes)
        if (r.online) r.id,
    };
    for (final id in online.difference(_online)) {
      unawaited(_loadWorkspace(id));
    }
    _online = online;
    final known = {for (final r in runtimes) r.id};
    _workspaces.removeWhere((id, _) => !known.contains(id));
  }

  Future<void> _loadWorkspace(String runtimeId) async {
    if (!_loading.add(runtimeId)) return;
    try {
      final catalog = await _model!.backend.workspaceCatalog(runtimeId);
      if (mounted) setState(() => _workspaces[runtimeId] = catalog);
    } catch (_) {
      // 取不到就少几个空项目、智能体对话退回引擎名，列表本身照常；下次上线或下拉再取。
    } finally {
      _loading.remove(runtimeId);
    }
  }

  Future<void> _refresh() async {
    final model = _model!;
    await Future.wait([
      model.backend.verifyConnections(),
      for (final id in _online) _loadWorkspace(id),
    ]);
  }

  /// 导词：标题收起后，写出当前滚到哪台机器。
  void _updateGuide() {
    final runtimes = _model!.fleet.value.runtimes;
    final barBottom =
        MediaQuery.paddingOf(context).top + kToolbarHeight + Space.sm;
    String? current;
    for (final runtime in runtimes) {
      final box =
          _machineKeys[runtime.id]?.currentContext?.findRenderObject()
              as RenderBox?;
      if (box == null || !box.attached) continue;
      if (box.localToGlobal(Offset.zero).dy <= barBottom) {
        current = runtime.name;
      }
    }
    if (current != _guide) setState(() => _guide = current);
  }

  String? _agentName(String runtimeId, String definitionId) {
    for (final a
        in _workspaces[runtimeId]?.agents ?? const <AcpAgentOption>[]) {
      if (a.agentDefinitionId == definitionId) return a.label;
    }
    return null;
  }

  Future<void> _delete(FleetRuntime runtime, SessionSummary session) async {
    final l10n = L10n.of(context);
    final model = _model!;
    final messenger = ScaffoldMessenger.of(context);
    final title = _rowTitle(l10n, session);
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(
          session.isTerminal
              ? l10n.sessionsDeleteTerminalTitle
              : l10n.sessionsDeleteConversationTitle,
        ),
        content: Text(
          session.isTerminal
              ? l10n.sessionsDeleteTerminalBody(title, runtime.name)
              : l10n.sessionsDeleteConversationBody(title, runtime.name),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.commonCancel),
          ),
          GalleyButton.danger(
            label: l10n.commonDelete,
            onPressed: () => Navigator.pop(context, true),
          ),
        ],
      ),
    );
    if (confirmed != true) return;
    try {
      // 删掉后运行时推新列表，行自己消失；自动化运行的会话运行时会拒绝，原话转给用户。
      await model.backend.deleteSession(runtime.id, session.id);
    } catch (error) {
      messenger.showSnackBar(
        SnackBar(
          content: Text(l10n.sessionsDeleteFailed(describeError(error))),
        ),
      );
    }
  }

  /// 长按：一个小弹层，打开 / 删除。左滑之外的第二条路，也给读屏和不习惯滑动的人。
  Future<void> _showActions(
    FleetRuntime runtime,
    SessionSummary session,
  ) async {
    final l10n = L10n.of(context);
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final action = await showModalBottomSheet<String>(
      context: context,
      useSafeArea: true,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.page + Space.xs,
                0,
                Space.page + Space.xs,
                Space.sm,
              ),
              child: Text(
                _rowTitle(l10n, session),
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: t.title.copyWith(color: c.ink),
              ),
            ),
            ShelfRow(
              leading: Icon(LucideIcons.messageSquareText, color: c.ink),
              title: Text(l10n.commonOpen),
              titleStyle: t.body,
              onTap: () => Navigator.pop(context, 'open'),
            ),
            ShelfRow(
              leading: Icon(LucideIcons.trash2, color: c.danger),
              title: Text(
                l10n.commonDelete,
                style: t.body.copyWith(color: c.danger),
              ),
              onTap: () => Navigator.pop(context, 'delete'),
            ),
            const SizedBox(height: Space.sm),
          ],
        ),
      ),
    );
    if (!mounted) return;
    switch (action) {
      case 'open':
        openSession(context, runtime.id, session);
      case 'delete':
        await _delete(runtime, session);
    }
  }

  void _openHistory(FleetRuntime runtime, SessionGroup group) {
    final root = group.projectRoot;
    if (root == null) return;
    final l10n = L10n.of(context);
    DetailHost.of(context).open(
      'history/${runtime.id}/$root',
      (_) => HistoryPage(
        runtimeId: runtime.id,
        projectRoot: root,
        projectTitle: group.title ?? l10n.sessionsNoProject,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final model = _model!;
    return ListenableBuilder(
      listenable: Listenable.merge([model.fleet, model.catalog, widget.filter]),
      builder: (context, _) {
        final runtimes = model.fleet.value.runtimes;
        // 筛到的机器被忘掉了：按全部显示，不留一个筛不出东西的芯片状态。
        final wanted = widget.filter.value;
        final filter = wanted != null && model.fleet.runtime(wanted) != null
            ? wanted
            : null;
        final machines = groupSessions(
          runtimes: runtimes,
          listOf: model.catalog.list,
          workspaces: _workspaces,
          machine: filter,
          query: _query,
          agentName: _agentName,
        );

        final List<Widget> body;
        if (runtimes.isEmpty) {
          body = [
            SliverFillRemaining(
              hasScrollBody: false,
              child: Center(
                child: EmptyNote(
                  icon: LucideIcons.monitorSmartphone,
                  title: l10n.sessionsNoMachinesTitle,
                  body: l10n.sessionsNoMachinesBody,
                ),
              ),
            ),
          ];
        } else if (machines.isEmpty) {
          body = [
            SliverToBoxAdapter(
              child: EmptyNote(
                icon: LucideIcons.searchX,
                title: l10n.sessionsNoMatchesTitle(_query.trim()),
                body: l10n.sessionsNoMatchesBody,
              ),
            ),
          ];
        } else {
          body = [
            for (final (index, machine) in machines.indexed)
              SliverToBoxAdapter(
                child: _MachineSection(
                  key: _machineKeys.putIfAbsent(
                    machine.runtime.id,
                    GlobalKey.new,
                  ),
                  machine: machine,
                  first: index == 0,
                  searching: _query.trim().isNotEmpty,
                  agentName: (id) => _agentName(machine.runtime.id, id),
                  onDelete: (s) => _delete(machine.runtime, s),
                  onActions: (s) => _showActions(machine.runtime, s),
                  onHistory: (g) => _openHistory(machine.runtime, g),
                ),
              ),
          ];
        }

        return SlidableAutoCloseBehavior(
          child: GalleyPage(
            title: l10n.sessionsTitle,
            controller: _scroll,
            guide: _guide,
            onRefresh: _refresh,
            actions: [
              IconButton(
                tooltip: l10n.sessionsNewSession,
                icon: const Icon(LucideIcons.plus),
                onPressed: runtimes.isEmpty
                    ? null
                    : () => showNewSessionSheet(context, runtimeId: filter),
              ),
            ],
            slivers: [
              if (runtimes.isNotEmpty)
                SliverToBoxAdapter(
                  child: _Toolbar(
                    controller: _search,
                    runtimes: runtimes,
                    selected: filter,
                    onQuery: (q) => setState(() => _query = q),
                    onSelect: (id) => widget.filter.value = id,
                  ),
                ),
              ...body,
            ],
          ),
        );
      },
    );
  }
}

String _rowTitle(L10n l10n, SessionSummary s) {
  final title = s.title.trim();
  if (title.isNotEmpty) return title;
  return s.isTerminal
      ? l10n.sessionsUntitledTerminal
      : l10n.sessionsUntitledConversation;
}

/// 搜索框 + 机器筛选芯片。只有一台机器时不出芯片：「全部」和它是同一回事。
class _Toolbar extends StatelessWidget {
  const _Toolbar({
    required this.controller,
    required this.runtimes,
    required this.selected,
    required this.onQuery,
    required this.onSelect,
  });

  final TextEditingController controller;
  final List<FleetRuntime> runtimes;
  final String? selected;
  final ValueChanged<String> onQuery;
  final ValueChanged<String?> onSelect;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);

    Widget chip({
      required String label,
      required bool on,
      required VoidCallback onTap,
      Color? dot,
    }) => Padding(
      padding: const EdgeInsetsDirectional.only(end: Space.sm),
      child: ChoiceChip(
        label: Text(label),
        selected: on,
        onSelected: (_) => onTap(),
        // 选中底是实心墨，字要跟着反白（主题的 labelStyle 只管未选中）。
        labelStyle: t.label.copyWith(color: on ? c.onSolid : c.ink),
        avatar: dot == null
            ? null
            : Container(
                width: Strokes.statusDot,
                height: Strokes.statusDot,
                decoration: BoxDecoration(
                  color: dot,
                  borderRadius: const BorderRadius.all(Radii.tag),
                ),
              ),
      ),
    );

    return Padding(
      padding: const EdgeInsets.only(bottom: Space.xs),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: Space.page),
            child: ValueListenableBuilder(
              valueListenable: controller,
              builder: (context, value, _) => TextField(
                controller: controller,
                onChanged: onQuery,
                textInputAction: TextInputAction.search,
                autocorrect: false,
                style: t.body.copyWith(color: c.ink),
                decoration: InputDecoration(
                  hintText: l10n.sessionsSearchHint,
                  prefixIcon: Icon(
                    LucideIcons.search,
                    size: Strokes.icon,
                    color: c.inkMuted,
                  ),
                  suffixIcon: value.text.isEmpty
                      ? null
                      : IconButton(
                          tooltip: l10n.sessionsSearchClear,
                          icon: Icon(
                            LucideIcons.x,
                            size: Strokes.icon,
                            color: c.inkMuted,
                          ),
                          onPressed: () {
                            controller.clear();
                            onQuery('');
                          },
                        ),
                ),
              ),
            ),
          ),
          if (runtimes.length > 1)
            Semantics(
              container: true,
              label: l10n.sessionsFilterMachines,
              child: SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                padding: const EdgeInsets.fromLTRB(
                  Space.page,
                  Space.sm,
                  Space.page,
                  0,
                ),
                child: Row(
                  children: [
                    chip(
                      label: l10n.sessionsFilterAll,
                      on: selected == null,
                      onTap: () => onSelect(null),
                    ),
                    for (final r in runtimes)
                      chip(
                        label: r.name,
                        on: selected == r.id,
                        dot: c.station(r.record.accent),
                        onTap: () => onSelect(r.id),
                      ),
                  ],
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// 一台机器：机器头（色带 + 名字 + 链路 / 离线缓存说明），下面按组排搁板。
class _MachineSection extends StatelessWidget {
  const _MachineSection({
    super.key,
    required this.machine,
    required this.first,
    required this.searching,
    required this.agentName,
    required this.onDelete,
    required this.onActions,
    required this.onHistory,
  });

  final MachineSessions machine;
  final bool first;
  final bool searching;
  final String? Function(String definitionId) agentName;
  final void Function(SessionSummary) onDelete;
  final void Function(SessionSummary) onActions;
  final void Function(SessionGroup) onHistory;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final runtime = machine.runtime;
    final list = machine.list;

    // 诚实的状态：列表不是实时的就写清楚是几点的数据，旧数据不冒充新数据。
    final savedAt = list?.cached == true ? list?.savedAtMs : null;
    final status = savedAt != null
        ? l10n.machineStatusCached(
            cachedStatusLabel(l10n, runtime.status),
            formatClock(DateTime.fromMillisecondsSinceEpoch(savedAt)),
          )
        : linkLabel(l10n, runtime.status);

    final children = <Widget>[
      Padding(
        padding: EdgeInsets.fromLTRB(
          Space.page + Space.xs,
          first ? Space.sm : Space.section + Space.sm,
          Space.page + Space.xs,
          0,
        ),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.center,
          children: [
            StationTape(color: c.station(runtime.record.accent), height: 20),
            const SizedBox(width: Space.sm),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Semantics(
                    header: true,
                    child: Text(
                      runtime.name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: t.title.copyWith(color: c.ink),
                    ),
                  ),
                  Text(
                    status,
                    style: t.meta.copyWith(
                      color: runtime.needsAttention ? c.danger : c.inkMuted,
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    ];

    // 还没有开着会话的项目（来自工作区目录）收进一组，每个一行直达历史，不给每个空项目
    // 单独一块搁板——那样一台机器开过的项目一多，真有会话的组就被淹没了。
    final active = [
      for (final g in machine.groups)
        if (g.sessions.isNotEmpty) g,
    ];
    final idleProjects = [
      for (final g in machine.groups)
        if (g.sessions.isEmpty && g.projectRoot != null) g,
    ];

    if (active.isEmpty) {
      final waiting = list == null || (list.cached && list.savedAtMs == null);
      children.add(
        Padding(
          padding: const EdgeInsets.only(top: Space.md),
          child: Shelf(
            children: [
              ShelfRow(
                title: Text(
                  waiting
                      ? (runtime.online
                            ? l10n.sessionsMachineWaiting
                            : linkLabel(l10n, runtime.status))
                      : l10n.sessionsMachineEmpty,
                ),
                titleStyle: t.callout.copyWith(color: c.inkMuted),
              ),
            ],
          ),
        ),
      );
    }

    for (final (index, group) in active.indexed) {
      final (icon, title) = switch (group.kind) {
        SessionGroupKind.project => (
          LucideIcons.folder,
          group.title ?? l10n.sessionsNoProject,
        ),
        SessionGroupKind.agentChats => (
          LucideIcons.bot,
          l10n.sessionsAgentChats,
        ),
        SessionGroupKind.automationRuns => (
          LucideIcons.workflow,
          l10n.sessionsAutomationRuns,
        ),
      };
      final showHistory =
          !searching &&
          group.kind == SessionGroupKind.project &&
          group.projectRoot != null;
      children
        ..add(
          ShelfHeader(
            first: index == 0,
            leading: Icon(icon, size: Strokes.iconSmall, color: c.inkMuted),
            title: title,
          ),
        )
        ..add(
          Shelf(
            children: [
              for (final s in group.sessions)
                _SessionRow(
                  key: ValueKey(sessionKey(runtime.id, s.id)),
                  runtimeId: runtime.id,
                  session: s,
                  stale: list?.cached ?? false,
                  group: group.kind,
                  agentName: s.agentDefinitionId == null
                      ? null
                      : agentName(s.agentDefinitionId!),
                  onDelete: () => onDelete(s),
                  onActions: () => onActions(s),
                ),
              if (showHistory)
                ShelfRow(
                  leading: Icon(
                    LucideIcons.history,
                    size: Strokes.icon,
                    color: c.inkMuted,
                  ),
                  title: Text(l10n.sessionsHistoryRow),
                  titleStyle: t.callout,
                  trailing: Icon(
                    LucideIcons.chevronRight,
                    size: Strokes.iconSmall,
                    color: c.inkFaint,
                  ),
                  onTap: () => onHistory(group),
                ),
            ],
          ),
        );
    }

    if (idleProjects.isNotEmpty) {
      children
        ..add(
          ShelfHeader(
            leading: Icon(
              LucideIcons.folderClosed,
              size: Strokes.iconSmall,
              color: c.inkMuted,
            ),
            title: l10n.sessionsIdleProjects,
          ),
        )
        ..add(
          Shelf(
            children: [
              for (final group in idleProjects)
                ShelfRow(
                  leading: Icon(
                    LucideIcons.history,
                    size: Strokes.icon,
                    color: c.inkMuted,
                  ),
                  title: Text(group.title ?? group.projectRoot!),
                  titleStyle: t.callout,
                  subtitle: Text(l10n.sessionsHistoryRow),
                  trailing: Icon(
                    LucideIcons.chevronRight,
                    size: Strokes.iconSmall,
                    color: c.inkFaint,
                  ),
                  onTap: () => onHistory(group),
                ),
            ],
          ),
        );
    }

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: children,
    );
  }
}

class _SessionRow extends StatelessWidget {
  const _SessionRow({
    super.key,
    required this.runtimeId,
    required this.session,
    required this.stale,
    required this.group,
    required this.agentName,
    required this.onDelete,
    required this.onActions,
  });

  final String runtimeId;
  final SessionSummary session;

  /// 这台机器的列表来自离线缓存：在做的不呼吸、不走表。
  final bool stale;
  final SessionGroupKind group;

  /// 智能体对话的智能体名（工作区目录里配到的）；配不到为空。
  final String? agentName;
  final VoidCallback onDelete;
  final VoidCallback onActions;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final host = DetailHost.of(context);
    final key = sessionKey(runtimeId, session.id);
    final selected = host.selectedKey == key;
    final mark = session.mark;
    final live = mark == SessionMark.running && !stale;
    final updated = fromUnixSeconds(session.updatedAt);

    // 副标题：谁 · 在哪 / 在做什么。项目组里项目名就是组头，不再写一遍；有事的会话写
    // 它此刻在做什么（工具名、审批问句），闲着的不写「空闲」（状态记号已经说了）。
    final detail = session.detail?.trim();
    final activity = switch (mark) {
      SessionMark.running || SessionMark.needsYou || SessionMark.failed =>
        detail != null && detail.isNotEmpty
            ? detail
            : phaseLabel(l10n, session.phase),
      _ => null,
    };
    final who = group == SessionGroupKind.agentChats
        ? (agentName ?? sessionAgentLabel(l10n, session))
        : sessionAgentLabel(l10n, session);
    final subtitle = [
      who,
      if (group == SessionGroupKind.automationRuns)
        ?session.automation?.automationName,
      ?activity,
    ].join(' · ');

    final row = ColoredBox(
      color: selected ? c.control : Colors.transparent,
      child: ShelfRow(
        leading: StatusMark(
          mark: mark,
          label: phaseLabel(l10n, session.phase),
          stale: stale,
        ),
        title: Text(_rowTitle(l10n, session)),
        subtitle: Row(
          children: [
            AgentGlyph(
              agent: session.agent,
              terminal: session.isTerminal,
              color: c.inkMuted,
              size: Strokes.glyphSmall,
            ),
            const SizedBox(width: Space.xs),
            Flexible(
              child: Text(
                subtitle,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
            ),
          ],
        ),
        trailing: live && updated != null
            ? ElapsedLabel(
                since: updated,
                style: t.monoTime.copyWith(color: c.ok),
              )
            : ListenableBuilder(
                listenable: Clock.instance,
                builder: (context, _) => Text(
                  formatAgo(l10n, updated, DateTime.now()) ?? '',
                  style: t.meta,
                ),
              ),
        onTap: () => openSession(context, runtimeId, session),
        onLongPress: onActions,
      ),
    );

    return Semantics(
      // 左滑是藏起来的手势，读屏滑不出按钮：删除另给一条读屏动作。
      customSemanticsActions: {
        CustomSemanticsAction(label: l10n.commonDelete): onDelete,
      },
      child: Slidable(
        key: ValueKey(key),
        groupTag: 'sessions',
        endActionPane: ActionPane(
          motion: const DrawerMotion(),
          extentRatio: 0.28,
          // 不给一划到底就删：删除会结束进程，滑出按钮 → 点 → 确认，三步都留着。
          children: [
            SlidableAction(
              onPressed: (_) => onDelete(),
              backgroundColor: c.danger,
              foregroundColor: c.onSolid,
              icon: LucideIcons.trash2,
              label: l10n.commonDelete,
            ),
          ],
        ),
        child: row,
      ),
    );
  }
}
