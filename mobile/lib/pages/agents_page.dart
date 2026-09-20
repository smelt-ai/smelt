import 'dart:async';

import 'package:flutter/material.dart';

import '../services/gateway_service.dart';
import '../theme/smelt_theme.dart';
import '../util/relative_time.dart';
import '../widgets/automation_source.dart';

/// 「智能体」栏。回答两个问题：**它什么时候会自己动**，以及**它为什么这么干**。
///
/// 手机是展示层（见 `docs/mobile-agents-ux.md` §0）：这一页不创建、不编辑、不删除。
/// 唯一开放的两个写操作是启停和「立即运行一次」——它们都是幂等的、可在桌面撤销的，
/// 而且恰好是人在外面时真正需要的两个动作（「今天别跑了」「现在就跑一次」）。
enum AgentsSegment { automations, agents }

class AgentsPage extends StatefulWidget {
  const AgentsPage({
    super.key,
    required this.onOpenSession,
    required this.startableAgentIds,
    required this.onStartConversation,
  });

  /// 从「上次运行」跳进那次执行现场。给的是会话 id，由 home 决定怎么打开。
  final void Function(String sessionId) onOpenSession;

  /// 能直接开对话的智能体定义 id。由工作区目录决定：引擎没注册成产品级引擎的
  /// 智能体在这里不出现，桌面也一样开不了。
  final Set<String> startableAgentIds;

  /// 开一场不绑项目的智能体对话，落在智能体自己的 space。
  final void Function(String agentDefinitionId) onStartConversation;

  @override
  State<AgentsPage> createState() => _AgentsPageState();
}

class _AgentsPageState extends State<AgentsPage> {
  StreamSubscription<AutomationCatalog>? _catalogSubscription;
  StreamSubscription<WsState>? _stateSubscription;
  AutomationCatalog? _catalog;

  @override
  void initState() {
    super.initState();
    // 先用最近一次的目录铺上，避免每次切回这一栏都闪一下「正在同步」。
    _catalog = gatewayService.lastAutomationCatalog;
    _catalogSubscription = gatewayService.automationCatalogStream.listen((
      catalog,
    ) {
      if (mounted) setState(() => _catalog = catalog);
    });
    // 连上时 `GatewayService` 已经拉过一次（会话行也要用这份目录），这里只补
    // 「进这一栏时还没连上过」的情况。
    _stateSubscription = gatewayService.stateStream.listen((state) {
      if (state == WsState.connected) gatewayService.listAutomations();
    });
    if (_catalog == null) gatewayService.listAutomations();
  }

  @override
  void dispose() {
    _catalogSubscription?.cancel();
    _stateSubscription?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AgentsView(
      catalog: _catalog,
      onRefresh: gatewayService.listAutomations,
      onSetEnabled: gatewayService.setAutomationEnabled,
      onRunOnce: gatewayService.runAutomationOnce,
      onOpenSession: widget.onOpenSession,
      startableAgentIds: widget.startableAgentIds,
      onStartConversation: widget.onStartConversation,
    );
  }
}

/// 纯展示层，方便单测直接喂目录。
class AgentsView extends StatefulWidget {
  const AgentsView({
    super.key,
    required this.catalog,
    required this.onRefresh,
    required this.onSetEnabled,
    required this.onRunOnce,
    required this.onOpenSession,
    this.startableAgentIds = const {},
    this.onStartConversation,
  });

  /// null = 还没拿到目录（daemon 订阅尚未同步）。空目录和「还没同步」必须分开
  /// 说，否则用户会以为自己的自动化没了。
  final AutomationCatalog? catalog;
  final VoidCallback onRefresh;
  final void Function(String automationId, bool enabled) onSetEnabled;
  final void Function(String automationId) onRunOnce;
  final void Function(String sessionId) onOpenSession;
  final Set<String> startableAgentIds;
  final void Function(String agentDefinitionId)? onStartConversation;

  @override
  State<AgentsView> createState() => _AgentsViewState();
}

class _AgentsViewState extends State<AgentsView> {
  AgentsSegment _segment = AgentsSegment.automations;

