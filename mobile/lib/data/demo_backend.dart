/// 演示后端：合成数据，不加载 Rust 动态库。
///
/// 两个用处：单测注入；`--dart-define=SMELT_DEMO=true` 启动后在模拟器里截图审查视觉。
/// 数据是**合成的**（机器名、项目、命令都是编的），只用来看界面长什么样。
///
/// 动作会改状态：批掉一张单据它就从轨上离开，能看到盖章和离轨动画。对话与终端的演示
/// 数据在 `demo/` 下各自的文件里。
library;

import 'dart:async';
import 'dart:typed_data';

import 'backend.dart';
import 'demo/demo_conversation.dart';
import 'demo/demo_terminal.dart';
import 'models.dart';

class DemoBackend implements Backend {
  DemoBackend({DateTime? now}) : _now = now ?? DateTime.now() {
    _seed();
  }

  final DateTime _now;
  final _fleet = StreamController<FleetView>.broadcast();
  final _pass = StreamController<Pass>.broadcast();
  final _catalog = StreamController<CatalogEvent>.broadcast();

  late List<FleetRuntime> _runtimes;
  late List<PassTicket> _tickets;
  late Map<String, List<SessionSummary>> _sessions;
  final Map<String, MessageDraft> _drafts = {};

  int _ago(Duration d) => _now.subtract(d).millisecondsSinceEpoch ~/ 1000;

  SessionSummary _s(
    String id,
    String title, {
    required String agent,
    required String project,
    SessionPhase phase = SessionPhase.idle,
    SessionStatus status = SessionStatus.idle,
    Duration ago = const Duration(hours: 3),
    SessionKind kind = SessionKind.acp,
    String? detail,
    AttentionItem? attention,
  }) => SessionSummary(
    id: id,
    kind: kind,
    title: title,
    phase: phase,
    status: status,
    agent: agent,
    cwd: '/Users/demo/code/$project',
    projectRoot: '/Users/demo/code/$project',
    projectTitle: project,
    projectOrder: 0,
    updatedAt: _ago(ago),
    detail: detail,
    attention: attention,
  );

