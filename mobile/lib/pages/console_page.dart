import 'package:flutter/material.dart';

import '../services/gateway_service.dart';
import '../theme/project_accent.dart';
import '../util/relative_time.dart';
import '../services/pending_actions_controller.dart';
import '../widgets/agent_icon.dart';
import '../widgets/approval_card.dart';
import '../widgets/automation_source.dart';
import '../widgets/session_row.dart';

/// 指挥台：打开 App 第一眼回答的是「有事等我吗」，而不是「我有哪些项目」。
///
/// `docs/product-roadmap.md §6` 早就写死了定位——手机是指挥台，不是第二块小终端。
/// 当前实现却把项目树当首屏，审批被埋在两层浏览动作后面。这一页把顺序倒过来。
///
/// 三段分组沿用 `ui-design-language.md` 里 Codex 那套：同一面板内不同性质的信息
/// 分组、每组有标题、组内才是列表。
class ConsolePage extends StatefulWidget {
  const ConsolePage({
    super.key,
    required this.onOpenSession,
    this.controller,
    this.onRespond,
  });

  final void Function(SessionSummary session) onOpenSession;

  /// 留出注入口，单测不必去动那个 final 全局单例。
  final PendingActionsController? controller;
  final void Function(String sessionId, String toolCallId, String optionId)?
  onRespond;

  @override
  State<ConsolePage> createState() => _ConsolePageState();
}

class _ConsolePageState extends State<ConsolePage> {
  late final PendingActionsController _controller;
  late final bool _ownsController;

  /// 已经发出去、还没等到结果的审批。按 toolCallId 记，避免连点两次。
  final Set<String> _submitting = {};

  @override
  void initState() {
    super.initState();
    _ownsController = widget.controller == null;
    _controller = widget.controller ?? PendingActionsController();
    _controller.addListener(_onChanged);
  }

  void _onChanged() {
    if (!mounted) return;
    setState(() {
      // 卡片消失即代表决策已生效，可以把「提交中」清掉。
      final live = _controller.items
          .map((item) => item.permission?.toolCallId)
          .whereType<String>()
          .toSet();
      _submitting.removeWhere((id) => !live.contains(id));
    });
  }

  @override
  void dispose() {
    _controller.removeListener(_onChanged);
    if (_ownsController) _controller.dispose();
    super.dispose();
  }

  void _respond(String sessionId, String toolCallId, String optionId) {
    setState(() => _submitting.add(toolCallId));
    final respond =
        widget.onRespond ??
        (String s, String t, String o) =>
            gatewayService.respondApproval(s, t, o);
    respond(sessionId, toolCallId, optionId);
  }

  @override
  Widget build(BuildContext context) {
    final waiting = _controller.items;
    final running = _controller.running;
    final done = _controller.recentlyDone;
    final recent = _controller.recentRuns;
    final idle = _controller.idle;

    if (waiting.isEmpty &&
        running.isEmpty &&
        done.isEmpty &&
        recent.isEmpty &&
        idle.isEmpty) {
      return const _ConsoleEmptyState();
    }

    return ListView(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 88),
      children: [
        if (waiting.isNotEmpty) ...[
          _SectionHeader(label: 'Needs you', count: waiting.length),
          for (final item in waiting)
            Padding(
              padding: const EdgeInsets.only(bottom: 10),
              child: _PendingCard(
                item: item,
                submitting:
                    item.permission != null &&
                    _submitting.contains(item.permission!.toolCallId),
                onOpen: () => widget.onOpenSession(item.session),
                onRespond: (optionId) => _respond(
                  item.sessionId,
                  item.permission!.toolCallId,
                  optionId,
                ),
              ),
            ),
        ],
        if (running.isNotEmpty) ...[
          _SectionHeader(label: 'Running', count: running.length),
          for (final session in running)
            _RunningTile(
              session: session,
              onTap: () => widget.onOpenSession(session),
            ),
        ],
        // 「刚完成」：跑完且你还没看过。设计稿的第三段，原来整段缺失——结果是
        // 一台跑完活儿的机器在指挥台上看起来跟没开机一样。
        if (done.isNotEmpty) ...[
          _SectionHeader(label: 'Recently done', count: done.length),
          for (final session in done)
            _DoneTile(
              session: session,
              onTap: () => widget.onOpenSession(session),
            ),
        ],
        // 「最近跑过」：从上一段里掉出来的（看过之后协议就把 done 降级成 idle）。
        // 没有这一段的话，一次跑完的会话在指挥台上会凭空消失，用户失去「最近跑
        // 的是哪个」这条线索。
        if (recent.isNotEmpty) ...[
          _SectionHeader(label: 'Recently ran', count: recent.length),
          for (final session in recent)
            _DoneTile(
              session: session,
              onTap: () => widget.onOpenSession(session),
            ),
        ],
        // 「闲置」：上面四段都不要的。有了这一段，指挥台才真的是**全部**对话
        // ——包括几乎永远闲置的智能体对话，它们在 Projects 的项目树里没有位置。
        if (idle.isNotEmpty) ...[
          _SectionHeader(label: 'Idle', count: idle.length),
          for (final session in idle)
            _DoneTile(
              session: session,
              onTap: () => widget.onOpenSession(session),
            ),
        ],
      ],
    );
  }
}

