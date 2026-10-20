import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../../ui/shelf.dart';
import '../../ui/status.dart';
import '../../ui/time.dart';
import '../session/open_session.dart';
import 'agents_board.dart';
import 'agents_widgets.dart';
import 'automation_text.dart';

/// 一条自动化的详情：它什么时候跑、跑的是什么、上次怎么样；能开关、能立即跑一次。
///
/// 手机是展示层：这里不编辑触发条件和动作，只开放两个幂等、可在桌面撤销的写操作。
/// 数据从 [AgentsBoard] 读，列表和详情看的是同一份。
class AutomationDetailPage extends StatelessWidget {
  const AutomationDetailPage({
    super.key,
    required this.board,
    required this.runtimeId,
    required this.automationId,
  });

  final AgentsBoard board;
  final String runtimeId;
  final String automationId;

  @override
  Widget build(BuildContext context) {
    final model = AppScope.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([board, model.fleet, model.catalog]),
      builder: (context, _) {
        final runtime = model.fleet.runtime(runtimeId);
        final automation = board.automation(runtimeId, automationId);
        if (automation == null) {
          return _GonePage(machine: runtime?.name ?? '');
        }
        return _AutomationDetail(
          board: board,
          runtime: runtime,
          runtimeId: runtimeId,
          automation: automation,
          runSession: switch (automation.lastRun?.sessionId) {
            final id? => model.catalog.session(runtimeId, id),
            null => null,
          },
        );
      },
    );
  }
}

class _GonePage extends StatelessWidget {
  const _GonePage({required this.machine});

  final String machine;

  @override
  Widget build(BuildContext context) => Scaffold(
    backgroundColor: GalleyColors.of(context).ground,
    appBar: AppBar(),
    body: Center(
      child: EmptyNote(
        icon: LucideIcons.circleSlash,
        title: L10n.of(context).agentsGone(machine),
      ),
    ),
  );
}

class _AutomationDetail extends StatelessWidget {
  const _AutomationDetail({
    required this.board,
    required this.runtime,
    required this.runtimeId,
    required this.automation,
    required this.runSession,
  });

  final AgentsBoard board;
  final FleetRuntime? runtime;
  final String runtimeId;
  final AutomationSummary automation;

  /// 上次运行的会话，还在会话目录里才有。
  final SessionSummary? runSession;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final online = runtime?.online ?? false;
    final enabled = board.enabled(runtimeId, automation);
    final running = board.isRunningOnce(runtimeId, automation.id);
    final labels = triggerLabels(l10n, automation);
    final nextRun = switch (automation.nextRunAt) {
      final secs? when enabled => fromUnixSeconds(secs),
      _ => null,
    };

    return Scaffold(
      backgroundColor: c.ground,
      appBar: AppBar(
        backgroundColor: c.ground,
        title: DetailTitle(title: automation.name, runtime: runtime),
      ),
      body: ListView(
        padding: const EdgeInsets.only(bottom: Space.xxl),
        children: [
          if (!online)
            Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.page + Space.xs,
                Space.sm,
                Space.page + Space.xs,
                0,
              ),
              child: Text(
                l10n.agentsMachineOffline(runtime?.name ?? ''),
                style: t.meta.copyWith(color: c.inkMuted),
              ),
            ),
          const SizedBox(height: Space.sm),
          Shelf(
            children: [
              MergeSemantics(
                child: ShelfRow(
                  title: Text(l10n.agentsEnabled),
                  trailing: Switch(
                    value: enabled,
                    onChanged: online
                        ? (value) => toggleAutomation(
                            context,
                            board,
                            runtimeId,
                            automation,
                            value,
                          )
                        : null,
                  ),
                ),
              ),
              if (nextRun != null)
                ListenableBuilder(
                  listenable: Clock.instance,
                  builder: (context, _) => FieldRow(
                    label: l10n.agentsNextRunTitle,
                    value: formatNextRun(
                      l10n,
                      MaterialLocalizations.of(context),
                      nextRun,
                      DateTime.now(),
                    ),
                  ),
                ),
            ],
          ),
          ShelfHeader(title: l10n.agentsTrigger),
          Shelf(
            children: [
              if (labels.isEmpty)
                NoteRow(text: l10n.agentsTriggerNone)
              else ...[
                for (final s in automation.schedules)
                  _IconLine(
                    icon: LucideIcons.clock,
                    text: scheduleLabel(l10n, s),
                  ),
                for (final topic in automation.eventTopics)
                  _IconLine(
                    icon: LucideIcons.zap,
                    text: l10n.agentsTriggerEvent(topic),
                  ),
                if (automation.webhook)
                  _IconLine(
                    icon: LucideIcons.webhook,
                    text: l10n.agentsTriggerWebhook,
                  ),
              ],
            ],
          ),
          ShelfHeader(title: l10n.agentsAction),
          Shelf(
            children: [
              _IconLine(
                icon: automation.action == AutomationActionKind.shell
                    ? LucideIcons.squareTerminal
                    : LucideIcons.bot,
                text: actionLabel(l10n, automation),
                danger:
                    automation.action == AutomationActionKind.agent &&
                    (automation.agentName?.trim().isEmpty ?? true),
              ),
            ],
          ),
          ShelfHeader(title: l10n.agentsLastRun),
          _LastRun(
            run: automation.lastRun,
            runtimeId: runtimeId,
            session: runSession,
          ),
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Space.page + Space.xs,
              Space.lg,
              Space.page + Space.xs,
              0,
            ),
            child: Text(
              l10n.agentsReadOnly,
              style: t.meta.copyWith(color: c.inkMuted),
            ),
          ),
        ],
      ),
      bottomNavigationBar: BottomActionBar(
        child: GalleyButton.solid(
          expand: true,
          icon: LucideIcons.play,
          label: running ? l10n.agentsStarting : l10n.agentsRunNow,
          onPressed: online && !running
              ? () => runAutomationNow(context, board, runtimeId, automation)
              : null,
        ),
      ),
    );
  }
}

