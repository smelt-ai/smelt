import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/adaptive.dart';
import '../../ui/agent_glyph.dart';
import '../../ui/page.dart';
import '../../ui/shelf.dart';
import '../../ui/station.dart';
import '../../ui/status.dart';
import '../../ui/time.dart';
import 'agent_detail_page.dart';
import 'agents_board.dart';
import 'agents_widgets.dart';
import 'automation_detail_page.dart';
import 'automation_text.dart';

enum _Segment { agents, automations }

/// 「智能体」：每台机器上的智能体定义与自动化，按机器分组。
///
/// 回答两个问题：它**为什么这么干**（智能体的长期指令、插件、上下文），以及它**什么时候
/// 会自己动**（自动化的触发条件、上次结果）。手机是展示层，只开放开关、立即运行一次、
/// 和智能体开始对话这几个动作；创建与编辑留在桌面。
class AgentsPage extends StatefulWidget {
  const AgentsPage({super.key});

  @override
  State<AgentsPage> createState() => _AgentsPageState();
}

class _AgentsPageState extends State<AgentsPage> {
  late final AgentsBoard _board;
  final _scroll = ScrollController();
  final _sectionKeys = <String, GlobalKey>{};
  _Segment _segment = _Segment.agents;
  String? _guide;

  @override
  void initState() {
    super.initState();
    // 板子一建好就按当前在线的机器取目录，之后机器上线再各自补取；build 里不发请求。
    _board = AgentsBoard(AppScope.of(context));
    _scroll.addListener(_updateGuide);
  }

  @override
  void dispose() {
    _scroll.dispose();
    _board.dispose();
    super.dispose();
  }

  /// 导词：标题收起后，写出当前滚到哪台机器。
  void _updateGuide() {
    final fleet = AppScope.of(context).fleet.value;
    final barBottom =
        MediaQuery.paddingOf(context).top + kToolbarHeight + Space.sm;
    String? current;
    for (final runtime in fleet.runtimes) {
      final box =
          _sectionKeys[runtime.id]?.currentContext?.findRenderObject()
              as RenderBox?;
      if (box == null || !box.attached) continue;
      if (box.localToGlobal(Offset.zero).dy <= barBottom) {
        current = runtime.name;
      }
    }
    if (current != _guide) setState(() => _guide = current);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final model = AppScope.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([model.fleet, _board]),
      builder: (context, _) {
        final runtimes = model.fleet.value.runtimes;
        if (runtimes.isEmpty) {
          return GalleyPage(
            title: l10n.agentsTitle,
            slivers: [
              SliverFillRemaining(
                hasScrollBody: false,
                child: Center(
                  child: EmptyNote(
                    icon: LucideIcons.bot,
                    title: l10n.agentsNoMachinesTitle,
                    body: l10n.agentsNoMachinesBody,
                  ),
                ),
              ),
            ],
          );
        }
        return GalleyPage(
          title: l10n.agentsTitle,
          controller: _scroll,
          guide: _guide,
          onRefresh: _board.refresh,
          slivers: [
            SliverToBoxAdapter(
              child: Padding(
                padding: const EdgeInsets.symmetric(vertical: Space.xs),
                child: SegmentedButton<_Segment>(
                  // 撑满页宽：两段各占一半，拇指够得着，也不像一枚漂在左边的小标签。
                  expandedInsets: const EdgeInsets.symmetric(
                    horizontal: Space.page,
                  ),
                  showSelectedIcon: false,
                  segments: [
                    ButtonSegment(
                      value: _Segment.agents,
                      label: Text(l10n.agentsSegmentAgents),
                    ),
                    ButtonSegment(
                      value: _Segment.automations,
                      label: Text(l10n.agentsSegmentAutomations),
                    ),
                  ],
                  selected: {_segment},
                  onSelectionChanged: (selection) =>
                      setState(() => _segment = selection.first),
                ),
              ),
            ),
            for (final (index, runtime) in runtimes.indexed)
              SliverToBoxAdapter(
                child: _MachineSection(
                  key: _sectionKeys.putIfAbsent(runtime.id, GlobalKey.new),
                  board: _board,
                  runtime: runtime,
                  segment: _segment,
                  first: index == 0,
                ),
              ),
          ],
        );
      },
    );
  }
}

/// 一台机器的一组：灶台标签色 + 机器名，下面一块搁板。
class _MachineSection extends StatelessWidget {
  const _MachineSection({
    super.key,
    required this.board,
    required this.runtime,
    required this.segment,
    required this.first,
  });

