import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../actions/ticket_bodies.dart';
import 'pass_ticket.dart';

/// 逐张处理：一屏一张单据，处理完盖章、自动滑到下一张；下一张已经在一次横滑之外。
///
/// 打开时记下单据的顺序，之后新来的接在末尾；已处理的单据留着最后一份数据和章，
/// 往回滑还看得到自己刚才做了什么。
class TriagePage extends StatefulWidget {
  const TriagePage({
    super.key,
    required this.initialIndex,
    required this.onOpenSession,
    this.embedded = false,
  });

  /// 嵌在平板详情栏里：没有关闭按钮，「打开对话」也不先退出本页。
  final bool embedded;

  final int initialIndex;

  /// 打开对话 / 终端。由打开本页的地方传进来：本页压在根导航上，拿不到外壳的详情去处。
  final void Function(String runtimeId, SessionSummary session) onOpenSession;

  @override
  State<TriagePage> createState() => _TriagePageState();
}

class _TriagePageState extends State<TriagePage> {
  // 每页只占 9 成宽：下一张单据从右边露出一截，一眼看出还有，横滑就到。
  late final PageController _pages = PageController(
    initialPage: widget.initialIndex,
    viewportFraction: 0.9,
  );
  final _order = <String>[];
  final _last = <String, PassTicket>{};
  late final PassStore _pass;
  int _page = 0;
  Timer? _advance;

  @override
  void initState() {
    super.initState();
    _page = widget.initialIndex;
    _pass = AppScope.of(context).pass;
    _absorb();
    _pass.addListener(_onPass);
  }

  @override
  void dispose() {
    _advance?.cancel();
    _pass.removeListener(_onPass);
    _pages.dispose();
    super.dispose();
  }

  void _absorb() {
    for (final ticket in _pass.value.tickets) {
      final key = sessionKey(ticket.runtimeId, ticket.session.id);
      _last[key] = ticket;
      if (!_order.contains(key)) _order.add(key);
    }
  }

  void _onPass() => setState(_absorb);

  bool _live(String key) => _pass.value.tickets.any(
    (t) => sessionKey(t.runtimeId, t.session.id) == key,
  );

  /// 处理完：等章落下，再滑到下一张还没处理的。
  void _answered() {
    _advance?.cancel();
    _advance = Timer(Motion.resolve(context, Motion.stamp), () {
      if (!mounted) return;
      final next = _nextOpen(_page + 1);
      _pages.animateToPage(
        next ?? _order.length,
        duration: Motion.resolve(context, Motion.ticket),
        curve: Motion.easeOut,
      );
    });
  }

  int? _nextOpen(int from) {
    for (var i = from; i < _order.length; i++) {
      if (_live(_order[i])) return i;
    }
    for (var i = 0; i < from && i < _order.length; i++) {
      if (_live(_order[i])) return i;
    }
    return null;
  }

  void _openCurrentConversation() {
    if (_page >= _order.length) return;
    final ticket = _last[_order[_page]]!;
    if (!widget.embedded) Navigator.of(context).pop();
    widget.onOpenSession(ticket.runtimeId, ticket.session);
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final model = AppScope.of(context);
    final count = _order.length;
    final onDone = _page >= count;
    return Scaffold(
      backgroundColor: c.ground,
      appBar: AppBar(
        automaticallyImplyLeading: false,
        leading: widget.embedded
            ? null
            : IconButton(
                tooltip: l10n.commonClose,
                icon: const Icon(LucideIcons.x),
                onPressed: () => Navigator.of(context).pop(),
              ),
        title: Text(
          onDone ? l10n.passTriageDone : l10n.passTriageTitle(_page + 1, count),
        ),
        actions: [
          if (!onDone)
            IconButton(
              tooltip: l10n.passOpenConversation,
              icon: const Icon(LucideIcons.messageSquare),
              onPressed: _openCurrentConversation,
            ),
        ],
      ),
      // 每一页是一张挂在轨下的单据（自然高度）：单据头、会话最近几条、请求和按钮；
      // 下一张从右边露出一截。第几张在标题里。
      // 单据挂在轨上：和指挥台同一道平涂的轨，横在顶上，每张单据的锯齿顶边塞在轨下。
      body: Stack(
        children: [
          PageView.builder(
            controller: _pages,
            padEnds: false,
            itemCount: count + 1,
            onPageChanged: (page) => setState(() => _page = page),
            itemBuilder: (context, index) {
              if (index >= count) {
                return Center(
                  child: EmptyNote(
                    icon: LucideIcons.checkCheck,
                    title: l10n.passTriageDone,
                    body: l10n.passTriageDoneBody,
                    action: widget.embedded
                        ? null
                        : GalleyButton.solid(
                            label: l10n.commonDone,
                            onPressed: () => Navigator.of(context).pop(),
                          ),
                  ),
                );
              }
              final key = _order[index];
              final ticket = _last[key]!;
              final live = _live(key);
              final stamp = live
                  ? null
                  : stampFor(l10n, model.pass.stamps[key]);
              final padding = EdgeInsets.fromLTRB(
                Space.xs,
                Strokes.railHeight - 2,
                Space.xs,
                MediaQuery.paddingOf(context).bottom + Space.lg,
              );
              // 单据自然高度，挂在轨下；内容长了整页往下滚。不硬撑满屏：撑出来的只是空纸。
              return SingleChildScrollView(
                padding: padding,
                child: AbsorbPointer(
                  absorbing: !live,
                  child: Stack(
                    children: [
                      PassTicketCard(
                        ticket: ticket,
                        number: index + 1,
                        expanded: true,
                        onAnswered: _answered,
                      ),
                      if (stamp != null)
                        Positioned.fill(child: Center(child: stamp)),
                    ],
                  ),
                ),
              );
            },
          ),
          Positioned(
            left: 0,
            right: 0,
            top: 0,
            height: Strokes.railHeight,
            child: ColoredBox(color: c.rail),
          ),
        ],
      ),
    );
  }
}