  @override
  Widget build(BuildContext context) {
    final catalog = widget.catalog;
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 16, 8),
          child: SegmentedButton<AgentsSegment>(
            segments: const [
              ButtonSegment(
                value: AgentsSegment.automations,
                icon: Icon(Icons.schedule, size: 18),
                label: Text('Automations'),
              ),
              ButtonSegment(
                value: AgentsSegment.agents,
                icon: Icon(Icons.smart_toy_outlined, size: 18),
                label: Text('Agents'),
              ),
            ],
            selected: {_segment},
            onSelectionChanged: (selection) =>
                setState(() => _segment = selection.first),
          ),
        ),
        Expanded(
          child: catalog == null
              ? const _AgentsPlaceholder(
                  icon: Icons.cloud_sync_outlined,
                  title: 'Syncing with your desktop',
                  detail: 'Automations appear once the daemon reports them.',
                )
              : RefreshIndicator(
                  onRefresh: () async => widget.onRefresh(),
                  child: switch (_segment) {
                    AgentsSegment.automations => _AutomationList(
                      automations: catalog.automations,
                      onSetEnabled: widget.onSetEnabled,
                      onRunOnce: widget.onRunOnce,
                      onOpenSession: widget.onOpenSession,
                    ),
                    AgentsSegment.agents => _AgentList(
                      agents: catalog.agents,
                      startableAgentIds: widget.startableAgentIds,
                      onStartConversation: widget.onStartConversation,
                    ),
                  },
                ),
        ),
      ],
    );
  }
}

class _AgentsPlaceholder extends StatelessWidget {
  const _AgentsPlaceholder({
    required this.icon,
    required this.title,
    required this.detail,
  });

  final IconData icon;
  final String title;
  final String detail;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.symmetric(horizontal: 32, vertical: 64),
      children: [
        Icon(icon, size: 40, color: theme.colorScheme.onSurfaceVariant),
        const SizedBox(height: 16),
        Text(
          title,
          textAlign: TextAlign.center,
          style: theme.textTheme.titleMedium,
        ),
        const SizedBox(height: 8),
        Text(
          detail,
          textAlign: TextAlign.center,
          style: theme.textTheme.bodySmall?.copyWith(
            color: theme.colorScheme.onSurfaceVariant,
          ),
        ),
      ],
    );
  }
}

class _AutomationList extends StatelessWidget {
  const _AutomationList({
    required this.automations,
    required this.onSetEnabled,
    required this.onRunOnce,
    required this.onOpenSession,
  });

  final List<AutomationSummary> automations;
  final void Function(String automationId, bool enabled) onSetEnabled;
  final void Function(String automationId) onRunOnce;
  final void Function(String sessionId) onOpenSession;

  @override
  Widget build(BuildContext context) {
    if (automations.isEmpty) {
      return const _AgentsPlaceholder(
        icon: Icons.schedule,
        title: 'No automations yet',
        detail:
            'Automations are created on the desktop. Once one exists you can '
            'pause it or run it from here.',
      );
    }
    return ListView.separated(
      padding: const EdgeInsets.only(bottom: 24),
      itemCount: automations.length,
      separatorBuilder: (_, _) => const Divider(height: 1),
      itemBuilder: (context, index) {
        final automation = automations[index];
        return _AutomationRow(
          automation: automation,
          onSetEnabled: (enabled) => onSetEnabled(automation.id, enabled),
          onTap: () => _showAutomationDetails(
            context,
            automation: automation,
            onRunOnce: () => onRunOnce(automation.id),
            onOpenSession: onOpenSession,
          ),
        );
      },
    );
  }
}

/// 目录的一行：名字、「什么时候 → 谁做」、上次结果。
///
/// 三行都在首屏，用户不用点进去就能判断「今天这条要不要停掉」。
class _AutomationRow extends StatelessWidget {
  const _AutomationRow({
    required this.automation,
    required this.onSetEnabled,
    required this.onTap,
  });