  void _seed() {
    _runtimes = [
      _runtime(
        'rt-mbp',
        'MacBook Pro',
        0,
        'macos',
        ConnectionState.connected,
        LinkPath.lan,
        12,
      ),
      _runtime(
        'rt-mini',
        'home-mini',
        1,
        'macos',
        ConnectionState.connected,
        LinkPath.relay,
        84,
      ),
      _runtime(
        'rt-build',
        'build-01',
        2,
        'linux',
        ConnectionState.reconnecting,
        null,
        null,
      ),
    ];

    final approval = _s(
      's-md',
      '修复 markdown 链接解析',
      agent: 'claude',
      project: 'smelt',
      phase: SessionPhase.awaitingApproval,
      status: SessionStatus.needsYou,
      ago: const Duration(seconds: 42),
      detail: 'Bash',
      attention: const AttentionItem(
        sessionId: 's-md',
        title: '等你批准',
        message: '想运行 cargo test',
        kind: AttentionKind.approval,
      ),
    );
    final question = _s(
      's-route',
      '接入 sing-box 路由',
      agent: 'codex',
      project: 'velum',
      phase: SessionPhase.waitingForUser,
      status: SessionStatus.needsYou,
      ago: const Duration(minutes: 3, seconds: 10),
      attention: const AttentionItem(
        sessionId: 's-route',
        title: '等你输入',
        message: '路由规则放在哪一层？',
        kind: AttentionKind.input,
      ),
    );
    final failed = _s(
      's-relay',
      '部署自建 relay',
      agent: 'claude',
      project: 'infra',
      phase: SessionPhase.failed,
      status: SessionStatus.needsYou,
      ago: const Duration(minutes: 5),
      attention: const AttentionItem(
        sessionId: 's-relay',
        title: '失败',
        message: 'systemctl restart iroh-relay 退出码 1：端口 443 已被占用',
        kind: AttentionKind.failure,
      ),
    );

    _tickets = [
      PassTicket(
        runtimeId: 'rt-mbp',
        session: approval,
        detailsLoaded: true,
        recent: demoRecentContext('s-md', _now),
        waitingSince: _now
            .subtract(const Duration(seconds: 42))
            .millisecondsSinceEpoch,
        permission: const PermissionRequest(
          toolCallId: 'tc-1',
          question: '运行测试，确认链接解析的修复',
          options: [
            PermissionOption(
              optionId: 'allow',
              name: '',
              kind: PermissionOptionKind.allowOnce,
            ),
            PermissionOption(
              optionId: 'always',
              name: '',
              kind: PermissionOptionKind.allowAlways,
            ),
            PermissionOption(
              optionId: 'reject',
              name: '',
              kind: PermissionOptionKind.rejectOnce,
            ),
          ],
          details: ApprovalDetails(
            kind: ApprovalKind.command,
            command: 'cargo test -p smelt-acp-view',
            cwd: '/Users/demo/code/smelt',
          ),
        ),
      ),
      PassTicket(
        runtimeId: 'rt-mini',
        session: question,
        detailsLoaded: true,
        recent: demoRecentContext('s-route', _now),
        waitingSince: _now
            .subtract(const Duration(minutes: 3, seconds: 10))
            .millisecondsSinceEpoch,
        elicitation: Elicitation(
          message: '路由规则放在哪一层？',
          isReady: false,
          fields: [
            ElicitationField(
              key: 'layer',
              title: '规则来源',
              required_: true,
              allowCustomInput: false,
              kind: ElicitationFieldKind.select,
              options: const ['订阅里的规则', '本地 overlay', '两者合并'],
              secret: false,
              chosen: Uint32List(0),
            ),
          ],
        ),
      ),
      PassTicket(
        runtimeId: 'rt-mini',
        session: failed,
        detailsLoaded: true,
        recent: demoRecentContext('s-relay', _now),
        waitingSince: _now
            .subtract(const Duration(minutes: 5))
            .millisecondsSinceEpoch,
      ),
    ];

    _sessions = {
      'rt-mbp': [
        approval,
        _s(
          's-fleet',
          '多运行时连接状态机',
          agent: 'pi',
          project: 'smelt',
          phase: SessionPhase.executingTool,
          status: SessionStatus.running,
          ago: const Duration(minutes: 12, seconds: 41),
          detail: '编辑 fleet.rs',
        ),
        _s(
          's-copy',
          '首页文案改版',
          agent: 'grok',
          project: 'website',
          phase: SessionPhase.succeeded,
          ago: const Duration(minutes: 2),
          attention: const AttentionItem(
            sessionId: 's-copy',
            title: '已完成',
            message: '完成',
            kind: AttentionKind.success,
          ),
        ),
        _s(
          's-docs',
          '整理 workspace 文档',
          agent: 'claude',
          project: 'smelt',
          phase: SessionPhase.succeeded,
          ago: const Duration(hours: 2),
        ),
        _s(
          's-zsh',
          'zsh',
          agent: '',
          project: 'smelt',
          kind: SessionKind.terminal,
          ago: const Duration(days: 1),
        ),
      ],
      'rt-mini': [
        question,
        failed,
        _s(
          's-notes',
          '整理 0.10 发布说明',
          agent: 'copilot',
          project: 'docs',
          phase: SessionPhase.thinking,
          status: SessionStatus.running,
          ago: const Duration(minutes: 3, seconds: 5),
        ),
        _s(
          's-quota',
          '额度面板',
          agent: 'codex',
          project: 'smelt',
          phase: SessionPhase.succeeded,
          ago: const Duration(hours: 5),
        ),
      ],
      'rt-build': [
        _s(
          's-build',
          'cargo build --release',
          agent: '',
          project: 'smelt',
          kind: SessionKind.terminal,
          phase: SessionPhase.idle,
          status: SessionStatus.running,
          ago: const Duration(minutes: 6),
        ),
      ],
    };
  }

  FleetRuntime _runtime(
    String id,
    String name,
    int accent,
    String os,
    ConnectionState state,
    LinkPath? path,
    int? rtt,
  ) => FleetRuntime(
    record: RuntimeRecord(
      runtimeId: id,
      name: name,
      endpointId: 'demo-$id',
      hostname: name,
      os: os,
      version: '0.10.0',
      accent: accent,
      position: accent,
      pairedAtMs: _now
          .subtract(const Duration(days: 30))
          .millisecondsSinceEpoch,
    ),
    status: RuntimeStatus(
      state: state,
      link: path == null ? null : LinkMetrics(path: path, rttMs: rtt),
      error: state == ConnectionState.reconnecting
          ? const SmeltError(kind: SmeltErrorKind.offline, message: '连不上运行时')
          : null,
    ),
  );

