import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/adaptive.dart';
import '../../ui/agent_glyph.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../../ui/shelf.dart';
import '../session/open_session.dart';
import 'agent_edit_page.dart';
import 'agent_memory_page.dart';
import 'agents_board.dart';
import 'agents_widgets.dart';

/// 智能体详情：回答「它为什么这么干」——长期指令、模型、插件、MCP、绑定的上下文，和它记下
/// 的东西；右上角编辑，底部「开始对话」在这台机器上开一场不绑项目的对话（落在智能体自己的
/// space）。
///
/// 长期指令全文展示不截断：排查时被截掉的那半句往往就是原因所在。
class AgentDetailPage extends StatefulWidget {
  const AgentDetailPage({
    super.key,
    required this.board,
    required this.runtimeId,
    required this.agentId,
  });

  final AgentsBoard board;
  final String runtimeId;
  final String agentId;

  @override
  State<AgentDetailPage> createState() => _AgentDetailPageState();
}

/// 新建会话后等它出现在会话目录里最多这么久；超时就提示去「会话」里找，不无限转圈。
const _sessionAppearTimeout = Duration(seconds: 15);

class _AgentDetailPageState extends State<AgentDetailPage> {
  bool _starting = false;

  /// 记忆没有推送：进页面取一次，从记忆页、编辑页回来再取。null = 还没取到或取不到。
  AgentMemories? _memories;

  /// 这台机器能力库里的模型、插件、MCP，用来把定义里的 id 显示成和编辑页一样的名字。
  /// 进页面、编辑回来时取；取到之前（或取不到）显示 id 原文。
  AgentEditor? _choices;

  @override
  void initState() {
    super.initState();
    unawaited(_loadMemories());
    unawaited(_loadChoices());
  }

  Future<void> _loadChoices() async {
    try {
      final choices = await AppScope.of(
        context,
      ).backend.agentEditor(widget.runtimeId, agentId: widget.agentId);
      if (mounted) setState(() => _choices = choices);
    } catch (_) {
      // 取不到就显示 id 原文，不挡详情本身。
    }
  }

  Future<void> _loadMemories() async {
    try {
      final memories = await AppScope.of(
        context,
      ).backend.agentMemories(widget.runtimeId, widget.agentId);
      if (mounted) setState(() => _memories = memories);
    } catch (_) {
      // 取不到就不显示条数；进记忆页会再取一次并说明原因。
    }
  }

  Future<void> _edit() async {
    final navigator = Navigator.of(context);
    final result = await navigator.push<AgentEditResult>(
      MaterialPageRoute(
        builder: (_) => AgentEditPage(
          board: widget.board,
          runtimeId: widget.runtimeId,
          agentId: widget.agentId,
        ),
      ),
    );
    if (!mounted) return;
    if (result == AgentEditResult.deleted) {
      navigator.pop();
      return;
    }
    unawaited(_loadMemories());
    unawaited(_loadChoices());
  }

  Future<void> _openMemories(AgentDefinitionSummary agent) async {
    await Navigator.of(context).push<void>(
      MaterialPageRoute(
        builder: (_) => AgentMemoryPage(
          runtimeId: widget.runtimeId,
          agentId: agent.id,
          agentName: agent.name,
          initial: _memories,
        ),
      ),
    );
    if (mounted) unawaited(_loadMemories());
  }

  /// 这次「开始对话」意图的请求 id。失败重试沿用同一个，运行时据此落到同一个会话，
  /// 不会因为第一次其实已经建好而多开一场。成功后清掉，下一次是新意图。
  String? _requestId;

