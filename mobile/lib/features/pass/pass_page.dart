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
import '../../ui/station.dart';
import '../../ui/status.dart';
import '../../ui/ticket_rail.dart';
import '../../ui/time.dart';
import '../actions/ticket_bodies.dart';
import '../launch/new_session_sheet.dart';
import '../machines/pairing.dart';
import '../session/open_session.dart';
import 'pass_ticket.dart';
import 'triage_page.dart';

/// 指挥台：单据轨 + 各灶台在做的事。
class PassPage extends StatefulWidget {
  const PassPage({super.key, this.onShowAllSessions});

  /// 切到「会话」页（可按机器筛）。
  final void Function(String? runtimeId)? onShowAllSessions;

  @override
  State<PassPage> createState() => _PassPageState();
}

class _PassPageState extends State<PassPage> {
  final _scroll = ScrollController();
  final _stationKeys = <String, GlobalKey>{};
  String? _guide;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_updateGuide);
  }

  @override
  void dispose() {
    _scroll.dispose();
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
          _stationKeys[runtime.id]?.currentContext?.findRenderObject()
              as RenderBox?;
      if (box == null || !box.attached) continue;
      if (box.localToGlobal(Offset.zero).dy <= barBottom) {
        current = runtime.name;
      }
    }
    if (current != _guide) setState(() => _guide = current);
  }

  void _openTriage(BuildContext context, int index) {
    final host = DetailHost.of(context);
    // 平板：逐张处理就在右侧详情栏里，换到这一张。
    if (isSplitLayout(context)) {
      host.open(
        'triage/$index',
        (_) => TriagePage(
          key: ValueKey('triage/$index'),
          initialIndex: index,
          embedded: true,
          onOpenSession: (runtimeId, session) =>
              openSessionWith(host, runtimeId, session),
        ),
      );
      return;
    }
    Navigator.of(context, rootNavigator: true).push(
      MaterialPageRoute<void>(
        fullscreenDialog: true,
        builder: (_) => TriagePage(
          initialIndex: index,
          onOpenSession: (runtimeId, session) =>
              openSessionWith(host, runtimeId, session),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final model = AppScope.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([model.fleet, model.pass]),
      builder: (context, _) {
        final fleet = model.fleet.value;
        final pass = model.pass.value;
        if (fleet.runtimes.isEmpty) {
          return GalleyPage(
            title: l10n.passTitle,
            slivers: [
              SliverFillRemaining(
                hasScrollBody: false,
                child: Center(
                  child: EmptyNote(
                    icon: LucideIcons.qrCode,
                    title: l10n.passNoMachinesTitle,
                    body: l10n.passNoMachinesBody,
                    action: GalleyButton.solid(
                      label: l10n.passScanCode,
                      icon: LucideIcons.scanLine,
                      onPressed: () => startPairing(context),
                    ),
                  ),
                ),
              ),
            ],
          );
        }

        final stations = {for (final s in pass.stations) s.runtimeId: s};
        return GalleyPage(
          title: l10n.passTitle,
          controller: _scroll,
          guide: _guide,
          onRefresh: () => model.backend.verifyConnections(),
          actions: [
            IconButton(
              tooltip: l10n.passNewSession,
              icon: const Icon(LucideIcons.plus),
              onPressed: () => showNewSessionSheet(context),
            ),
          ],
          slivers: [
            // 平板分栏：单据就在右侧详情栏里逐张摆着，左栏不再挂一份轨（同一张单据不在
            // 一屏上出现两次），只留一行告诉你有几件事、点了把右栏切到第一张。
            if (isSplitLayout(context))
              SliverToBoxAdapter(
                child: _TicketSummary(
                  count: pass.tickets.length,
                  onOpen: pass.tickets.isEmpty
                      ? null
                      : () => _openTriage(context, 0),
                ),
              )
            else
              SliverLayoutBuilder(
                // 单据宽度跟着所在栏走（平板上指挥台只占列表栏），给下一张留出露边，
                // 一眼看出轨上不止一张。
                builder: (context, constraints) => SliverToBoxAdapter(
                  child: TicketRail(
                    ticketWidth:
                        (constraints.crossAxisExtent -
                                2 * Space.page -
                                Space.ticketPeek)
                            .clamp(Space.ticketMin, Space.ticketMax),
                    stamps: {
                      for (final entry in model.pass.stamps.entries)
                        entry.key: ?stampFor(l10n, entry.value),
                    },
                    emptyChild: Row(
                      children: [
                        Icon(
                          LucideIcons.check,
                          size: Strokes.iconSmall,
                          color: GalleyColors.of(context).inkMuted,
                        ),
                        const SizedBox(width: Space.sm),
                        Text(
                          l10n.passRailEmpty,
                          style: GalleyText.of(context).note.copyWith(
                            color: GalleyColors.of(context).inkMuted,
                          ),
                        ),
                      ],
                    ),
                    entries: [
                      for (final (index, ticket) in pass.tickets.indexed)
                        RailEntry(
                          id: sessionKey(ticket.runtimeId, ticket.session.id),
                          child: PassTicketCard(
                            ticket: ticket,
                            number: index + 1,
                            onOpen: () => _openTriage(context, index),
                          ),
                        ),
                    ],
                  ),
                ),
              ),
            for (final (index, runtime) in fleet.runtimes.indexed)
              SliverToBoxAdapter(
                child: _StationSection(
                  key: _stationKeys.putIfAbsent(runtime.id, GlobalKey.new),
                  runtime: runtime,
                  station: stations[runtime.id],
                  first: index == 0,
                  onShowAll: () => widget.onShowAllSessions?.call(runtime.id),
                ),
              ),
          ],
        );
      },
    );
  }
}

class _StationSection extends StatelessWidget {
  const _StationSection({
    super.key,
    required this.runtime,
    required this.station,
    required this.first,
    required this.onShowAll,
  });

  final FleetRuntime runtime;
  final PassStation? station;
  final bool first;
  final VoidCallback onShowAll;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final color = c.station(runtime.record.accent);
    final s = station;

    String status = linkLabel(l10n, runtime.status);
    if (!runtime.online && s?.savedAtMs != null) {
      // 和设置里同一种说法（重连中 / 离线 / 被移除），再加上数据是几点的。
      status = l10n.machineStatusCached(
        cachedStatusLabel(l10n, runtime.status),
        formatClock(DateTime.fromMillisecondsSinceEpoch(s!.savedAtMs!)),
      );
    }

    final rows = <Widget>[
      for (final session in s?.running ?? const <SessionSummary>[])
        // 缓存里的「在做」只是上次看到的样子：不呼吸、不走表。
        _SessionLine(
          runtime: runtime,
          session: session,
          live: !(s?.cached ?? false),
          stale: s?.cached ?? false,
        ),
      for (final session in s?.done ?? const <SessionSummary>[])
        _SessionLine(runtime: runtime, session: session),
      for (final session in s?.recent ?? const <SessionSummary>[])
        _SessionLine(runtime: runtime, session: session, quiet: true),
    ];

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ShelfHeader(
          first: first,
          leading: StationTape(color: color, height: Strokes.headerTape),
          title: runtime.name,
          trailing: Text(
            status,
            style: t.meta.copyWith(
              color: runtime.needsAttention ? c.danger : c.inkMuted,
            ),
          ),
        ),
        if (runtime.needsAttention)
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Space.page,
              0,
              Space.page,
              Space.sm,
            ),
            child: Align(
              alignment: Alignment.centerLeft,
              child: GalleyButton.quiet(
                label: l10n.passRepair,
                icon: LucideIcons.scanLine,
                onPressed: () => startPairing(context),
              ),
            ),
          ),
        Opacity(
          opacity: runtime.online || rows.isEmpty ? 1 : 0.72,
          child: Shelf(
            children: [
              ...rows,
              ShelfRow(
                title: Text(
                  rows.isEmpty ? l10n.passStationIdle : l10n.passSeeAllSessions,
                ),
                titleStyle: t.callout.copyWith(
                  color: rows.isEmpty ? c.inkMuted : c.ink,
                ),
                trailing: Icon(
                  LucideIcons.chevronRight,
                  size: Strokes.iconSmall,
                  color: c.inkFaint,
                ),
                onTap: onShowAll,
              ),
            ],
          ),
        ),
      ],
    );
  }
}