  Pass _passView() {
    List<SessionSummary> pick(String rt, bool Function(SessionSummary) f) =>
        (_sessions[rt] ?? const []).where(f).toList();
    return Pass(
      tickets: List.of(_tickets),
      stations: [
        for (final r in _runtimes)
          PassStation(
            runtimeId: r.record.runtimeId,
            running: pick(
              r.record.runtimeId,
              (s) => s.status == SessionStatus.running,
            ),
            done: pick(
              r.record.runtimeId,
              (s) => s.phase == SessionPhase.succeeded && s.attention != null,
            ),
            // 同内核 `pass::is_recent`：动过、不在别的组，不分相位，新的在前，最多 5 条。
            recent:
                (pick(
                      r.record.runtimeId,
                      (s) =>
                          s.status == SessionStatus.idle &&
                          !const {
                            AttentionKind.approval,
                            AttentionKind.input,
                            AttentionKind.failure,
                          }.contains(s.attention?.kind) &&
                          !(s.phase == SessionPhase.succeeded &&
                              s.attention != null) &&
                          s.updatedAt > 0,
                    )..sort((a, b) => b.updatedAt.compareTo(a.updatedAt)))
                    .take(5)
                    .toList(),
            cached: r.status.state != ConnectionState.connected,
            savedAtMs: r.status.state == ConnectionState.connected
                ? null
                : _now
                      .subtract(const Duration(minutes: 9))
                      .millisecondsSinceEpoch,
          ),
      ],
    );
  }

  Stream<T> _seeded<T>(StreamController<T> controller, T Function() current) {
    late StreamController<T> out;
    StreamSubscription<T>? sub;
    out = StreamController<T>(
      onListen: () {
        out.add(current());
        sub = controller.stream.listen(out.add);
      },
      onCancel: () => sub?.cancel(),
    );
    return out.stream;
  }

  void _publish() {
    _fleet.add(FleetView(runtimes: List.of(_runtimes)));
    _pass.add(_passView());
  }

  void _resolve(String runtimeId, String sessionId) {
    _tickets.removeWhere(
      (t) => t.runtimeId == runtimeId && t.session.id == sessionId,
    );
    final list = _sessions[runtimeId];
    if (list != null) {
      final i = list.indexWhere((s) => s.id == sessionId);
      if (i >= 0) {
        final s = list[i];
        list[i] = SessionSummary(
          id: s.id,
          kind: s.kind,
          title: s.title,
          phase: SessionPhase.executingTool,
          status: SessionStatus.running,
          agent: s.agent,
          cwd: s.cwd,
          projectRoot: s.projectRoot,
          projectTitle: s.projectTitle,
          projectOrder: s.projectOrder,
          updatedAt: DateTime.now().millisecondsSinceEpoch ~/ 1000,
        );
      }
    }
    _publish();
    _catalog.add(
      CatalogEvent(
        runtimeId: runtimeId,
        kind: CatalogEventKind.sessions,
        sessions: SessionList(
          sessions: List.of(list ?? const []),
          cached: false,
        ),
      ),
    );
  }

  // 身份与配对 ---------------------------------------------------------------
  @override
  PairingTarget parsePairing(String uri) =>
      const PairingTarget(endpointId: 'demo-new');

  @override
  Future<RuntimeRecord> pair(String uri, String deviceName) async =>
      _runtimes.first.record;

  // 机队 ---------------------------------------------------------------------
  @override
  Stream<FleetView> fleetEvents() =>
      _seeded(_fleet, () => FleetView(runtimes: List.of(_runtimes)));

  FleetRuntime _with(FleetRuntime r, {String? name, int? accent}) =>
      FleetRuntime(
        record: RuntimeRecord(
          runtimeId: r.record.runtimeId,
          name: name ?? r.record.name,
          endpointId: r.record.endpointId,
          relayUrl: r.record.relayUrl,
          hostname: r.record.hostname,
          os: r.record.os,
          version: r.record.version,
          accent: accent ?? r.record.accent,
          position: r.record.position,
          pairedAtMs: r.record.pairedAtMs,
        ),
        status: r.status,
        pushSupported: r.pushSupported,
      );

  @override
  Future<void> renameRuntime(String runtimeId, String name) async {
    _runtimes = [
      for (final r in _runtimes)
        r.record.runtimeId == runtimeId ? _with(r, name: name) : r,
    ];
    _publish();
  }

  @override
  Future<void> setRuntimeAccent(String runtimeId, int accent) async {
    _runtimes = [
      for (final r in _runtimes)
        r.record.runtimeId == runtimeId ? _with(r, accent: accent) : r,
    ];
    _publish();
  }

  @override
  Future<void> moveRuntime(String runtimeId, int toIndex) async {
    final r = _runtimes.firstWhere((r) => r.record.runtimeId == runtimeId);
    _runtimes = [..._runtimes]..remove(r);
    _runtimes.insert(toIndex.clamp(0, _runtimes.length), r);
    _publish();
  }

  @override
  Future<void> forgetRuntime(String runtimeId) async {
    _runtimes = [
      for (final r in _runtimes)
        if (r.record.runtimeId != runtimeId) r,
    ];
    _tickets.removeWhere((t) => t.runtimeId == runtimeId);
    _sessions.remove(runtimeId);
    _publish();
  }

