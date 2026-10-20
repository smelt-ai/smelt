import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/agent_glyph.dart';
import '../../ui/ticket.dart';
import '../../ui/time.dart';
import '../actions/ticket_bodies.dart';

/// 停靠单据上的一件事：一个待批的审批，或一个询问。
@immutable
class DockItem {
  const DockItem.approval(
    PermissionRequest this.permission, {
    required this.position,
    required this.count,
  }) : elicitation = null;

  const DockItem.question(Elicitation this.elicitation)
    : permission = null,
      position = 1,
      count = 1;

  final PermissionRequest? permission;
  final Elicitation? elicitation;

  /// 第几张、一共几张待批（只对审批有意义）。
  final int position;
  final int count;

  /// 同一件事跨快照稳定：审批按 tool call id，询问按题面。
  String get key => permission != null
      ? 'p:${permission!.toolCallId}'
      : 'e:${elicitation!.message}';
}

/// 招牌动作在对话页的延伸：待处理的审批 / 询问以单据纸停靠在输入框上方。
///
/// 处理后先盖章，再收起；收起的过程里下一张（如果有）补上来。单据可以向下收成一条
/// 单据头，腾出地方看对话。
class TicketDock extends StatefulWidget {
  const TicketDock({
    super.key,
    required this.runtimeId,
    required this.sessionId,
    required this.item,
    required this.collapsed,
    required this.onToggleCollapsed,
    required this.onAnswered,
  });

  final String runtimeId;
  final String sessionId;
  final DockItem? item;
  final bool collapsed;
  final VoidCallback onToggleCollapsed;

  /// 在这张单据上做了决定、内核已收下。页面据此把它从停靠位拿下。
  final void Function(DockItem item) onAnswered;

  @override
  State<TicketDock> createState() => _TicketDockState();
}

class _TicketDockState extends State<TicketDock> {
  DockItem? _shown;
  DateTime _shownAt = DateTime.now();
  String? _answeredHere;
  TicketStamp? _stamp;
  bool _leaving = false;
  Timer? _hold;

  @override
  void initState() {
    super.initState();
    _shown = widget.item;
  }

  @override
  void didUpdateWidget(TicketDock old) {
    super.didUpdateWidget(old);
    final next = widget.item;
    if (_leaving) return; // 收起的计时到点时会取最新的 item。
    if (next?.key == _shown?.key) {
      _shown = next; // 同一件事，内容可能更新了（比如询问里选中了一项）。
      return;
    }
    if (_shown == null) {
      _show(next);
      return;
    }
    _leave();
  }

  void _show(DockItem? item) {
    _shown = item;
    _shownAt = DateTime.now();
    _answeredHere = null;
  }

  /// 当前这张离开：在这里处理掉的先盖章，等章落下再换下一张。
  void _leave() {
    final model = AppScope.of(context);
    final l10n = L10n.of(context);
    final record =
        model.pass.stamps[sessionKey(widget.runtimeId, widget.sessionId)];
    // 只认这张单据挂上之后、在这里盖的章：别处处理掉的单据不冒充是你批的。
    final stamp =
        _answeredHere == _shown?.key &&
            record != null &&
            !record.at.isBefore(_shownAt)
        ? stampFor(l10n, record)
        : null;
    final hold = stamp == null
        ? Duration.zero
        : Motion.resolve(context, Motion.stamp);
    if (hold == Duration.zero) {
      _show(widget.item);
      return;
    }
    _leaving = true;
    _stamp = stamp;
    _hold?.cancel();
    _hold = Timer(hold, () {
      if (!mounted) return;
      setState(() {
        _leaving = false;
        _stamp = null;
        _show(widget.item);
      });
    });
  }

  void _answered(DockItem item) {
    _answeredHere = item.key;
    widget.onAnswered(item);
  }

  @override
  void dispose() {
    _hold?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final item = _shown;
    final duration = Motion.resolve(context, Motion.ticket);
    final child = item == null
        ? const SizedBox(width: double.infinity)
        : Padding(
            padding: const EdgeInsets.only(bottom: Space.sm),
            child: AbsorbPointer(
              absorbing: _leaving,
              child: _DockTicket(
                key: ValueKey(item.key),
                runtimeId: widget.runtimeId,
                sessionId: widget.sessionId,
                item: item,
                stamp: _stamp,
                collapsed: widget.collapsed && !_leaving,
                onToggleCollapsed: widget.onToggleCollapsed,
                onAnswered: () => _answered(item),
              ),
            ),
          );
    // 减少动态效果时直接换，不套 AnimatedSize：零时长的 AnimatedSize 会在自己的布局里
    // 把自己再标脏。
    if (duration == Duration.zero) return child;
    return AnimatedSize(
      duration: duration,
      curve: Motion.easeOut,
      alignment: Alignment.bottomCenter,
      child: child,
    );
  }
}

