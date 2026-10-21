import 'dart:async';

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
import '../session/open_session.dart';
import 'launch_flow.dart';

/// 新建会话：底部弹层，分步选 机器 → 项目 → 启动方式。
///
/// [runtimeId] 给了且在线就从选项目开始（「会话」页筛到某台机器时）；只有一台在线时
/// 跳过选机器。建好后等会话进目录再打开；弹层在等待期间被划掉也照样打开——用户已经
/// 按下了「开」，划掉弹层不等于反悔。
Future<void> showNewSessionSheet(BuildContext context, {String? runtimeId}) {
  // 弹层压在根导航上，不在外壳的 DetailHost 下面：打开会话的去处和提示条在这里先取好。
  final model = AppScope.of(context);
  final host = DetailHost.of(context);
  final messenger = ScaffoldMessenger.of(context);
  return showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    useSafeArea: true,
    builder: (_) => NewSessionSheet(
      model: model,
      host: host,
      messenger: messenger,
      initialRuntimeId: runtimeId,
    ),
  );
}

enum _Step { machine, project, action, launching }

class NewSessionSheet extends StatefulWidget {
  const NewSessionSheet({
    super.key,
    required this.model,
    required this.host,
    required this.messenger,
    this.initialRuntimeId,
  });

  final AppModel model;
  final DetailHost host;
  final ScaffoldMessengerState messenger;
  final String? initialRuntimeId;

  @override
  State<NewSessionSheet> createState() => _NewSessionSheetState();
}

class _NewSessionSheetState extends State<NewSessionSheet> {
  _Step _step = _Step.machine;

  /// 选机器这一步是不是被跳过了（那样从选项目往回退就是关弹层）。
  bool _machineSkipped = false;
  String? _runtimeId;

  /// 选好的项目；[_agentSpace] 为真时表示「不选项目，跟智能体对话」。
  WorkspaceProject? _project;
  bool _agentSpace = false;

  WorkspaceCatalog? _workspace;
  String? _workspaceError;
  int _workspaceGeneration = 0;

  LaunchIntent? _intent;
  bool _waitingForList = false;
  String? _launchError;

  @override
  void initState() {
    super.initState();
    final online = widget.model.fleet.value.runtimes.where((r) => r.online);
    final wanted = widget.initialRuntimeId == null
        ? null
        : widget.model.fleet.runtime(widget.initialRuntimeId!);
    if (wanted != null && wanted.online) {
      _machineSkipped = true;
      _chooseMachine(wanted.id, animate: false);
    } else if (wanted == null && online.length == 1) {
      _machineSkipped = true;
      _chooseMachine(online.single.id, animate: false);
    }
  }

  // 步骤 ---------------------------------------------------------------------

  void _chooseMachine(String runtimeId, {bool animate = true}) {
    void apply() {
      _runtimeId = runtimeId;
      _project = null;
      _agentSpace = false;
      _workspace = null;
      _workspaceError = null;
      _step = _Step.project;
    }

    animate ? setState(apply) : apply();
    unawaited(_loadWorkspace(runtimeId));
  }

  Future<void> _loadWorkspace(String runtimeId) async {
    final generation = ++_workspaceGeneration;
    try {
      final catalog = await widget.model.backend.workspaceCatalog(runtimeId);
      if (!mounted || generation != _workspaceGeneration) return;
      setState(() => _workspace = catalog);
    } catch (error) {
      if (!mounted || generation != _workspaceGeneration) return;
      setState(() => _workspaceError = describeError(error));
    }
  }

  void _retryWorkspace() {
    final id = _runtimeId;
    if (id == null) return;
    setState(() => _workspaceError = null);
    unawaited(_loadWorkspace(id));
  }

  void _chooseProject(WorkspaceProject? project) => setState(() {
    _project = project;
    _agentSpace = project == null;
    _step = _Step.action;
  });

  bool get _canGoBack => switch (_step) {
    _Step.machine => false,
    _Step.project => !_machineSkipped,
    _Step.action => true,
    // 发送中不往回退（结果回来时会落在一个已经不存在的步骤上）；失败了可以回去换一个。
    _Step.launching => _launchError != null,
  };

  void _back() {
    if (!_canGoBack) return;
    setState(() {
      _step = switch (_step) {
        _Step.launching => _Step.action,
        _Step.action => _Step.project,
        _ => _Step.machine,
      };
      _launchError = null;
    });
  }