  @override
  Future<void> reconnectRuntime(String runtimeId) async {}

  @override
  Future<void> verifyConnections() async {
    await Future<void>.delayed(const Duration(milliseconds: 400));
  }

  @override
  Future<List<RuntimeRecord>> importRuntimes(
    List<RuntimeImport> runtimes,
  ) async => const [];

  /// 演示数据里没有真机器，登记无处可去。
  @override
  void setPushRegistration(PushRegistration? registration) {}

  // 指挥台 -------------------------------------------------------------------
  @override
  Stream<Pass> passEvents() => _seeded(_pass, _passView);

  // 会话目录 -----------------------------------------------------------------
  @override
  Stream<CatalogEvent> catalogEvents() {
    late StreamController<CatalogEvent> out;
    StreamSubscription<CatalogEvent>? sub;
    out = StreamController<CatalogEvent>(
      onListen: () {
        for (final r in _runtimes) {
          final id = r.record.runtimeId;
          out.add(
            CatalogEvent(
              runtimeId: id,
              kind: CatalogEventKind.sessions,
              sessions: SessionList(
                sessions: List.of(_sessions[id] ?? const []),
                cached: r.status.state != ConnectionState.connected,
                savedAtMs: r.status.state == ConnectionState.connected
                    ? null
                    : _now
                          .subtract(const Duration(minutes: 9))
                          .millisecondsSinceEpoch,
              ),
            ),
          );
        }
        sub = _catalog.stream.listen(out.add);
      },
      onCancel: () => sub?.cancel(),
    );
    return out.stream;
  }

  @override
  void markRead(String runtimeId, String sessionId) {}

  /// 开着的项目根（每台机器一样），按打开的先后。
  final List<String> _projectRoots = [
    '/Users/demo/code/smelt',
    '/Users/demo/code/velum',
    '/Users/demo/code/website',
  ];

  static const _home = '/Users/demo';

  /// 合成的目录树：目录 → 子目录名。
  static const Map<String, List<String>> _dirs = {
    '/': ['Users'],
    '/Users': ['demo'],
    '/Users/demo': ['code', 'Documents', 'Downloads'],
    '/Users/demo/code': ['playground', 'smelt', 'velum', 'website'],
    '/Users/demo/code/playground': ['rust-demo'],
    '/Users/demo/code/smelt': ['crates', 'docs', 'mobile'],
  };

  static String _name(String path) =>
      path == '/' ? '/' : path.substring(path.lastIndexOf('/') + 1);

  static String? _parent(String path) {
    if (path == '/') return null;
    final cut = path.lastIndexOf('/');
    return cut == 0 ? '/' : path.substring(0, cut);
  }

  static String _display(String path) => path == _home
      ? '~'
      : path.startsWith('$_home/')
      ? '~${path.substring(_home.length)}'
      : path;

  @override
  Future<DirListing> browseDirs(String runtimeId, {String? path}) async {
    final dir =
        path ??
        (_projectRoots.isEmpty ? _home : _parent(_projectRoots.last) ?? _home);
    // 树里没列子目录的就是叶子。
    final children = _dirs[dir] ?? const <String>[];
    final shortcuts = <String>[_home];
    for (final root in _projectRoots.reversed) {
      final parent = _parent(root);
      if (parent != null && !shortcuts.contains(parent)) shortcuts.add(parent);
    }
    return DirListing(
      path: dir,
      displayPath: _display(dir),
      name: _name(dir),
      parent: _parent(dir),
      opened: _projectRoots.contains(dir),
      shortcuts: [
        for (final s in shortcuts.take(4))
          DirShortcut(label: _display(s), path: s),
      ],
      entries: [
        for (final path in children.map((c) => dir == '/' ? '/$c' : '$dir/$c'))
          DirEntry(
            name: _name(path),
            path: path,
            opened: _projectRoots.contains(path),
          ),
      ],
    );
  }

  @override
  Future<WorkspaceProject> openProject(String runtimeId, String root) async {
    if (!_projectRoots.contains(root)) _projectRoots.add(root);
    return _projectOf(root);
  }

  WorkspaceProject _projectOf(String root) => WorkspaceProject(
    root: root,
    title: _name(root),
    order: _projectRoots.indexOf(root),
  );