class _DockTicket extends StatelessWidget {
  const _DockTicket({
    super.key,
    required this.runtimeId,
    required this.sessionId,
    required this.item,
    required this.stamp,
    required this.collapsed,
    required this.onToggleCollapsed,
    required this.onAnswered,
  });

  final String runtimeId;
  final String sessionId;
  final DockItem item;
  final TicketStamp? stamp;
  final bool collapsed;
  final VoidCallback onToggleCollapsed;
  final VoidCallback onAnswered;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final model = AppScope.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([model.fleet, model.pass, model.catalog]),
      builder: (context, _) {
        final runtime = model.fleet.runtime(runtimeId);
        final session = model.catalog.session(runtimeId, sessionId);
        final ticket = model.pass.value.tickets
            .where((t) => t.runtimeId == runtimeId && t.session.id == sessionId)
            .firstOrNull;
        final agent = agentDisplayName(session?.agent ?? '');
        final permission = item.permission;
        final ask = permission != null
            ? (permission.details.command != null
                  ? l10n.passWantsToRun(agent)
                  : l10n.passWantsTo(agent))
            : l10n.passHasQuestion(agent);
        final kind = permission != null
            ? l10n.conversationTicketApproval
            : l10n.conversationTicketQuestion;
        final stationColor = runtime == null
            ? c.inkFaint
            : c.station(runtime.record.accent);

        final head = Semantics(
          button: true,
          expanded: !collapsed,
          label: '$kind, $ask',
          hint: collapsed
              ? l10n.conversationTicketExpand
              : l10n.conversationTicketCollapse,
          excludeSemantics: true,
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: onToggleCollapsed,
            child: ConstrainedBox(
              constraints: const BoxConstraints(minHeight: Space.touch),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                mainAxisSize: MainAxisSize.min,
                children: [
                  TicketHead(
                    leading: [
                      Text(
                        item.count > 1
                            ? '$kind · ${l10n.conversationTicketCount(item.position, item.count)}'
                            : kind,
                      ),
                      Container(
                        width: Strokes.statusDot,
                        height: Strokes.statusDot,
                        color: stationColor,
                      ),
                      Text(runtime?.name ?? runtimeId),
                    ],
                    trailing: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        if (ticket != null)
                          ElapsedLabel(
                            since: DateTime.fromMillisecondsSinceEpoch(
                              ticket.waitingSince,
                            ),
                            style: t.monoHead.copyWith(
                              color: c.onPaper,
                              fontWeight: FontWeight.w700,
                            ),
                          ),
                        SizedBox.square(
                          dimension: Space.xxl + Space.xs,
                          child: Icon(
                            collapsed
                                ? LucideIcons.chevronUp
                                : LucideIcons.chevronDown,
                            size: Strokes.iconSmall,
                            color: c.onPaperMuted,
                          ),
                        ),
                      ],
                    ),
                  ),
                  const SizedBox(height: Space.xs),
                  Row(
                    children: [
                      AgentGlyph(
                        agent: session?.agent ?? '',
                        color: c.onPaperMuted,
                        size: 14,
                      ),
                      const SizedBox(width: Space.xs),
                      Expanded(
                        child: Text(
                          ask,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: t.headline.copyWith(color: c.onPaper),
                        ),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
        );

        final Widget body = permission != null
            ? ApprovalBody(
                runtimeId: runtimeId,
                sessionId: sessionId,
                permission: permission,
                expanded: true,
                onAnswered: onAnswered,
              )
            : ElicitationBody(
                runtimeId: runtimeId,
                sessionId: sessionId,
                elicitation: item.elicitation!,
                expanded: true,
                onAnswered: onAnswered,
              );

        // 键盘弹起时可用高度变小：单据最多占剩下的一小半，多出来的正文在纸里滚。
        final media = MediaQuery.of(context);
        final maxHeight =
            (media.size.height - media.viewInsets.bottom - media.padding.top) *
            0.45;
        return ConstrainedBox(
          constraints: BoxConstraints(maxHeight: maxHeight),
          child: TicketPaper(
            stamp: stamp,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              mainAxisSize: MainAxisSize.min,
              children: [
                head,
                if (!collapsed) ...[
                  const TicketRule(),
                  Flexible(child: SingleChildScrollView(child: body)),
                ],
              ],
            ),
          ),
        );
      },
    );
  }
}