  Future<void> _start(String optionId) async {
    final model = AppScope.of(context);
    final host = DetailHost.of(context);
    final messenger = ScaffoldMessenger.of(context);
    final l10n = L10n.of(context);
    final requestId = _requestId ??= model.backend.newRequestId();
    setState(() => _starting = true);
    try {
      final sessionId = await model.backend.createSession(
        widget.runtimeId,
        projectRoot: '',
        agentOptionId: optionId,
        requestId: requestId,
      );
      _requestId = null;
      final session = await waitForSession(
        model.catalog,
        widget.runtimeId,
        sessionId,
        timeout: _sessionAppearTimeout,
      );
      if (session == null) {
        messenger.showSnackBar(
          SnackBar(content: Text(l10n.agentsStartPending)),
        );
      } else {
        openSessionWith(host, widget.runtimeId, session);
      }
    } catch (error) {
      messenger.showSnackBar(
        SnackBar(content: Text(l10n.agentsStartFailed(describeError(error)))),
      );
    } finally {
      if (mounted) setState(() => _starting = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final model = AppScope.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([widget.board, model.fleet]),
      builder: (context, _) {
        final runtime = model.fleet.runtime(widget.runtimeId);
        final agent = widget.board.agent(widget.runtimeId, widget.agentId);
        final c = GalleyColors.of(context);
        final l10n = L10n.of(context);
        if (agent == null) {
          return Scaffold(
            backgroundColor: c.ground,
            appBar: AppBar(),
            body: Center(
              child: EmptyNote(
                icon: LucideIcons.circleSlash,
                title: l10n.agentsGone(runtime?.name ?? ''),
              ),
            ),
          );
        }
        final online = runtime?.online ?? false;
        final options = widget.board.machine(widget.runtimeId).startOptions;
        final optionId = options?[agent.id];
        return Scaffold(
          backgroundColor: c.ground,
          appBar: AppBar(
            backgroundColor: c.ground,
            title: DetailTitle(title: agent.name, runtime: runtime),
            actions: [
              IconButton(
                tooltip: l10n.agentsEdit,
                icon: const Icon(LucideIcons.pencil),
                onPressed: online ? _edit : null,
              ),
            ],
          ),
          body: _AgentBody(
            agent: agent,
            runtime: runtime,
            online: online,
            memories: _memories,
            choices: _choices,
            onMemories: online ? () => _openMemories(agent) : null,
          ),
          bottomNavigationBar: switch ((options, optionId)) {
            (_, final id?) => BottomActionBar(
              child: GalleyButton.solid(
                expand: true,
                icon: LucideIcons.messageSquarePlus,
                label: _starting
                    ? l10n.agentsStarting
                    : l10n.agentsStartConversation,
                onPressed: online && !_starting ? () => _start(id) : null,
              ),
            ),
            // 工作区目录已取到、里面没有它：在这台机器上开不了，说清楚原因，不给一个
            // 按下去只会报错的按钮。
            (_?, null) => BottomActionBar(
              child: Text(
                l10n.agentsCannotStart,
                style: GalleyText.of(context).meta.copyWith(color: c.inkMuted),
              ),
            ),
            // 还不知道能不能开（工作区目录没取到）：先不出按钮。
            (null, null) => null,
          },
        );
      },
    );
  }
}

class _AgentBody extends StatelessWidget {
  const _AgentBody({
    required this.agent,
    required this.runtime,
    required this.online,
    required this.memories,
    required this.choices,
    required this.onMemories,
  });

  final AgentDefinitionSummary agent;
  final FleetRuntime? runtime;
  final bool online;
  final AgentMemories? memories;
  final AgentEditor? choices;
  final VoidCallback? onMemories;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final prompt = agent.prompt.trim();
    final description = agent.description.trim();
    final hasContext =
        agent.contextFolders.isNotEmpty || agent.contextLinks.isNotEmpty;
    return ListView(
      padding: const EdgeInsets.only(bottom: Space.xxl),
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(
            Space.page + Space.xs,
            Space.sm,
            Space.page + Space.xs,
            0,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (description.isNotEmpty) ...[
                Text(description, style: t.body.copyWith(color: c.ink)),
                const SizedBox(height: Space.sm),
              ],
              Row(
                children: [
                  AgentGlyph(
                    agent: agent.engineKindId,
                    color: c.inkMuted,
                    size: Strokes.iconSmall,
                  ),
                  const SizedBox(width: Space.sm),
                  Flexible(
                    child: Text(
                      l10n.agentsEngine(agentDisplayName(agent.engineKindId)),
                      style: t.meta.copyWith(color: c.inkMuted),
                    ),
                  ),
                ],
              ),
              if (!online) ...[
                const SizedBox(height: Space.sm),
                Text(
                  l10n.agentsMachineOffline(runtime?.name ?? ''),
                  style: t.meta.copyWith(color: c.inkMuted),
                ),
              ],
            ],
          ),
        ),
        ShelfHeader(title: l10n.agentsPrompt),
        Shelf(
          children: [
            Padding(
              padding: const EdgeInsets.all(Space.lg),
              child: prompt.isEmpty
                  ? Text(
                      l10n.agentsNoPrompt,
                      style: t.body.copyWith(color: c.inkMuted),
                    )
                  : SelectableText(
                      agent.prompt,
                      style: t.body.copyWith(color: c.ink),
                    ),
            ),
          ],
        ),
        ShelfHeader(title: l10n.agentEditModel),
        Shelf(
          children: [
            _Line(
              icon: LucideIcons.cpu,
              text: agentModelLabel(
                l10n,
                choices?.models,
                agent.modelProvider,
                agent.modelId,
              ),
            ),
          ],
        ),
        if (agent.plugins.isNotEmpty) ...[
          ShelfHeader(title: l10n.agentsPlugins),
          Shelf(
            children: [for (final id in agent.plugins) _pluginRow(l10n, id)],
          ),
        ],
        if (agent.mcpServers.isNotEmpty) ...[
          ShelfHeader(title: l10n.agentEditMcp),
          Shelf(
            children: [
              for (final name in agent.mcpServers) _mcpRow(l10n, name),
            ],
          ),
        ],
        if (hasContext) ...[
          ShelfHeader(title: l10n.agentsContext),
          Shelf(
            children: [
              for (final folder in agent.contextFolders)
                _Line(icon: LucideIcons.folder, text: folder),
              for (final link in agent.contextLinks)
                _Line(
                  icon: LucideIcons.link,
                  text: link,
                  onTap: _openable(link),
                ),
            ],
          ),
        ],
        ShelfHeader(title: l10n.agentEditMemory),
        Shelf(
          children: [
            ShelfRow(
              leading: Icon(
                LucideIcons.brain,
                size: Strokes.icon,
                color: c.inkMuted,
              ),
              title: Text(l10n.agentsMemories),
              subtitle: Text(_memorySummary(l10n)),
              trailing: Icon(
                LucideIcons.chevronRight,
                size: Strokes.iconSmall,
                color: c.inkFaint,
              ),
              onTap: onMemories,
            ),
          ],
        ),
      ],
    );
  }

  /// 和编辑页一样：类型图标、名字、说明；坏了的写原因，机器上没有了的照实说。
  Widget _pluginRow(L10n l10n, String id) {
    final loaded = choices;
    final plugin = loaded?.plugins.where((p) => p.id == id).firstOrNull;
    if (plugin == null) {
      return _ChoiceLine(
        icon: loaded == null ? LucideIcons.plug : LucideIcons.circleSlash,
        title: id,
        subtitle: loaded == null ? null : l10n.agentEditMissing,
        danger: loaded != null,
      );
    }
    final description = plugin.description.trim();
    return _ChoiceLine(
      icon: pluginIcon(plugin.kind),
      title: plugin.name,
      subtitle: plugin.broken ?? (description.isEmpty ? null : description),
      danger: plugin.broken != null,
    );
  }

  Widget _mcpRow(L10n l10n, String name) {
    final loaded = choices;
    final server = loaded?.mcpServers.where((s) => s.name == name).firstOrNull;
    if (loaded != null && server == null) {
      return _ChoiceLine(
        icon: LucideIcons.circleSlash,
        title: name,
        subtitle: l10n.agentEditMissing,
        danger: true,
      );
    }
    final description = server?.description?.trim() ?? '';
    return _ChoiceLine(
      icon: LucideIcons.server,
      title: name,
      subtitle: server != null && !server.enabled
          ? l10n.agentEditMcpDisabled
          : (description.isEmpty ? null : description),
    );
  }

  String _memorySummary(L10n l10n) {
    final m = memories;
    final parts = [
      if (!agent.memoryEnabled) l10n.agentsMemoryOff,
      if (m != null && m.pending > 0) l10n.agentsMemoryPending(m.pending),
      if (m != null && m.pending == 0)
        l10n.agentsMemoryCount(m.agent.items.length + m.user.items.length),
    ];
    return parts.isEmpty ? l10n.commonLoading : parts.join(' · ');
  }

  /// 只有 http(s) 链接能点开；别的（内部 id、相对路径）原样展示。
  VoidCallback? _openable(String link) {
    final uri = Uri.tryParse(link.trim());
    if (uri == null || !(uri.isScheme('http') || uri.isScheme('https'))) {
      return null;
    }
    return () =>
        unawaited(launchUrl(uri, mode: LaunchMode.externalApplication));
  }
}

