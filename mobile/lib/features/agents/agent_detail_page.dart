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
import 'agents_board.dart';
import 'agents_widgets.dart';

/// 智能体定义的只读页：回答「它为什么这么干」——长期指令、插件、绑定的上下文；
/// 底部「开始对话」在这台机器上开一场不绑项目的对话（落在智能体自己的 space）。
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
          ),
          body: _AgentBody(agent: agent, runtime: runtime, online: online),
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
  });

  final AgentDefinitionSummary agent;
  final FleetRuntime? runtime;
  final bool online;

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
        if (agent.plugins.isNotEmpty) ...[
          ShelfHeader(title: l10n.agentsPlugins),
          Shelf(
            children: [
              for (final plugin in agent.plugins)
                _Line(icon: LucideIcons.plug, text: plugin),
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
    );
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