  @override
  Future<WorkspaceCatalog> workspaceCatalog(String runtimeId) async =>
      WorkspaceCatalog(
        projects: [for (final root in _projectRoots) _projectOf(root)],
        agents: const [
          AcpAgentOption(id: 'claude', kind: 'claude', label: 'Claude Code'),
          AcpAgentOption(id: 'codex', kind: 'codex', label: 'Codex'),
          AcpAgentOption(id: 'pi', kind: 'pi', label: 'Pi'),
          AcpAgentOption(
            id: 'assistant',
            kind: 'pi',
            label: '工作助手',
            agentDefinitionId: 'def-assistant',
          ),
        ],
        sections: const LaunchSections(
          common: [
            LaunchAction(
              key: 'claude',
              label: 'Claude Code',
              target: LaunchTarget.conversation,
              agent: 'claude',
            ),
            LaunchAction(
              key: 'codex',
              label: 'Codex',
              target: LaunchTarget.conversation,
              agent: 'codex',
            ),
            LaunchAction(
              key: 'shell',
              label: 'Shell',
              target: LaunchTarget.blankTerminal,
              agent: '',
            ),
          ],
          terminal: [
            LaunchAction(
              key: 'claude-cli',
              label: 'Claude Code',
              target: LaunchTarget.terminal,
              agent: 'claude',
            ),
          ],
          conversation: [
            LaunchAction(
              key: 'pi',
              label: 'Pi',
              target: LaunchTarget.conversation,
              agent: 'pi',
            ),
          ],
        ),
      );

  void _pushSessions(String runtimeId) {
    _catalog.add(
      CatalogEvent(
        runtimeId: runtimeId,
        kind: CatalogEventKind.sessions,
        sessions: SessionList(
          sessions: List.of(_sessions[runtimeId] ?? const []),
          cached: false,
        ),
      ),
    );
    _publish();
  }

  /// 同一个 requestId 落到同一个会话（和运行时一样幂等）。
  final Map<String, String> _created = {};

  @override
  Future<String> createSession(
    String runtimeId, {
    required String projectRoot,
    String? agentOptionId,
    String? launchKey,
    String? resumeId,
    SessionKind? kind,
    String? requestId,
  }) async {
    final known = requestId == null ? null : _created[requestId];
    if (known != null) return known;
    final id = 'demo-new-${_created.length + 1}';
    if (requestId != null) _created[requestId] = id;
    final agent = agentOptionId ?? launchKey ?? '';
    final project = projectRoot
        .split('/')
        .where((p) => p.isNotEmpty)
        .lastOrNull;
    final session = SessionSummary(
      id: id,
      kind: kind ?? SessionKind.acp,
      title: resumeId != null ? '继续：$resumeId' : '新会话',
      phase: SessionPhase.connecting,
      status: SessionStatus.running,
      agent: agent,
      cwd: projectRoot.isEmpty ? null : projectRoot,
      projectRoot: projectRoot.isEmpty ? null : projectRoot,
      projectTitle: project,
      projectOrder: 0,
      updatedAt: DateTime.now().millisecondsSinceEpoch ~/ 1000,
    );
    (_sessions[runtimeId] ??= []).insert(0, session);
    await Future<void>.delayed(const Duration(milliseconds: 300));
    _pushSessions(runtimeId);
    return id;
  }

  @override
  Future<void> deleteSession(String runtimeId, String sessionId) async {
    _sessions[runtimeId]?.removeWhere((s) => s.id == sessionId);
    _pushSessions(runtimeId);
  }

  final Map<String, bool> _automationEnabled = {
    'auto-standup': true,
    'auto-backup': false,
  };
  final Set<String> _automationRunning = {};

  AutomationRunSummary? _runningNow(String id) =>
      _automationRunning.contains(id)
      ? AutomationRunSummary(
          status: AutomationRunStatus.running,
          source: AutomationRunSource.manual,
          startedAt: DateTime.now().millisecondsSinceEpoch ~/ 1000,
        )
      : null;

  AutomationCatalog get _automations => AutomationCatalog(
    automations: [
      AutomationSummary(
        id: 'auto-standup',
        name: '每日站会摘要',
        enabled: _automationEnabled['auto-standup']!,
        schedules: const [
          AutomationSchedule(
            kind: ScheduleKind.daily,
            hour: 9,
            minute: 30,
            minutes: 0,
            hours: 0,
            days: 0,
            endHour: 0,
            endMinute: 0,
          ),
        ],
        eventTopics: const [],
        webhook: false,
        action: AutomationActionKind.agent,
        agentName: '工作助手',
        nextRunAt: _ago(const Duration(hours: -14)),
        lastRun:
            _runningNow('auto-standup') ??
            AutomationRunSummary(
              status: AutomationRunStatus.completed,
              source: AutomationRunSource.scheduled,
              startedAt: _ago(const Duration(hours: 10)),
              finishedAt: _ago(const Duration(hours: 10, minutes: -2)),
            ),
      ),
      AutomationSummary(
        id: 'auto-backup',
        name: '夜间备份检查',
        enabled: _automationEnabled['auto-backup']!,
        schedules: const [],
        eventTopics: const [],
        webhook: true,
        action: AutomationActionKind.shell,
        lastRun:
            _runningNow('auto-backup') ??
            AutomationRunSummary(
              status: AutomationRunStatus.failed,
              source: AutomationRunSource.webhook,
              startedAt: _ago(const Duration(days: 1)),
              error: '磁盘空间不足',
            ),
      ),
    ],
    agents: List.of(_agents),
  );