class _SectionHeader extends StatelessWidget {
  const _SectionHeader({required this.label, required this.count});

  final String label;
  final int count;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 8, 4, 8),
      child: Row(
        children: [
          Text(
            label.toUpperCase(),
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
              letterSpacing: 0.8,
              fontWeight: FontWeight.w600,
            ),
          ),
          const SizedBox(width: 8),
          Text(
            '$count',
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}

class _PendingCard extends StatelessWidget {
  const _PendingCard({
    required this.item,
    required this.submitting,
    required this.onOpen,
    required this.onRespond,
  });

  final PendingActionItem item;
  final bool submitting;
  final VoidCallback onOpen;
  final void Function(String optionId) onRespond;

  @override
  Widget build(BuildContext context) {
    final header = _CardHeader(session: item.session, onTap: onOpen);

    // 详情还没到：先把「谁在等」摆出来，别让整张卡空着。attention 推送是即时的，
    // 快照要多跑一个来回，这段空窗期用户至少知道有事发生。
    if (item.permission == null) {
      return Card(
        margin: EdgeInsets.zero,
        child: ListTile(
          title: header,
          subtitle: Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Text(
              item.elicitation?.message ?? item.question,
              maxLines: 3,
              overflow: TextOverflow.ellipsis,
            ),
          ),
          trailing: item.elicitation != null
              ? const Icon(Icons.chevron_right)
              : const SizedBox(
                  width: 16,
                  height: 16,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
          onTap: onOpen,
        ),
      );
    }

    final session = item.session;
    final automation = session.automation;
    final subtitle = [
      if (automation != null)
        automation.automationName
      else if (session.projectTitle?.trim().isNotEmpty == true)
        session.projectTitle!.trim(),
      if (automation != null) automationTriggerLabel(automation.runSource),
      if (session.agent.trim().isNotEmpty) session.agent.trim(),
      ?formatRelativeEpochSeconds(session.updatedAt),
    ].join(' · ');

    return ApprovalCard(
      permission: item.permission!,
      submitting: submitting,
      header: header,
      detailSubtitle: subtitle,
      showWorkspacePath: automation == null,
      onRespond: onRespond,
    );
  }
}

/// 卡片头：用户自己开的会话画项目身份，自动化 Run 画来源和这次的输入。
///
/// 分岔而不是在同一行里塞两种含义——Run 没有项目，硬套项目色块会指向一个用户
/// 从没见过的 daemon 工作区路径。
class _CardHeader extends StatelessWidget {
  const _CardHeader({required this.session, required this.onTap});

  final SessionSummary session;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final automation = session.automation;
    if (automation == null) {
      return _SessionHeader(session: session, onTap: onTap);
    }
    final prompt = automation.prompt?.trim();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        AutomationSourceHeader(source: automation, onTap: onTap),
        if (prompt != null && prompt.isNotEmpty)
          AutomationRunInput(prompt: prompt),
      ],
    );
  }
}

/// 卡片上的会话身份（设计稿 A 的 meta 行）。
///
/// 指挥台把多个会话摆在一起，「这是谁」必须一眼可辨，否则就是在无上下文地批准
/// 命令。所以这一行给足四件事：项目色块、项目名、agent、多久以前。
class _SessionHeader extends StatelessWidget {
  const _SessionHeader({required this.session, required this.onTap});

  final SessionSummary session;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.colorScheme.onSurfaceVariant;
    final project = session.projectTitle?.trim();
    final label = project?.isNotEmpty == true ? project! : session.title;
    final age = formatRelativeEpochSeconds(session.updatedAt);

    return InkWell(
      onTap: onTap,
      child: Row(
        children: [
          Container(
            width: 10,
            height: 10,
            decoration: BoxDecoration(
              color: projectAccent(
                session.projectRoot ?? label,
                theme.brightness,
              ),
              borderRadius: BorderRadius.circular(3),
            ),
          ),
          const SizedBox(width: 7),
          Flexible(
            child: Text(
              label,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.labelMedium?.copyWith(
                color: theme.colorScheme.onSurface,
                fontWeight: FontWeight.w600,
              ),
            ),
          ),
          if (session.agent.trim().isNotEmpty) ...[
            const SizedBox(width: 6),
            _AgentChip(agent: session.agent),
          ],
          const Spacer(),
          if (age != null)
            Text(
              age,
              style: theme.textTheme.labelSmall?.copyWith(color: muted),
            ),
        ],
      ),
    );
  }
}

