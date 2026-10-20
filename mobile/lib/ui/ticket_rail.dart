import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../theme/galley.dart';
import 'ticket.dart';

/// 轨上的一张单据。[id] 跨重建稳定（运行时 id + 会话 id）。
class RailEntry {
  const RailEntry({required this.id, required this.child});

  final String id;
  final Widget child;
}

/// 招牌动作：单据轨。
///
/// 一道钢灰的轨横在指挥台顶上，需要你动手的单据挂在轨上横排。
/// 新单据从右边挂上来、轻轻晃一下；单据离开时如果有章（[stamps]），先盖章、再上滑离轨，
/// 旁边的单据补位。系统要求减少动态效果时，挂上和离开都是直接出现 / 消失。
class TicketRail extends StatefulWidget {
  const TicketRail({
    super.key,
    required this.entries,
    this.stamps = const {},
    this.ticketWidth = 296,
    this.emptyChild,
  });

  final List<RailEntry> entries;

  /// 刚处理掉的单据该盖什么章。单据从 [entries] 消失时查这里。
  final Map<String, TicketStamp> stamps;
  final double ticketWidth;

  /// 没有单据时轨下显示的东西（一句「没有等你处理的事」）。
  final Widget? emptyChild;

  @override
  State<TicketRail> createState() => _TicketRailState();
}

class _Slot {
  _Slot(this.id, this.child, {required this.entering});

  final String id;
  Widget child;
  bool entering;
  bool leaving = false;
  TicketStamp? stamp;
}

class _TicketRailState extends State<TicketRail> {
  late List<_Slot> _slots = [
    for (final e in widget.entries) _Slot(e.id, e.child, entering: false),
  ];
  final _timers = <String, Timer>{};

  @override
  void didUpdateWidget(TicketRail old) {
    super.didUpdateWidget(old);
    final incoming = {for (final e in widget.entries) e.id: e};
    final next = <_Slot>[];
    // 先按新顺序排在场的，再把正在离开的插回它们原来的位置附近。
    final existing = {for (final s in _slots) s.id: s};
    for (final e in widget.entries) {
      final slot = existing[e.id];
      if (slot != null && !slot.leaving) {
        slot.child = e.child;
        next.add(slot);
      } else {
        next.add(_Slot(e.id, e.child, entering: true));
      }
    }
    final reduce = Motion.resolve(context, Motion.ticket) == Duration.zero;
    for (final (index, slot) in _slots.indexed) {
      if (incoming.containsKey(slot.id) && !slot.leaving) continue;
      // 减少动态效果：离开的单据直接消失，不留离轨动画的位置。
      if (reduce) {
        _timers.remove(slot.id)?.cancel();
        continue;
      }
      if (!slot.leaving) {
        slot.leaving = true;
        slot.stamp = widget.stamps[slot.id];
        _scheduleRemoval(slot);
      }
      next.insert(math.min(index, next.length), slot);
    }
    _slots = next;
  }

  void _scheduleRemoval(_Slot slot) {
    final hold =
        (slot.stamp != null ? Motion.stamp : Duration.zero) + Motion.ticket;
    _timers[slot.id]?.cancel();
    _timers[slot.id] = Timer(hold, () {
      if (!mounted) return;
      setState(() => _slots.removeWhere((s) => identical(s, slot)));
      _timers.remove(slot.id);
    });
  }

  @override
  void dispose() {
    for (final t in _timers.values) {
      t.cancel();
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final hasTickets = _slots.isNotEmpty;
    return Stack(
      children: [
        Padding(
          padding: const EdgeInsets.only(top: Strokes.railHeight - 2),
          child: hasTickets
              ? SingleChildScrollView(
                  scrollDirection: Axis.horizontal,
                  padding: const EdgeInsets.fromLTRB(
                    Space.page,
                    0,
                    Space.page,
                    Space.lg,
                  ),
                  clipBehavior: Clip.none,
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      for (final slot in _slots)
                        _RailSlot(
                          key: ValueKey(slot.id),
                          entering: slot.entering,
                          leaving: slot.leaving,
                          stamp: slot.stamp,
                          width: widget.ticketWidth,
                          child: slot.child,
                        ),
                    ],
                  ),
                )
              : Padding(
                  padding: const EdgeInsets.fromLTRB(
                    Space.page,
                    Space.md,
                    Space.page,
                    Space.md,
                  ),
                  child: widget.emptyChild ?? const SizedBox.shrink(),
                ),
        ),
        // 轨画在单据上面：单据顶边塞在轨下。一道平涂的结构线，不做渐变斜面。
        Positioned(
          left: 0,
          right: 0,
          top: 0,
          height: Strokes.railHeight,
          child: ColoredBox(color: c.rail),
        ),
      ],
    );
  }
}

class _RailSlot extends StatefulWidget {
  const _RailSlot({
    super.key,
    required this.entering,
    required this.leaving,
    required this.stamp,
    required this.width,
    required this.child,
  });

  final bool entering;
  final bool leaving;
  final TicketStamp? stamp;
  final double width;
  final Widget child;

  @override
  State<_RailSlot> createState() => _RailSlotState();
}

class _RailSlotState extends State<_RailSlot> with TickerProviderStateMixin {
  late final AnimationController _enter = AnimationController(
    vsync: this,
    duration: Motion.ticket,
    value: widget.entering ? 0 : 1,
  );
  late final AnimationController _leave = AnimationController(
    vsync: this,
    duration: Motion.ticket,
  );
  Timer? _leaveDelay;

  @override
  void initState() {
    super.initState();
    if (widget.entering) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (!mounted) return;
        if (Motion.resolve(context, Motion.ticket) == Duration.zero) {
          _enter.value = 1;
        } else {
          _enter.forward();
        }
      });
    }
  }

  @override
  void didUpdateWidget(_RailSlot old) {
    super.didUpdateWidget(old);
    if (widget.leaving && !old.leaving) {
      if (Motion.resolve(context, Motion.ticket) == Duration.zero) {
        _leave.value = 1;
        return;
      }
      // 有章就先让章落下来，再离轨。
      _leaveDelay = Timer(
        widget.stamp != null ? Motion.stamp : Duration.zero,
        () {
          if (mounted) _leave.forward();
        },
      );
    }
  }

  @override
  void dispose() {
    _leaveDelay?.cancel();
    _enter.dispose();
    _leave.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: Listenable.merge([_enter, _leave]),
      builder: (context, child) {
        final e = Motion.easeOut.transform(_enter.value);
        final l = Motion.easeInOut.transform(_leave.value);
        // 挂上来时绕顶边中点晃一下，幅度随时间衰减。
        final swing = 0.05 * math.sin(e * math.pi * 2.5) * (1 - e);
        return Align(
          alignment: Alignment.topLeft,
          widthFactor: 1 - l,
          child: Opacity(
            opacity: (e * (1 - l)).clamp(0, 1),
            child: Transform.translate(
              offset: Offset(40 * (1 - e), -28 * l),
              child: Transform.rotate(
                angle: swing,
                alignment: Alignment.topCenter,
                child: child,
              ),
            ),
          ),
        );
      },
      child: Padding(
        padding: const EdgeInsets.only(right: Space.md),
        child: SizedBox(
          width: widget.width,
          child: widget.stamp == null || !widget.leaving
              ? widget.child
              : Stack(
                  children: [
                    widget.child,
                    Positioned.fill(child: Center(child: widget.stamp!)),
                  ],
                ),
        ),
      ),
    );
  }
}
