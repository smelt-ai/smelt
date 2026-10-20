import 'package:flutter/material.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/agent_glyph.dart';
import '../../ui/time.dart';
import '../../ui/ticket.dart';
import '../actions/ticket_bodies.dart';
import 'ticket_context.dart';

/// 一张单据：单据头（序号 · 机器 · 等了多久）、标题、agent 在要什么、动作。
class PassTicketCard extends StatelessWidget {
  const PassTicketCard({
    super.key,
    required this.ticket,
    required this.number,
    this.expanded = false,
    this.onOpen,
    this.onAnswered,
  });

  final PassTicket ticket;
  final int number;
  final bool expanded;

  /// 点单据（或紧凑版里的「回答」）：进逐张处理。
  final VoidCallback? onOpen;
  final VoidCallback? onAnswered;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final model = AppScope.of(context);
    final session = ticket.session;
    final runtime = model.fleet.runtime(ticket.runtimeId);
    final agent = agentDisplayName(session.agent);
    final failed =
        ticket.permission == null &&
        ticket.elicitation == null &&
        (session.phase == SessionPhase.failed ||
            session.attention?.kind == AttentionKind.failure);

    final ask = switch (ticket) {
      PassTicket(permission: final p?) when p.details.command != null =>
        l10n.passWantsToRun(agent),
      PassTicket(permission: _?) => l10n.passWantsTo(agent),
      PassTicket(elicitation: _?) => l10n.passHasQuestion(agent),
      _ when failed => l10n.passFailed(agent),
      _ => phaseLabel(l10n, session.phase),
    };

    final Widget body;
    if (ticket.permission case final permission?) {
      body = ApprovalBody(
        runtimeId: ticket.runtimeId,
        sessionId: session.id,
        permission: permission,
        expanded: expanded,
        onAnswered: onAnswered,
      );
    } else if (ticket.elicitation case final elicitation?) {
      body = ElicitationBody(
        runtimeId: ticket.runtimeId,
        sessionId: session.id,
        elicitation: elicitation,
        expanded: expanded,
        onOpen: onOpen,
        onAnswered: onAnswered,
      );
    } else if (!ticket.detailsLoaded) {
      body = const TicketSkeleton();
    } else {
      final message = session.attention?.message ?? session.detail;
      body = message == null
          ? const SizedBox.shrink()
          : TicketExcerpt(message, mono: false, maxLines: expanded ? 12 : 3);
    }

    final waitingSince = DateTime.fromMillisecondsSinceEpoch(
      ticket.waitingSince,
    );
    final stationColor = runtime == null
        ? c.inkFaint
        : c.station(runtime.record.accent);

    return Semantics(
      container: true,
      label: '${session.title}, $ask',
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: expanded ? null : onOpen,
        child: TicketPaper(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            mainAxisSize: MainAxisSize.min,
            children: [
              TicketHead(
                leading: [
                  Text(l10n.passTicketNumber(number)),
                  Container(width: 8, height: 8, color: stationColor),
                  Text(runtime?.record.name ?? ticket.runtimeId),
                ],
                trailing: ElapsedLabel(
                  since: waitingSince,
                  style: t.monoHead.copyWith(
                    color: failed ? c.danger : c.onPaper,
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ),
              const SizedBox(height: Space.sm),
              Text(
                session.title,
                maxLines: expanded ? 3 : 1,
                overflow: TextOverflow.ellipsis,
                style: t.headline.copyWith(color: c.onPaper),
              ),
              const SizedBox(height: Space.xxs),
              Row(
                children: [
                  AgentGlyph(
                    agent: session.agent,
                    terminal: session.isTerminal,
                    color: c.onPaperMuted,
                    size: 14,
                  ),
                  const SizedBox(width: Space.xs),
                  Expanded(
                    child: Text(
                      [ask, ?session.placeLabel].join(' · '),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: t.meta.copyWith(color: c.onPaperMuted),
                    ),
                  ),
                ],
              ),
              const TicketRule(),
              // 完整版（逐张处理）：先印会话最近几条（它刚才在做什么），再是请求和按钮。
              // 单据保持自然高度，不硬撑满屏：撑出来的只会是一段空纸。
              if (expanded && ticket.recent.isNotEmpty) ...[
                TicketContext(entries: ticket.recent),
                const TicketRule(),
              ],
              body,
            ],
          ),
        ),
      ),
    );
  }
}