/// agent 名做成 chip 而不是裸文字：它是分类标签，跟项目名不是一个层级。
///
/// 图标用 `AgentGlyph` 而不是 `AgentIcon`：这里**不该**按状态染色。审批卡本身就是
/// 「在等你」，chip 再喊一遍同一件事只会跟卡片抢注意力——它在这儿的职责只有分类。
class _AgentChip extends StatelessWidget {
  const _AgentChip({required this.agent});

  final String agent;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      padding: const EdgeInsets.fromLTRB(5, 2, 6, 2),
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(5),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          AgentGlyph(agent: agent, size: 11),
          const SizedBox(width: 4),
          Text(
            agent,
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
              fontWeight: FontWeight.w500,
            ),
          ),
        ],
      ),
    );
  }
}

/// 「正在跑」的一行（设计稿 A）：呼吸中的 agent 图标 + 标题 + 项目·当前动作 +
/// 多久以前。
class _RunningTile extends StatelessWidget {
  const _RunningTile({required this.session, required this.onTap});

  final SessionSummary session;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return _ConsoleRow(
      session: session,
      onTap: onTap,
      // 只有这一段保留脉冲。「还在动」是 Running 段的全部意义——静止的图标没法把
      // 「正在跑」和「跑完了」区分开，光靠颜色差别太弱。脉冲作用在图标上而不是
      // 换成圆点，身份和活性就都保住了。
      leading: _Pulse(
        key: _pulseKey,
        child: AgentIcon(session: session),
      ),
    );
  }
}

/// 「刚完成」/「最近跑过」的一行。跟 running 同构，只是图标不再呼吸。
class _DoneTile extends StatelessWidget {
  const _DoneTile({required this.session, required this.onTap});

  final SessionSummary session;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return _ConsoleRow(
      session: session,
      onTap: onTap,
      leading: AgentIcon(session: session),
    );
  }
}

class _ConsoleRow extends StatelessWidget {
  const _ConsoleRow({
    required this.session,
    required this.onTap,
    required this.leading,
  });

  final SessionSummary session;
  final VoidCallback onTap;
  final Widget leading;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final age = formatRelativeEpochSeconds(session.updatedAt);
    return SessionRow(
      session: session,
      onTap: onTap,
      leading: leading,
      // 指挥台是跨项目的分诊列表，光看标题认不出是哪个项目的事。
      showProject: true,
      trailing: age == null
          ? null
          : Text(
              age,
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
    );
  }
}

/// 运行中的脉冲点。设计稿里 running 是「活的」——静止的圆点跟已完成分不出来，
/// 而这两者的处置方式完全不同。
/// 让 child 缓慢呼吸。
///
/// 原来这里是个自带绘制的脉冲圆点，现在只负责动效、把画什么留给调用方——指挥台的
/// 行已经改用 agent 图标，圆点那套绘制没有了去处。
/// 给测试一个抓手：`FadeTransition` 满树都是（`MaterialPageRoute` 的转场就是
/// 一个），按类型找会误命中路由动画。
const _pulseKey = Key('console-pulse');

class _Pulse extends StatefulWidget {
  const _Pulse({super.key, required this.child});

  final Widget child;

  @override
  State<_Pulse> createState() => _PulseState();
}

class _PulseState extends State<_Pulse> with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1100),
  )..repeat(reverse: true);

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    // 关掉动效的用户不该被闪烁打扰，直接给静止的图标。
    if (MediaQuery.maybeDisableAnimationsOf(context) ?? false) {
      return widget.child;
    }
    return FadeTransition(
      opacity: _controller.drive(Tween(begin: 0.45, end: 1)),
      child: widget.child,
    );
  }
}

class _ConsoleEmptyState extends StatelessWidget {
  const _ConsoleEmptyState();

  @override
  Widget build(BuildContext context) {
    final muted = Theme.of(context).colorScheme.onSurfaceVariant;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.check_circle_outline, size: 44, color: muted),
            const SizedBox(height: 12),
            Text(
              'Nothing needs you',
              style: Theme.of(context).textTheme.titleMedium,
            ),
            const SizedBox(height: 6),
            Text(
              'Approvals and running sessions show up here.',
              textAlign: TextAlign.center,
              style: TextStyle(color: muted),
            ),
          ],
        ),
      ),
    );
  }
}