  @override
  Future<AutomationCatalog> automationCatalog(String runtimeId) async =>
      _automations;

  // 智能体 -------------------------------------------------------------------

  final List<AgentDefinitionSummary> _agents = [
    const AgentDefinitionSummary(
      id: 'def-assistant',
      name: '工作助手',
      description: '开发和日常工作处理',
      engineKindId: 'pi',
      prompt: '你是工作助手，负责开发和日常工作处理。',
      plugins: ['skill:lark-cli', 'skill:meegle'],
      mcpServers: ['notion'],
      contextFolders: ['/Users/demo/notes'],
      contextLinks: [],
      modelProvider: '',
      modelId: '',
      memoryEnabled: true,
    ),
  ];
  int _agentSeq = 0;

  static AgentDraft _draftOf(AgentDefinitionSummary a) => AgentDraft(
    name: a.name,
    prompt: a.prompt,
    plugins: a.plugins,
    mcpServers: a.mcpServers,
    contextFolders: a.contextFolders,
    contextLinks: a.contextLinks,
    modelProvider: a.modelProvider,
    modelId: a.modelId,
    memoryEnabled: a.memoryEnabled,
  );

  static AgentDefinitionSummary _summaryOf(String id, AgentDraft d) =>
      AgentDefinitionSummary(
        id: id,
        name: d.name.trim(),
        description: '',
        engineKindId: 'pi',
        prompt: d.prompt,
        plugins: d.plugins,
        mcpServers: d.mcpServers,
        contextFolders: d.contextFolders,
        contextLinks: d.contextLinks,
        modelProvider: d.modelProvider,
        modelId: d.modelId,
        memoryEnabled: d.memoryEnabled,
      );

  static const _demoChoices = AgentChoices(
    models: [
      ModelGroup(
        provider: 'anthropic',
        providerName: 'Anthropic',
        models: [
          ModelOption(
            provider: 'anthropic',
            id: 'claude-opus',
            label: 'Anthropic / Claude Opus',
          ),
          ModelOption(
            provider: 'anthropic',
            id: 'claude-sonnet',
            label: 'Anthropic / Claude Sonnet',
          ),
        ],
      ),
    ],
    plugins: [
      PluginOption(
        id: 'skill:lark-cli',
        kind: PluginKind.skill,
        name: 'lark-cli',
        description: '飞书文档、消息、任务',
      ),
      PluginOption(
        id: 'skill:meegle',
        kind: PluginKind.skill,
        name: 'meegle',
        description: '飞书项目工作项',
      ),
      PluginOption(
        id: 'extension:notify',
        kind: PluginKind.extension_,
        name: 'notify',
        description: '',
        broken: '文件不存在',
      ),
    ],
    mcpServers: [
      McpOption(name: 'notion', description: '笔记', enabled: true),
      McpOption(name: 'linear', enabled: false),
    ],
  );

  @override
  Future<AgentChoices> agentChoices(String runtimeId) async => _demoChoices;

  @override
  Future<AgentEditor> agentEditor(String runtimeId, {String? agentId}) async {
    final agent = agentId == null
        ? null
        : _agents.firstWhere((a) => a.id == agentId);
    return AgentEditor(
      draft: agent == null
          ? const AgentDraft(
              name: '',
              prompt: '',
              plugins: [],
              mcpServers: [],
              contextFolders: [],
              contextLinks: [],
              modelProvider: '',
              modelId: '',
              memoryEnabled: true,
            )
          : _draftOf(agent),
      choices: _demoChoices,
    );
  }

  @override
  Future<AgentDefinitionSummary> createAgent(
    String runtimeId,
    AgentDraft draft,
  ) async {
    final agent = _summaryOf('def-new-${++_agentSeq}', draft);
    _agents.add(agent);
    return agent;
  }

  @override
  Future<AgentDefinitionSummary> updateAgent(
    String runtimeId,
    String agentId, {
    required AgentDraft original,
    required AgentDraft edited,
  }) async {
    final index = _agents.indexWhere((a) => a.id == agentId);
    final agent = _summaryOf(agentId, edited);
    _agents[index] = agent;
    return agent;
  }

  @override
  Future<void> deleteAgent(String runtimeId, String agentId) async {
    _agents.removeWhere((a) => a.id == agentId);
  }