  // 新建 ---------------------------------------------------------------------

  void _startAction(LaunchAction action) => _launch(
    LaunchIntent(
      runtimeId: _runtimeId!,
      projectRoot: _project?.root ?? '',
      launchKey: action.key,
      label: action.label,
      requestId: widget.model.backend.newRequestId(),
    ),
  );

  void _startAgent(AcpAgentOption agent) => _launch(
    LaunchIntent(
      runtimeId: _runtimeId!,
      projectRoot: _project?.root ?? '',
      agentOptionId: agent.id,
      label: agent.label,
      requestId: widget.model.backend.newRequestId(),
    ),
  );

  Future<void> _launch(LaunchIntent intent) async {
    setState(() {
      _intent = intent;
      _step = _Step.launching;
      _launchError = null;
      _waitingForList = false;
    });
    final model = widget.model;
    final l10n = L10n.of(context);
    final String sessionId;
    try {
      sessionId = await intent.send(model.backend);
    } catch (error) {
      if (mounted) {
        setState(() => _launchError = describeError(error));
      } else {
        widget.messenger.showSnackBar(
          SnackBar(content: Text(l10n.launchFailed(describeError(error)))),
        );
      }
      return;
    }
    if (mounted) setState(() => _waitingForList = true);
    final session = await waitForSession(
      model.catalog,
      intent.runtimeId,
      sessionId,
    );
    if (mounted) Navigator.of(context).pop();
    if (session == null) {
      final machine = model.fleet.runtime(intent.runtimeId)?.name ?? '';
      widget.messenger.showSnackBar(
        SnackBar(content: Text(l10n.launchNotListed(machine))),
      );
      return;
    }
    openSessionWith(widget.host, intent.runtimeId, session);
  }