  final AutomationSummary automation;
  final void Function(bool enabled) onSetEnabled;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.colorScheme.onSurfaceVariant;
    return InkWell(
      onTap: onTap,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 8, 12),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    automation.name.isEmpty ? automation.id : automation.name,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.titleSmall?.copyWith(
                      fontWeight: FontWeight.w600,
                      // 停用的行整体压暗，一眼分得出「它现在不会动」。
                      color: automation.enabled
                          ? theme.colorScheme.onSurface
                          : muted,
                    ),
                  ),
                  const SizedBox(height: 2),
                  Text(
                    '${automationTriggerSummary(automation)} → '
                    '${automationExecutorLabel(automation)}',
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodySmall?.copyWith(color: muted),
                  ),
                  const SizedBox(height: 4),
                  _AutomationStatusLine(automation: automation),
                ],
              ),
            ),
            Switch(value: automation.enabled, onChanged: onSetEnabled),
          ],
        ),
      ),
    );
  }
}

/// 「上次结果 · 下次运行」。停用时不画下次运行——它不会到来。
class _AutomationStatusLine extends StatelessWidget {
  const _AutomationStatusLine({required this.automation});

  final AutomationSummary automation;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.colorScheme.onSurfaceVariant;
    final lastRun = automation.lastRun;
    final nextRunAt = automation.nextRunAt;
    final next = automation.enabled && nextRunAt != null
        ? formatUpcomingEpochSeconds(nextRunAt)
        : null;

    final parts = <Widget>[];
    if (lastRun != null) {
      final at = lastRun.finishedAt ?? lastRun.startedAt;
      final age = at == null ? null : formatRelativeEpochSeconds(at);
      parts
        ..add(
          Container(
            width: 7,
            height: 7,
            decoration: BoxDecoration(
              color: automationStatusColor(context, lastRun.status),
              shape: BoxShape.circle,
            ),
          ),
        )
        ..add(const SizedBox(width: 6))
        ..add(
          Text(
            age == null
                ? automationRunStatusLabel(lastRun.status)
                : '${automationRunStatusLabel(lastRun.status)} · $age',
            style: theme.textTheme.labelSmall?.copyWith(color: muted),
          ),
        );
    } else {
      parts.add(
        Text(
          'Never run',
          style: theme.textTheme.labelSmall?.copyWith(color: muted),
        ),
      );
    }
    if (next != null) {
      parts
        ..add(const SizedBox(width: 8))
        ..add(
          Text(
            'Next $next',
            style: theme.textTheme.labelSmall?.copyWith(color: muted),
          ),
        );
    }
    return Row(children: parts);
  }
}

/// 「谁做」。定义被删掉时说实话，不编一个名字——那正是用户要回桌面修的东西。
String automationExecutorLabel(AutomationSummary automation) {
  if (automation.actionKind == 'shell') return 'Shell command';
  return automation.agentName ?? 'Missing agent';
}

void _showAutomationDetails(
  BuildContext context, {
  required AutomationSummary automation,
  required VoidCallback onRunOnce,
  required void Function(String sessionId) onOpenSession,
}) {
  showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (sheetContext) => _AutomationDetailsSheet(
      automation: automation,
      onRunOnce: onRunOnce,
      onOpenSession: onOpenSession,
    ),
  );
}

class _AutomationDetailsSheet extends StatelessWidget {
  const _AutomationDetailsSheet({
    required this.automation,
    required this.onRunOnce,
    required this.onOpenSession,
  });