/// 插件、MCP 的一行：和编辑页同样的图标、名字和说明，只是不能勾。
class _ChoiceLine extends StatelessWidget {
  const _ChoiceLine({
    required this.icon,
    required this.title,
    this.subtitle,
    this.danger = false,
  });

  final IconData icon;
  final String title;
  final String? subtitle;
  final bool danger;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final sub = subtitle;
    return ShelfRow(
      leading: Icon(icon, size: Strokes.icon, color: c.inkMuted),
      title: Text(title),
      subtitle: sub == null
          ? null
          : Text(
              sub,
              maxLines: 2,
              style: danger ? t.meta.copyWith(color: c.danger) : null,
            ),
    );
  }
}

/// 一项配置：图标 + 全文。路径和链接可能很长，换行展示，不截断。
class _Line extends StatelessWidget {
  const _Line({required this.icon, required this.text, this.onTap});

  final IconData icon;
  final String text;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final row = ConstrainedBox(
      constraints: const BoxConstraints(minHeight: Space.touch),
      child: Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Space.lg,
          vertical: Space.md,
        ),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(icon, size: Strokes.iconSmall, color: c.inkMuted),
            const SizedBox(width: Space.md),
            Expanded(
              child: Text(
                text,
                style: t.body.copyWith(color: onTap == null ? c.ink : c.link),
              ),
            ),
          ],
        ),
      ),
    );
    if (onTap == null) return row;
    return Semantics(
      link: true,
      child: InkWell(onTap: onTap, child: row),
    );
  }
}

/// 等新建的会话出现在会话目录里。已经在就立刻返回；超时返回 null。
Future<SessionSummary?> waitForSession(
  CatalogStore catalog,
  String runtimeId,
  String sessionId, {
  required Duration timeout,
}) {
  final found = catalog.session(runtimeId, sessionId);
  if (found != null) return Future.value(found);
  final completer = Completer<SessionSummary?>();
  Timer? timer;
  late final VoidCallback listener;
  void finish(SessionSummary? session) {
    if (completer.isCompleted) return;
    catalog.removeListener(listener);
    timer?.cancel();
    completer.complete(session);
  }

  listener = () {
    final session = catalog.session(runtimeId, sessionId);
    if (session != null) finish(session);
  };
  catalog.addListener(listener);
  timer = Timer(timeout, () => finish(null));
  return completer.future;
}