  // 界面 ---------------------------------------------------------------------

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final maxHeight = MediaQuery.sizeOf(context).height * 0.85;
    final body = switch (_step) {
      _Step.machine => _machineStep(context),
      _Step.project => _projectStep(context),
      _Step.action => _actionStep(context),
      _Step.launching => _launchingStep(context),
    };
    return PopScope(
      // 系统返回先退一步，退到第一步再关弹层。
      canPop: !_canGoBack,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) _back();
      },
      child: ConstrainedBox(
        constraints: BoxConstraints(maxHeight: maxHeight),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _Header(
              title: l10n.launchTitle,
              runtime: _runtimeId == null || _step == _Step.machine
                  ? null
                  : widget.model.fleet.runtime(_runtimeId!),
              place: switch (_step) {
                _Step.action || _Step.launching =>
                  _agentSpace ? l10n.launchNoProject : _project?.title,
                _ => null,
              },
              onBack: _canGoBack ? _back : null,
            ),
            Flexible(
              child: AnimatedSwitcher(
                duration: Motion.resolve(context, Motion.standard),
                switchInCurve: Motion.easeOut,
                switchOutCurve: Motion.easeOut,
                // 只摆新一步、让它淡入：默认布局把淡出的上一步叠在底下，两份列表会在
                // 转场里重影。上一步直接让位，弹层高度一次到位。
                layoutBuilder: (current, _) =>
                    current ?? const SizedBox.shrink(),
                child: KeyedSubtree(key: ValueKey(_step), child: body),
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _scroll(List<Widget> children) => ListView(
    shrinkWrap: true,
    padding: EdgeInsets.only(
      bottom: MediaQuery.paddingOf(context).bottom + Space.lg,
    ),
    children: children,
  );

  Widget _machineStep(BuildContext context) {
    final l10n = L10n.of(context);
    final c = GalleyColors.of(context);
    return ListenableBuilder(
      listenable: widget.model.fleet,
      builder: (context, _) {
        final runtimes = widget.model.fleet.value.runtimes;
        final anyOnline = runtimes.any((r) => r.online);
        return _scroll([
          ShelfHeader(title: l10n.launchStepMachine, first: true),
          if (!anyOnline)
            Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.page + Space.xs,
                0,
                Space.page + Space.xs,
                Space.md,
              ),
              child: Text(
                l10n.launchNoMachineOnline,
                style: GalleyText.of(context).note.copyWith(color: c.inkMuted),
              ),
            ),
          _RowList(
            children: [
              for (final runtime in runtimes)
                _MachineRow(
                  runtime: runtime,
                  onTap: runtime.online
                      ? () => _chooseMachine(runtime.id)
                      : null,
                ),
            ],
          ),
        ]);
      },
    );
  }

  Widget _projectStep(BuildContext context) {
    final l10n = L10n.of(context);
    final workspace = _workspace;
    if (_workspaceError != null) {
      return _scroll([
        EmptyNote(
          icon: LucideIcons.cloudOff,
          title: l10n.launchLoadFailed,
          body: _workspaceError,
          action: GalleyButton.solid(
            label: l10n.commonRetry,
            onPressed: _retryWorkspace,
          ),
        ),
      ]);
    }
    if (workspace == null) return const _Busy();
    final agents = workspace.agents
        .where((a) => a.agentDefinitionId != null)
        .toList();
    if (workspace.projects.isEmpty && agents.isEmpty) {
      return _scroll([
        EmptyNote(icon: LucideIcons.folderX, title: l10n.launchNoProjects),
      ]);
    }
    final c = GalleyColors.of(context);
    return _scroll([
      ShelfHeader(title: l10n.launchStepProject, first: true),
      _RowList(
        children: [
          // 不选项目（跟智能体对话）放最前：它不属于下面任何一个项目。
          if (agents.isNotEmpty)
            ShelfRow(
              leading: Icon(
                LucideIcons.bot,
                size: Strokes.icon,
                color: c.inkMuted,
              ),
              title: Text(l10n.launchNoProject),
              subtitle: Text(l10n.launchNoProjectHint),
              trailing: _chevron(c),
              onTap: () => _chooseProject(null),
            ),
          for (final project in workspace.projects)
            ShelfRow(
              leading: Icon(
                LucideIcons.folder,
                size: Strokes.icon,
                color: c.inkMuted,
              ),
              title: Text(project.title),
              subtitle: Text(project.root),
              trailing: _chevron(c),
              onTap: () => _chooseProject(project),
            ),
        ],
      ),
    ]);
  }

  Widget _actionStep(BuildContext context) {
    final l10n = L10n.of(context);
    final c = GalleyColors.of(context);
    final workspace = _workspace ?? emptyWorkspaceCatalog;
    final agents = workspace.agents
        .where((a) => a.agentDefinitionId != null)
        .toList();
    final sections = _agentSpace
        ? const <(String, List<LaunchAction>)>[]
        : [
            (l10n.launchSectionPinned, workspace.sections.common),
            (l10n.launchSectionConversation, workspace.sections.conversation),
            (l10n.launchSectionTerminal, workspace.sections.terminal),
          ];
    final children = <Widget>[];
    for (final (title, actions) in sections) {
      if (actions.isEmpty) continue;
      children
        ..add(ShelfHeader(title: title, first: children.isEmpty))
        ..add(
          _RowList(
            children: [
              for (final action in actions)
                ShelfRow(
                  leading: _actionGlyph(action, c),
                  title: Text(action.label),
                  subtitle: Text(
                    action.target == LaunchTarget.conversation
                        ? l10n.kindConversation
                        : l10n.kindTerminal,
                  ),
                  onTap: () => _startAction(action),
                ),
            ],
          ),
        );
    }
    if (agents.isNotEmpty) {
      children
        ..add(
          ShelfHeader(title: l10n.launchSectionAgents, first: children.isEmpty),
        )
        ..add(
          _RowList(
            children: [
              for (final agent in agents)
                ShelfRow(
                  leading: AgentGlyph(
                    agent: agent.kind,
                    color: c.ink,
                    size: Strokes.icon,
                  ),
                  title: Text(agent.label),
                  subtitle: Text(
                    l10n.launchAgentSubtitle(agentDisplayName(agent.kind)),
                  ),
                  onTap: () => _startAgent(agent),
                ),
            ],
          ),
        );
    }
    if (children.isEmpty) {
      return _scroll([
        EmptyNote(icon: LucideIcons.squareDashed, title: l10n.launchNoActions),
      ]);
    }
    return _scroll(children);
  }

  Widget _actionGlyph(LaunchAction action, GalleyColors c) {
    if (action.target == LaunchTarget.blankTerminal) {
      return Icon(LucideIcons.squareTerminal, size: Strokes.icon, color: c.ink);
    }
    // 认不出是哪家时：终端动作画终端、对话动作画机器人，别让终端看起来像对话。
    return AgentGlyph(
      agent: action.agent,
      terminal: action.target != LaunchTarget.conversation,
      color: c.ink,
      size: Strokes.icon,
    );
  }

  Widget _launchingStep(BuildContext context) {
    final l10n = L10n.of(context);
    final intent = _intent;
    if (intent == null) return const SizedBox.shrink();
    final error = _launchError;
    if (error != null) {
      return _scroll([
        EmptyNote(
          icon: LucideIcons.circleX,
          title: l10n.launchFailedTitle(intent.label),
          body: error,
          action: Wrap(
            spacing: Space.sm,
            runSpacing: Space.sm,
            alignment: WrapAlignment.center,
            children: [
              GalleyButton.quiet(label: l10n.launchBack, onPressed: _back),
              // 同一个 requestId 再发一次：上次其实成功了的话，运行时会落到同一个会话。
              GalleyButton.solid(
                label: l10n.commonRetry,
                onPressed: () => _launch(intent),
              ),
            ],
          ),
        ),
      ]);
    }
    return _Busy(
      label: _waitingForList
          ? l10n.launchWaiting
          : l10n.launchStarting(intent.label),
    );
  }

  Widget _chevron(GalleyColors c) => Icon(
    LucideIcons.chevronRight,
    size: Strokes.iconSmall,
    color: c.inkFaint,
  );
}

