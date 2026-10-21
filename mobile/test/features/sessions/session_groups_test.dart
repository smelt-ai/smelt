import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/features/sessions/session_groups.dart';

SessionSummary _s(
  String id, {
  String? root,
  String? project,
  int order = 0,
  String agent = 'claude',
  String? definition,
  AutomationSource? automation,
  String? cwd,
}) => SessionSummary(
  id: id,
  kind: SessionKind.acp,
  title: 'title $id',
  phase: SessionPhase.idle,
  status: SessionStatus.idle,
  agent: agent,
  cwd: cwd ?? root,
  projectRoot: root,
  projectTitle: project,
  projectOrder: order,
  updatedAt: 0,
  agentDefinitionId: definition,
  automation: automation,
);

FleetRuntime _runtime(String id, {bool online = true}) => FleetRuntime(
  record: RuntimeRecord(
    runtimeId: id,
    name: 'machine $id',
    endpointId: 'e-$id',
    version: '0',
    accent: 0,
    position: 0,
    pairedAtMs: 0,
  ),
  status: RuntimeStatus(
    state: online ? ConnectionState.connected : ConnectionState.reconnecting,
  ),
);

const _automation = AutomationSource(
  automationName: '每日站会摘要',
  runStatus: AutomationRunStatus.completed,
  runSource: AutomationRunSource.scheduled,
);

void main() {
  final lists = {
    'a': SessionList(
      cached: false,
      sessions: [
        _s('1', root: '/code/web', project: 'web', order: 1),
        _s('2', root: '/code/api/', project: 'api', order: 0),
        _s('3', definition: 'def-x', cwd: '/spaces/x'),
        _s('4', automation: _automation, cwd: '/auto/run-1'),
        _s('5', root: '/code/web', project: 'web', order: 1, agent: 'codex'),
        // 开在项目里的智能体对话：留在项目组，不拆进「智能体对话」。
        _s(
          '7',
          root: '/code/web',
          project: 'web',
          order: 1,
          definition: 'def-x',
        ),
      ],
    ),
    'b': SessionList(cached: true, savedAtMs: 1, sessions: [_s('6')]),
  };

  List<MachineSessions> group({
    String? machine,
    String query = '',
    Map<String, WorkspaceCatalog> workspaces = const {},
  }) => groupSessions(
    runtimes: [_runtime('a'), _runtime('b', online: false)],
    listOf: (id) => lists[id],
    workspaces: workspaces,
    machine: machine,
    query: query,
    agentName: (rt, def) => def == 'def-x' ? '工作助手' : null,
  );

  test('按机器 → 项目分组；不在项目里的智能体对话放最前，自动化运行单独成组', () {
    final machines = group();
    expect(machines.map((m) => m.runtime.record.runtimeId), ['a', 'b']);
    final a = machines.first.groups;
    expect(a.map((g) => g.kind), [
      SessionGroupKind.agentChats,
      SessionGroupKind.project,
      SessionGroupKind.project,
      SessionGroupKind.automationRuns,
    ]);
    // 只有不挂在项目下的智能体对话进这一组。
    expect(a[0].sessions.map((s) => s.id), ['3']);
    // api 的 order 更小，排在前面；项目根去掉末尾斜杠。
    expect(a[1].title, 'api');
    expect(a[1].projectRoot, '/code/api');
    expect(a[2].title, 'web');
    // 项目里的智能体对话按原次序留在项目组。
    expect(a[2].sessions.map((s) => s.id), ['1', '5', '7']);
    expect(a[3].sessions.map((s) => s.id), ['4']);
    // 智能体对话、自动化运行不按 cwd 造出「项目」。
    expect(a.where((g) => g.projectRoot == '/spaces/x'), isEmpty);
    expect(a.where((g) => g.projectRoot == '/auto/run-1'), isEmpty);

    // 认不出项目的会话归「无项目」，不给历史入口。
    final b = machines.last.groups.single;
    expect(b.title, isNull);
    expect(b.projectRoot, isNull);
  });

  test('按机器筛选只留那一台', () {
    final machines = group(machine: 'b');
    expect(machines.single.runtime.record.runtimeId, 'b');
  });

  test('工作区里还没有会话的项目也列出来，搜索时不列', () {
    const workspace = WorkspaceCatalog(
      projects: [
        WorkspaceProject(root: '/code/web', title: 'web', order: 1),
        WorkspaceProject(root: '/code/docs', title: 'docs', order: 2),
      ],
      agents: [],
      sections: LaunchSections(common: [], terminal: [], conversation: []),
    );
    final a = group(workspaces: {'a': workspace}).first.groups;
    final docs = a.singleWhere((g) => g.title == 'docs');
    expect(docs.sessions, isEmpty);
    expect(docs.projectRoot, '/code/docs');
    // web 已经有会话，不重复出一组。
    expect(a.where((g) => g.title == 'web').length, 1);

    final searching = group(workspaces: {'a': workspace}, query: 'web');
    expect(searching.first.groups.where((g) => g.title == 'docs'), isEmpty);
  });

  test('搜索看标题、项目、agent、智能体名、自动化名；空组和空机器去掉', () {
    expect(group(query: 'api').single.groups.single.sessions.single.id, '2');
    // agent 品牌名，大小写不敏感。
    expect(group(query: 'CODEX').single.groups.single.sessions.single.id, '5');
    // 智能体名：项目外的在「智能体对话」，项目里的留在项目组。
    expect(
      group(query: '工作助手').single.groups.map(
        (g) => '${g.kind.name}:${g.sessions.map((s) => s.id).join(',')}',
      ),
      ['agentChats:3', 'project:7'],
    );
    expect(
      group(query: '站会').single.groups.single.kind,
      SessionGroupKind.automationRuns,
    );
    // 多个词要都出现。
    expect(group(query: 'web codex').single.groups.single.sessions.length, 1);
    expect(group(query: 'nothing-like-this'), isEmpty);
  });
}
