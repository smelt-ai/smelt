/// 「会话」页的分组：机器 → 项目。不在项目里的智能体对话、自动化运行各自成组。
///
/// 这里只决定「放在哪一组、组怎么排」，不判断谁在等你、谁算刚完成（那是内核 Pass 的事）；
/// 组内会话保持内核给的次序（已按桌面菜单排好）。抽成纯函数是为了能脱离界面单测。
library;

import '../../data/models.dart';
import '../../ui/agent_glyph.dart';

enum SessionGroupKind { project, agentChats, automationRuns }

class SessionGroup {
  const SessionGroup({
    required this.kind,
    required this.key,
    required this.sessions,
    this.title,
    this.projectRoot,
  });

  final SessionGroupKind kind;

  /// 组在这台机器里的稳定标识（项目根路径，或固定的组名）。
  final String key;

  /// 项目名。空表示认不出项目（界面写「无项目」）；非项目组也为空，组名由界面给。
  final String? title;

  /// 能列历史会话的项目根；认不出项目时为空，不出「历史会话」入口。
  final String? projectRoot;

  final List<SessionSummary> sessions;
}

class MachineSessions {
  const MachineSessions({
    required this.runtime,
    required this.list,
    required this.groups,
  });

  final FleetRuntime runtime;

  /// 这台机器的会话列表；还没收到任何快照时为空。
  final SessionList? list;
  final List<SessionGroup> groups;

  bool get isEmpty => groups.every((g) => g.sessions.isEmpty);
}

/// 项目在列表里的归属键：去掉末尾斜杠的项目根，没有就退到 cwd。
String _projectKey(SessionSummary s) {
  for (final raw in [s.projectRoot, s.cwd]) {
    final v = raw?.trim().replaceAll(RegExp(r'/+$'), '');
    if (v != null && v.isNotEmpty) return v;
  }
  return '';
}

String? _lastSegment(String path) {
  final parts = path.split('/').where((p) => p.isNotEmpty);
  return parts.isEmpty ? null : parts.last;
}

/// 搜索看的文字：标题、项目、agent（原始 id 和品牌名）、智能体名、自动化名。
String _haystack(SessionSummary s, String? agentName) => [
  s.title,
  ?s.projectTitle,
  ?s.placeLabel,
  s.agent,
  agentDisplayName(s.agent),
  ?agentName,
  ?s.automation?.automationName,
].join('\n').toLowerCase();

/// 空格分开的每个词都要出现（与），大小写不敏感。
bool matchesQuery(SessionSummary s, String query, {String? agentName}) {
  final words = query
      .toLowerCase()
      .split(RegExp(r'\s+'))
      .where((w) => w.isNotEmpty);
  if (words.isEmpty) return true;
  final hay = _haystack(s, agentName);
  return words.every(hay.contains);
}

/// 按机器顺序（[runtimes]）分组。
///
/// - [machine] 非空时只留那台机器。
/// - [workspaces] 是各台机器的工作区目录：里面还没有会话的项目也列出来（`sessions` 为空，
///   界面把它们收进一组直达历史），这样续接一个暂时没开着会话的项目不用先去电脑上开一个。
/// - [query] 非空时只留匹配的会话，空组和空机器一并去掉。
List<MachineSessions> groupSessions({
  required List<FleetRuntime> runtimes,
  required SessionList? Function(String runtimeId) listOf,
  Map<String, WorkspaceCatalog> workspaces = const {},
  String? machine,
  String query = '',
  String? Function(String runtimeId, String definitionId)? agentName,
}) {
  final searching = query.trim().isNotEmpty;
  final out = <MachineSessions>[];
  for (final runtime in runtimes) {
    final id = runtime.record.runtimeId;
    if (machine != null && id != machine) continue;
    final list = listOf(id);

    // 项目组：键 → (次序, 首次出现位置, 组)。
    final projects = <String, ({int order, int seen, _ProjectAcc acc})>{};
    final agentChats = <SessionSummary>[];
    final automationRuns = <SessionSummary>[];
    var seen = 0;

    for (final s in list?.sessions ?? const <SessionSummary>[]) {
      final definition = s.agentDefinitionId?.trim();
      final name = definition == null || definition.isEmpty
          ? null
          : agentName?.call(id, definition);
      if (!matchesQuery(s, query, agentName: name)) continue;
      // 自动化运行的工作区是运行时按自动化分配的目录，按 cwd 归项目会凭空造出一个
      // 用户没打开过的「项目」。
      if (s.automation != null) {
        automationRuns.add(s);
        continue;
      }
      // 智能体对话挂在项目下就留在项目里（和桌面侧栏一致）；不在项目里的开在智能体
      // 自己的 space，同理不能按 cwd 造项目，归「智能体对话」。
      if (definition != null &&
          definition.isNotEmpty &&
          (s.projectRoot?.trim().isEmpty ?? true)) {
        agentChats.add(s);
        continue;
      }
      final key = _projectKey(s);
      final entry = projects.putIfAbsent(
        key,
        () => (
          order: s.projectOrder,
          seen: seen++,
          acc: _ProjectAcc(
            title: s.projectTitle?.trim().isNotEmpty == true
                ? s.projectTitle!.trim()
                : _lastSegment(key),
          ),
        ),
      );
      entry.acc.sessions.add(s);
    }

    if (!searching) {
      for (final p in workspaces[id]?.projects ?? const <WorkspaceProject>[]) {
        final key = p.root.trim().replaceAll(RegExp(r'/+$'), '');
        if (key.isEmpty || projects.containsKey(key)) continue;
        projects[key] = (
          order: p.order,
          seen: seen++,
          acc: _ProjectAcc(title: p.title),
        );
      }
    }

    final ordered = projects.entries.toList()
      ..sort((a, b) {
        final byOrder = a.value.order.compareTo(b.value.order);
        return byOrder != 0 ? byOrder : a.value.seen.compareTo(b.value.seen);
      });

    final groups = <SessionGroup>[
      // 智能体对话不属于任何项目，放在最前，不被一长串项目压到底下。
      if (agentChats.isNotEmpty)
        SessionGroup(
          kind: SessionGroupKind.agentChats,
          key: '#agents',
          sessions: List.unmodifiable(agentChats),
        ),
      for (final e in ordered)
        SessionGroup(
          kind: SessionGroupKind.project,
          key: e.key,
          title: e.value.acc.title,
          projectRoot: e.key.isEmpty ? null : e.key,
          sessions: List.unmodifiable(e.value.acc.sessions),
        ),
      if (automationRuns.isNotEmpty)
        SessionGroup(
          kind: SessionGroupKind.automationRuns,
          key: '#automations',
          sessions: List.unmodifiable(automationRuns),
        ),
    ];

    if (searching) {
      groups.removeWhere((g) => g.sessions.isEmpty);
      if (groups.isEmpty) continue;
    }
    out.add(MachineSessions(runtime: runtime, list: list, groups: groups));
  }
  return out;
}

class _ProjectAcc {
  _ProjectAcc({required this.title});

  final String? title;
  final sessions = <SessionSummary>[];
}