/// 最近一次运行：状态、来源、起止时间、错误，以及能跳进那次执行现场的入口。
class _LastRun extends StatelessWidget {
  const _LastRun({
    required this.run,
    required this.runtimeId,
    required this.session,
  });

  final AutomationRunSummary? run;
  final String runtimeId;
  final SessionSummary? session;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final material = MaterialLocalizations.of(context);
    final r = run;
    if (r == null) return Shelf(children: [NoteRow(text: l10n.agentsNeverRun)]);

    final now = DateTime.now();
    final started = r.startedAt == null ? null : fromUnixSeconds(r.startedAt!);
    final finished = r.finishedAt == null
        ? null
        : fromUnixSeconds(r.finishedAt!);
    final status = runStatusLabel(l10n, r.status);
    final error = r.error?.trim();
    final s = session;
    return Shelf(
      children: [
        ShelfRow(
          leading: StatusMark(mark: runMark(r.status), label: status),
          title: Text(status),
        ),
        FieldRow(
          label: l10n.agentsRunSource,
          value: runSourceLabel(l10n, r.source),
        ),
        if (started != null)
          FieldRow(
            label: l10n.agentsRunStartedAt,
            value: formatMoment(l10n, material, started, now),
          ),
        if (finished != null)
          FieldRow(
            label: l10n.agentsRunFinishedAt,
            value: formatMoment(l10n, material, finished, now),
          ),
        if (started != null && finished != null)
          FieldRow(
            label: l10n.agentsRunDuration,
            value: formatElapsed(finished.difference(started)),
            valueStyle: t.monoTime,
          ),
        if (error != null && error.isNotEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(
              horizontal: Space.lg,
              vertical: Space.md,
            ),
            child: SelectableText(
              error,
              style: t.body.copyWith(color: c.danger),
            ),
          ),
        if (s != null)
          ShelfRow(
            leading: Icon(
              LucideIcons.messageSquare,
              size: Strokes.icon,
              color: c.ink,
            ),
            title: Text(l10n.agentsOpenRun),
            titleStyle: t.body,
            trailing: Icon(
              LucideIcons.chevronRight,
              size: Strokes.iconSmall,
              color: c.inkFaint,
            ),
            onTap: () => openSession(context, runtimeId, s),
          ),
      ],
    );
  }
}

class _IconLine extends StatelessWidget {
  const _IconLine({
    required this.icon,
    required this.text,
    this.danger = false,
  });

  final IconData icon;
  final String text;
  final bool danger;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final color = danger ? c.danger : c.ink;
    return ConstrainedBox(
      constraints: const BoxConstraints(minHeight: Space.touch),
      child: Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Space.lg,
          vertical: Space.md,
        ),
        child: Row(
          children: [
            Icon(
              icon,
              size: Strokes.icon,
              color: danger ? c.danger : c.inkMuted,
            ),
            const SizedBox(width: Space.md),
            Expanded(
              child: Text(text, style: t.body.copyWith(color: color)),
            ),
          ],
        ),
      ),
    );
  }
}