  late final List<MemoryItem> _agentMemories = [
    MemoryItem(
      fileName: 'deploy-window.md',
      name: '发版窗口',
      description: '周四晚上不发版',
      rule: false,
      pendingReason: '读过外部网页的那一轮写的',
      stale: false,
      readCount: BigInt.zero,
      body: '周四晚上有例行维护，发版挪到周五上午。',
    ),
    MemoryItem(
      fileName: 'reply-style.md',
      name: '回复风格',
      description: '先给结论再给细节',
      rule: true,
      stale: false,
      readCount: BigInt.from(12),
      lastUsedMs: _now
          .subtract(const Duration(hours: 2))
          .millisecondsSinceEpoch,
      body: '回复先给结论，细节放后面。',
    ),
  ];

  AgentMemories get _memories => AgentMemories(
    agent: MemoryShelf(items: List.of(_agentMemories), coreOverBudget: false),
    user: const MemoryShelf(items: [], coreOverBudget: false),
    pending: _agentMemories.where((m) => m.pendingReason != null).length,
  );

  @override
  Future<AgentMemories> agentMemories(String runtimeId, String agentId) async =>
      _memories;

  @override
  Future<AgentMemories> approveMemory(
    String runtimeId,
    String agentId,
    MemoryLayer layer,
    String fileName,
  ) async {
    final index = _agentMemories.indexWhere((m) => m.fileName == fileName);
    final m = _agentMemories[index];
    _agentMemories[index] = MemoryItem(
      fileName: m.fileName,
      name: m.name,
      description: m.description,
      rule: m.rule,
      stale: m.stale,
      readCount: m.readCount,
      lastUsedMs: m.lastUsedMs,
      body: m.body,
    );
    return _memories;
  }

  @override
  Future<AgentMemories> deleteMemory(
    String runtimeId,
    String agentId,
    MemoryLayer layer,
    String fileName,
  ) async {
    _agentMemories.removeWhere((m) => m.fileName == fileName);
    return _memories;
  }

  @override
  Future<AutomationCatalog> setAutomationEnabled(
    String runtimeId,
    String automationId,
    bool enabled,
  ) async {
    await Future<void>.delayed(const Duration(milliseconds: 200));
    _automationEnabled[automationId] = enabled;
    return _automations;
  }

  @override
  Future<AutomationCatalog> runAutomationOnce(
    String runtimeId,
    String automationId,
  ) async {
    await Future<void>.delayed(const Duration(milliseconds: 200));
    _automationRunning.add(automationId);
    return _automations;
  }

  @override
  Future<List<HistorySessionSummary>> listHistory(
    String runtimeId,
    String projectRoot,
    String agentOptionId,
  ) async => [
    HistorySessionSummary(
      resumeId: 'h-1',
      title: '重构 catalog 投影',
      lastActiveAtMs: _now
          .subtract(const Duration(days: 1))
          .millisecondsSinceEpoch,
      messageCount: 48,
    ),
    HistorySessionSummary(
      resumeId: 'h-2',
      title: '排查 iroh 重连',
      customTitle: 'iroh 重连（已修）',
      lastActiveAtMs: _now
          .subtract(const Duration(days: 4))
          .millisecondsSinceEpoch,
      messageCount: 112,
    ),
  ];

  @override
  Future<HistoryRenameResult> renameHistory(
    String runtimeId, {
    required String projectRoot,
    required String agentOptionId,
    required String resumeId,
    String? title,
  }) async => HistoryRenameResult(title: title ?? '', customTitle: title);

  // 对话 ---------------------------------------------------------------------
  @override
  Stream<ConversationEvent> watchConversation(
    String runtimeId,
    String sessionId,
  ) => demoConversation(
    runtimeId,
    sessionId,
    now: _now,
    resolvedToolCalls: _resolvedToolCalls,
  );

  /// 已经批过的工具调用：对话演示数据据此不再挂待批。
  final Set<String> _resolvedToolCalls = {};

  @override
  Future<bool> loadOlder(String runtimeId, String sessionId) async => false;

  @override
  Future<PendingActions> pendingActions(
    String runtimeId,
    String sessionId,
  ) async {
    final t = _tickets
        .where((t) => t.runtimeId == runtimeId && t.session.id == sessionId)
        .firstOrNull;
    return PendingActions(
      permissions: [?t?.permission],
      elicitation: t?.elicitation,
      recent: t?.recent ?? const [],
    );
  }

  int _requests = 0;

  @override
  String newRequestId() => 'demo-request-${_requests++}';

  @override
  Future<void> sendMessage(
    String runtimeId,
    String sessionId, {
    required String requestId,
    required String text,
    required List<ImageInput> images,
  }) async {}

  @override
  Future<void> cancelTurn(String runtimeId, String sessionId) async {}