  final AutomationSummary automation;
  final VoidCallback onRunOnce;
  final void Function(String sessionId) onOpenSession;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.colorScheme.onSurfaceVariant;
    final lastRun = automation.lastRun;
    final nextRunAt = automation.nextRunAt;
    final sessionId = lastRun?.sessionId;

    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(20, 0, 20, 20),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              automation.name.isEmpty ? automation.id : automation.name,
              style: theme.textTheme.titleMedium,
            ),
            const SizedBox(height: 12),
            _DetailRow(
              label: 'When',
              value: automationTriggerSummary(automation),
            ),
            _DetailRow(
              label: 'Who',
              value: automationExecutorLabel(automation),
            ),
            if (automation.enabled && nextRunAt != null)
              _DetailRow(
                label: 'Next run',
                value: formatUpcomingEpochSeconds(nextRunAt) ?? '—',
              ),
            if (!automation.enabled)
              _DetailRow(label: 'Status', value: 'Paused'),
            if (lastRun != null)
              _DetailRow(
                label: 'Last run',
                value: [
                  automationRunStatusLabel(lastRun.status),
                  automationTriggerLabel(lastRun.source),
                  if (formatRelativeEpochSeconds(
                        lastRun.finishedAt ?? lastRun.startedAt ?? 0,
                      ) !=
                      null)
                    formatRelativeEpochSeconds(
                      lastRun.finishedAt ?? lastRun.startedAt ?? 0,
                    )!,
                ].join(' · '),
              ),
            if (lastRun?.error != null) ...[
              const SizedBox(height: 8),
              Text(
                lastRun!.error!,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: context.smeltColors.danger,
                ),
              ),
            ],
            const SizedBox(height: 20),
            Row(
              children: [
                Expanded(
                  child: FilledButton.tonalIcon(
                    onPressed: () {
                      Navigator.of(context).pop();
                      onRunOnce();
                      ScaffoldMessenger.of(context).showSnackBar(
                        const SnackBar(content: Text('Run requested')),
                      );
                    },
                    icon: const Icon(Icons.play_arrow, size: 18),
                    label: const Text('Run now'),
                  ),
                ),
                if (sessionId != null) ...[
                  const SizedBox(width: 12),
                  Expanded(
                    child: OutlinedButton.icon(
                      onPressed: () {
                        Navigator.of(context).pop();
                        onOpenSession(sessionId);
                      },
                      icon: const Icon(Icons.open_in_new, size: 18),
                      label: const Text('Open last run'),
                    ),
                  ),
                ],
              ],
            ),
            const SizedBox(height: 12),
            Text(
              'Editing automations stays on the desktop.',
              style: theme.textTheme.labelSmall?.copyWith(color: muted),
            ),
          ],
        ),
      ),
    );
  }
}

class _DetailRow extends StatelessWidget {
  const _DetailRow({required this.label, required this.value});

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 88,
            child: Text(
              label,
              style: theme.textTheme.labelMedium?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ),
          Expanded(child: Text(value, style: theme.textTheme.bodyMedium)),
        ],
      ),
    );
  }
}

class _AgentList extends StatelessWidget {
  const _AgentList({
    required this.agents,
    required this.startableAgentIds,
    required this.onStartConversation,
  });

  final List<AgentDefinitionSummary> agents;
  final Set<String> startableAgentIds;
  final void Function(String agentDefinitionId)? onStartConversation;

  @override
  Widget build(BuildContext context) {
    if (agents.isEmpty) {
      return const _AgentsPlaceholder(
        icon: Icons.smart_toy_outlined,
        title: 'No agents yet',
        detail: 'Agents are defined on the desktop.',
      );
    }
    return ListView.separated(
      padding: const EdgeInsets.only(bottom: 24),
      itemCount: agents.length,
      separatorBuilder: (_, _) => const Divider(height: 1),
      itemBuilder: (context, index) {
        final agent = agents[index];
        final start = _startCallback(agent);
        return ListTile(
          title: Text(agent.name.isEmpty ? agent.id : agent.name),
          subtitle: Text(
            _agentSubtitle(agent),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
          ),
          // 开对话是这一行的主动作，但不能抢走整行的点击：配置页是「它为什么
          // 这么干」的唯一入口，误触开一场对话比误触看一眼配置贵得多。
          trailing: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (start != null)
                IconButton(
                  icon: const Icon(Icons.chat_bubble_outline),
                  tooltip: 'Start conversation',
                  onPressed: start,
                ),
              const Icon(Icons.chevron_right),
            ],
          ),
          onTap: () => Navigator.of(context).push(
            MaterialPageRoute<void>(
              builder: (_) =>
                  AgentDefinitionPage(agent: agent, onStartConversation: start),
            ),
          ),
        );
      },
    );
  }

  VoidCallback? _startCallback(AgentDefinitionSummary agent) {
    final start = onStartConversation;
    if (start == null || !startableAgentIds.contains(agent.id)) return null;
    return () => start(agent.id);
  }
}