/// 弹层头：返回、标题，下面一行写已经选了什么（机器 · 项目）。
class _Header extends StatelessWidget {
  const _Header({
    required this.title,
    required this.runtime,
    required this.place,
    required this.onBack,
  });

  final String title;
  final FleetRuntime? runtime;
  final String? place;
  final VoidCallback? onBack;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final r = runtime;
    return Padding(
      padding: const EdgeInsets.fromLTRB(Space.xs, 0, Space.page, Space.sm),
      child: Row(
        children: [
          if (onBack != null)
            IconButton(
              tooltip: l10n.launchBack,
              icon: const Icon(LucideIcons.chevronLeft),
              onPressed: onBack,
            )
          else
            const SizedBox(width: Space.md),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisSize: MainAxisSize.min,
              children: [
                Semantics(
                  header: true,
                  child: Text(title, style: t.title.copyWith(color: c.ink)),
                ),
                if (r != null)
                  Row(
                    children: [
                      Flexible(
                        child: StationLabel(
                          name: r.name,
                          color: c.station(r.record.accent),
                        ),
                      ),
                      if (place != null)
                        Flexible(
                          child: Text(
                            ' · $place',
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: t.meta.copyWith(color: c.inkMuted),
                          ),
                        ),
                    ],
                  ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _MachineRow extends StatelessWidget {
  const _MachineRow({required this.runtime, required this.onTap});

  final FleetRuntime runtime;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final enabled = onTap != null;
    final status = linkLabel(l10n, runtime.status);
    return ShelfRow(
      leading: StationTape(color: c.station(runtime.record.accent), height: 28),
      title: Text(
        runtime.name,
        style: enabled ? null : t.headline.copyWith(color: c.inkMuted),
      ),
      subtitle: Text(
        enabled ? status : l10n.launchMachineUnavailable(status),
        maxLines: 2,
      ),
      trailing: enabled
          ? Icon(
              LucideIcons.chevronRight,
              size: Strokes.iconSmall,
              color: c.inkFaint,
            )
          : null,
      onTap: onTap,
    );
  }
}

/// 弹层里的一列行：行间发丝线，不再垫一层搁板底（弹层本身就是那块板）。
class _RowList extends StatelessWidget {
  const _RowList({required this.children});

  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        for (final (index, child) in children.indexed) ...[
          if (index > 0)
            Padding(
              padding: const EdgeInsetsDirectional.only(start: Space.lg),
              child: Divider(height: Strokes.hairline, color: c.hairline),
            ),
          child,
        ],
      ],
    );
  }
}

class _Busy extends StatelessWidget {
  const _Busy({this.label});

  final String? label;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Padding(
      padding: EdgeInsets.fromLTRB(
        Space.page,
        Space.xxl,
        Space.page,
        MediaQuery.paddingOf(context).bottom + Space.xxl,
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          const CircularProgressIndicator.adaptive(),
          const SizedBox(height: Space.md),
          Semantics(
            liveRegion: true,
            child: Text(
              label ?? L10n.of(context).commonLoading,
              textAlign: TextAlign.center,
              style: t.note.copyWith(color: c.inkMuted),
            ),
          ),
        ],
      ),
    );
  }
}