  @override
  Future<void> setConfigOption(
    String runtimeId,
    String sessionId, {
    required String configId,
    required String valueId,
    bool? boolean,
  }) async {}

  @override
  Future<void> respondApproval(
    String runtimeId,
    String sessionId, {
    required String toolCallId,
    required String optionId,
  }) async {
    await Future<void>.delayed(const Duration(milliseconds: 250));
    _resolvedToolCalls.add(toolCallId);
    _resolve(runtimeId, sessionId);
  }

  @override
  Future<void> chooseElicitation(
    String runtimeId,
    String sessionId, {
    required int fieldIndex,
    required int optionIndex,
  }) async {
    final i = _tickets.indexWhere(
      (t) => t.runtimeId == runtimeId && t.session.id == sessionId,
    );
    final e = i < 0 ? null : _tickets[i].elicitation;
    if (e == null) return;
    final fields = [
      for (final (index, f) in e.fields.indexed)
        index == fieldIndex
            ? ElicitationField(
                key: f.key,
                title: f.title,
                required_: f.required_,
                allowCustomInput: f.allowCustomInput,
                kind: f.kind,
                options: f.options,
                secret: f.secret,
                url: f.url,
                chosen: Uint32List.fromList([optionIndex]),
                textValue: f.textValue,
              )
            : f,
    ];
    final t = _tickets[i];
    _tickets[i] = PassTicket(
      runtimeId: t.runtimeId,
      session: t.session,
      detailsLoaded: true,
      recent: t.recent,
      waitingSince: t.waitingSince,
      elicitation: Elicitation(
        message: e.message,
        fields: fields,
        isReady: true,
      ),
    );
    _publish();
  }

  @override
  Future<void> updateElicitationText(
    String runtimeId,
    String sessionId, {
    required int fieldIndex,
    required String value,
  }) async {}

  @override
  Future<void> submitElicitation(
    String runtimeId,
    String sessionId,
    List<ElicitationTextInput> textValues,
  ) async {
    await Future<void>.delayed(const Duration(milliseconds: 250));
    _resolve(runtimeId, sessionId);
  }

  @override
  Future<void> dismissElicitation(String runtimeId, String sessionId) async =>
      _resolve(runtimeId, sessionId);

  @override
  Future<SessionSkills> listSkills(String runtimeId, String sessionId) async =>
      const SessionSkills(
        supported: true,
        skills: [
          SessionSkill(name: 'lark-cli', description: '飞书个人身份 CLI'),
          SessionSkill(name: 'meegle', description: '飞书项目操作'),
        ],
      );

  @override
  Future<ImageData> loadImage(
    String runtimeId,
    String sessionId, {
    required int entry,
    required String digest,
    int? maxEdge,
  }) => demoImage(entry, digest);

  // 终端 ---------------------------------------------------------------------
  @override
  Stream<TerminalEvent> terminalAttach(
    String runtimeId,
    String sessionId, {
    required TerminalGeometry geometry,
    int? maxScrollbackLines,
  }) {
    // 机器没连上就不会有终端流（真实内核会一直重试），演示也不假装实时。
    final online = _runtimes.any(
      (r) =>
          r.record.runtimeId == runtimeId &&
          r.status.state == ConnectionState.connected,
    );
    // 不推也不关：和真实内核一样一直「连接中」。
    return online
        ? demoTerminal(sessionId, geometry)
        : StreamController<TerminalEvent>().stream;
  }

  @override
  void terminalResize(
    String runtimeId,
    String sessionId,
    TerminalGeometry geometry,
  ) {}

  @override
  void terminalInput(String runtimeId, String sessionId, String data) =>
      demoTerminalInput(sessionId, data);

  @override
  void terminalDetach(String runtimeId, String sessionId) {}

  // 草稿 ---------------------------------------------------------------------
  @override
  Future<MessageDraft?> loadDraft(String runtimeId, String sessionId) async =>
      _drafts['$runtimeId/$sessionId'];

  @override
  Future<void> saveDraft(
    String runtimeId,
    String sessionId,
    MessageDraft d,
  ) async => _drafts['$runtimeId/$sessionId'] = d;

  @override
  Future<void> clearDraft(String runtimeId, String sessionId) async =>
      _drafts.remove('$runtimeId/$sessionId');

  @override
  Future<bool> resolveDraft(
    String runtimeId,
    String sessionId, {
    required String requestId,
    required bool succeeded,
  }) async {
    final key = '$runtimeId/$sessionId';
    final d = _drafts[key];
    if (d?.requestId != requestId) return false;
    if (succeeded) {
      _drafts.remove(key);
    } else {
      _drafts[key] = MessageDraft(content: d!.content, images: d.images);
    }
    return true;
  }
}