String _agentSubtitle(AgentDefinitionSummary agent) {
  final parts = <String>[
    if (agent.agentId.isNotEmpty) agent.agentId,
    if (agent.plugins.isNotEmpty) _count(agent.plugins.length, 'plugin'),
    if (agent.contextFolders.isNotEmpty)
      _count(agent.contextFolders.length, 'folder'),
  ];
  return parts.isEmpty ? 'No engine configured' : parts.join(' · ');
}

String _count(int value, String noun) =>
    value == 1 ? '1 $noun' : '$value ${noun}s';

/// 智能体的只读配置页。回答「它为什么这么干」：工作方式、插件、绑定的上下文。
///
/// 全文展示 prompt 而不是截断——排查时被截掉的那半句往往就是原因所在。
class AgentDefinitionPage extends StatelessWidget {
  const AgentDefinitionPage({
    super.key,
    required this.agent,
    this.onStartConversation,
  });

  final AgentDefinitionSummary agent;

  /// null = 这个智能体现在开不了对话（引擎未注册，或者还没拿到工作区目录）。
  final VoidCallback? onStartConversation;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.colorScheme.onSurfaceVariant;
    return Scaffold(
      appBar: AppBar(
        title: Text(agent.name.isEmpty ? agent.id : agent.name),
      ),
      body: ListView(
        padding: const EdgeInsets.fromLTRB(20, 16, 20, 32),
        children: [
          if (onStartConversation case final start?) ...[
            FilledButton.icon(
              onPressed: () {
                // 先退回列表再开：新会话会把整个 home 切到会话页，留着这一页
                // 只会让返回键把用户丢回一个跟当前对话无关的配置页。
                Navigator.of(context).pop();
                start();
              },
              icon: const Icon(Icons.chat_bubble_outline),
              label: const Text('Start conversation'),
            ),
            const SizedBox(height: 20),
          ],
          if (agent.description.isNotEmpty) ...[
            Text(agent.description, style: theme.textTheme.bodyMedium),
            const SizedBox(height: 20),
          ],
          _AgentSection(
            title: 'Engine',
            child: Text(
              agent.agentId.isEmpty ? 'Not configured' : agent.agentId,
              style: theme.textTheme.bodyMedium,
            ),
          ),
          if (agent.prompt.isNotEmpty)
            _AgentSection(
              title: 'Working style',
              child: SelectableText(
                agent.prompt,
                style: theme.textTheme.bodyMedium,
              ),
            ),
          if (agent.plugins.isNotEmpty)
            _AgentSection(
              title: 'Plugins',
              child: _AgentChips(values: agent.plugins),
            ),
          if (agent.contextFolders.isNotEmpty)
            _AgentSection(
              title: 'Folders',
              child: _AgentLines(values: agent.contextFolders),
            ),
          if (agent.contextLinks.isNotEmpty)
            _AgentSection(
              title: 'Links',
              child: _AgentLines(values: agent.contextLinks),
            ),
          const SizedBox(height: 8),
          Text(
            'Read-only on mobile. Edit this agent on the desktop.',
            style: theme.textTheme.labelSmall?.copyWith(color: muted),
          ),
        ],
      ),
    );
  }
}

class _AgentSection extends StatelessWidget {
  const _AgentSection({required this.title, required this.child});

  final String title;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 20),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            title.toUpperCase(),
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
              letterSpacing: 0.8,
            ),
          ),
          const SizedBox(height: 6),
          child,
        ],
      ),
    );
  }
}

class _AgentChips extends StatelessWidget {
  const _AgentChips({required this.values});

  final List<String> values;

  @override
  Widget build(BuildContext context) {
    return Wrap(
      spacing: 8,
      runSpacing: 4,
      children: [
        for (final value in values)
          Chip(
            label: Text(value),
            visualDensity: VisualDensity.compact,
            materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
          ),
      ],
    );
  }
}

class _AgentLines extends StatelessWidget {
  const _AgentLines({required this.values});

  final List<String> values;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        for (final value in values)
          Padding(
            padding: const EdgeInsets.only(bottom: 2),
            child: SelectableText(
              value,
              style: theme.textTheme.bodySmall?.copyWith(
                fontFamily: 'monospace',
              ),
            ),
          ),
      ],
    );
  }
}