  final AgentsBoard board;
  final FleetRuntime runtime;
  final _Segment segment;
  final bool first;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final machine = board.machine(runtime.id);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ShelfHeader(
          first: first,
          leading: StationTape(
            color: c.station(runtime.record.accent),
            height: Strokes.headerTape,
          ),
          title: runtime.name,
          // 在线不写链路：这一页关心的是机器上有什么，不是连得多快。不在线才说。
          trailing: runtime.online
              ? null
              : Text(
                  linkLabel(l10n, runtime.status),
                  style: t.meta.copyWith(
                    color: runtime.needsAttention ? c.danger : c.inkMuted,
                  ),
                ),
        ),
        Shelf(children: _rows(context, l10n, machine)),
      ],
    );
  }

  List<Widget> _rows(BuildContext context, L10n l10n, MachineAgents machine) {
    // 不在线时不显示上次取到的目录：开关、立即运行都用不了，旧数据也不能冒充现状。
    if (!runtime.online) {
      return [NoteRow(icon: LucideIcons.wifiOff, text: l10n.agentsOfflineNote)];
    }
    final catalog = machine.catalog;
    final error = machine.error;
    final errorRow = error == null
        ? null
        : NoteRow(
            icon: LucideIcons.circleAlert,
            danger: true,
            text: l10n.agentsLoadFailed(error),
            action: TextButton(
              onPressed: () => board.reload(runtime.id),
              child: Text(l10n.commonRetry),
            ),
          );
    if (catalog == null) {
      return [errorRow ?? NoteRow(busy: true, text: l10n.commonLoading)];
    }
    final rows = switch (segment) {
      _Segment.agents => [
        for (final agent in catalog.agents)
          _AgentRow(board: board, runtimeId: runtime.id, agent: agent),
      ],
      _Segment.automations => [
        for (final automation in catalog.automations)
          _AutomationRow(
            board: board,
            runtimeId: runtime.id,
            automation: automation,
          ),
      ],
    };
    return [
      ?errorRow,
      if (rows.isEmpty)
        NoteRow(
          text: segment == _Segment.agents
              ? l10n.agentsEmptyAgents
              : l10n.agentsEmptyAutomations,
        ),
      ...rows,
    ];
  }
}

/// 平板上详情栏里正开着的那一行，底色压一档。
class _Selectable extends StatelessWidget {
  const _Selectable({required this.detailKey, required this.child});

  final String detailKey;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final selected = DetailHost.of(context).selectedKey == detailKey;
    return ColoredBox(
      color: selected ? GalleyColors.of(context).control : Colors.transparent,
      child: child,
    );
  }
}

class _AgentRow extends StatelessWidget {
  const _AgentRow({
    required this.board,
    required this.runtimeId,
    required this.agent,
  });

  final AgentsBoard board;
  final String runtimeId;
  final AgentDefinitionSummary agent;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final key = 'agent:${sessionKey(runtimeId, agent.id)}';
    final description = agent.description.trim();
    return _Selectable(
      detailKey: key,
      child: ShelfRow(
        leading: AgentGlyph(
          agent: agent.engineKindId,
          color: c.ink,
          size: Strokes.icon,
        ),
        title: Text(agent.name),
        subtitle: Text(
          description.isEmpty
              ? agentDisplayName(agent.engineKindId)
              : description,
        ),
        trailing: Icon(
          LucideIcons.chevronRight,
          size: Strokes.iconSmall,
          color: c.inkFaint,
        ),
        onTap: () => DetailHost.of(context).open(
          key,
          (_) => AgentDetailPage(
            board: board,
            runtimeId: runtimeId,
            agentId: agent.id,
          ),
        ),
      ),
    );
  }
}

/// 自动化的一行：名字、什么时候跑、上次结果、开关。三件事都在首屏，不用点进去就能判断
/// 「今天这条要不要停掉」。
class _AutomationRow extends StatelessWidget {
  const _AutomationRow({
    required this.board,
    required this.runtimeId,
    required this.automation,
  });

  final AgentsBoard board;
  final String runtimeId;
  final AutomationSummary automation;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final key = 'automation:${sessionKey(runtimeId, automation.id)}';
    final enabled = board.enabled(runtimeId, automation);
    final run = automation.lastRun;
    return _Selectable(
      detailKey: key,
      child: ShelfRow(
        leading: StatusMark(
          mark: run == null ? SessionMark.idle : runMark(run.status),
          label: run == null
              ? l10n.agentsNeverRun
              : runStatusLabel(l10n, run.status),
        ),
        // 暂停的整行名字压暗，一眼分得出「它现在不会自己动」。
        title: Text(
          automation.name,
          style: enabled ? null : TextStyle(color: c.inkMuted),
        ),
        subtitle: ListenableBuilder(
          listenable: Clock.instance,
          builder: (context, _) {
            final now = DateTime.now();
            return Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(
                  automationWhenLine(
                    l10n,
                    MaterialLocalizations.of(context),
                    automation,
                    enabled,
                    now,
                  ),
                ),
                Text(lastRunLine(l10n, run, now)),
              ],
            );
          },
        ),
        trailing: Semantics(
          label: automation.name,
          child: Switch(
            value: enabled,
            onChanged: (value) =>
                toggleAutomation(context, board, runtimeId, automation, value),
          ),
        ),
        onTap: () => DetailHost.of(context).open(
          key,
          (_) => AutomationDetailPage(
            board: board,
            runtimeId: runtimeId,
            automationId: automation.id,
          ),
        ),
      ),
    );
  }
}