class _SessionLine extends StatelessWidget {
  const _SessionLine({
    required this.runtime,
    required this.session,
    this.live = false,
    this.quiet = false,
    this.stale = false,
  });

  final FleetRuntime runtime;
  final SessionSummary session;
  final bool live;
  final bool quiet;
  final bool stale;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final host = DetailHost.of(context);
    final key = sessionKey(runtime.id, session.id);
    final updated = fromUnixSeconds(session.updatedAt);
    final selected = host.selectedKey == key;
    return ColoredBox(
      color: selected ? c.control : Colors.transparent,
      child: ShelfRow(
        leading: StatusMark(
          mark: session.mark,
          label: phaseLabel(l10n, session.phase),
          stale: stale,
        ),
        title: Text(session.title),
        titleStyle: quiet ? GalleyText.of(context).callout : null,
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
                [
                  live
                      ? (session.detail ?? phaseLabel(l10n, session.phase))
                      : sessionAgentLabel(l10n, session),
                  ?session.placeLabel,
                ].join(' · '),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
            ),
          ],
        ),
        trailing: live && updated != null
            ? ElapsedLabel(
                since: updated,
                style: GalleyText.of(context).monoTime.copyWith(color: c.ok),
              )
            : ListenableBuilder(
                listenable: Clock.instance,
                builder: (context, _) => Text(
                  formatAgo(l10n, updated, DateTime.now()) ?? '',
                  style: GalleyText.of(context).meta,
                ),
              ),
        onTap: () => openSession(context, runtime.id, session),
      ),
    );
  }
}

/// 平板分栏时左栏顶上的那一行：几件事等你。琥珀只在有事时出现。
class _TicketSummary extends StatelessWidget {
  const _TicketSummary({required this.count, required this.onOpen});

  final int count;
  final VoidCallback? onOpen;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final waiting = count > 0;
    return Shelf(
      children: [
        ShelfRow(
          leading: Icon(
            waiting ? LucideIcons.hand : LucideIcons.check,
            size: Strokes.icon,
            color: waiting ? c.heatInk : c.inkMuted,
          ),
          title: Text(
            waiting ? l10n.passTicketsWaiting(count) : l10n.passRailEmpty,
          ),
          titleStyle: waiting
              ? t.headline.copyWith(color: c.heatInk)
              : t.callout.copyWith(color: c.inkMuted),
          trailing: waiting
              ? Icon(
                  LucideIcons.chevronRight,
                  size: Strokes.iconSmall,
                  color: c.inkFaint,
                )
              : null,
          onTap: onOpen,
        ),
      ],
    );
  }
}
