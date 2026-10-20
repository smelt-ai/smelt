/// 对话页的演示数据（**合成的**：路径、命令、输出都是编的，只用来看界面）。
///
/// - `s-fleet`：一段进行中的长对话——用户消息（带一张截图）、助手 markdown（代码块、
///   链接）、工具调用（读文件、带 diff 的编辑、带终端输出的命令；完成 / 失败 / 进行中）、
///   计划清单、一条后台任务便签、模型与配置项。
/// - `s-md`：停在一条待批的审批上，和指挥台那张单据（`tc-1`）是同一件事。
/// - `s-route`：停在一个选择题上，和指挥台那张单据一致。
/// - 断线那台机器（`rt-build`）上的会话：来自离线缓存的快照。
/// - 其它会话：空对话。
library;

import 'dart:typed_data';

import 'package:image/image.dart' as img;

import '../models.dart';

Stream<ConversationEvent> demoConversation(
  String runtimeId,
  String sessionId, {
  required DateTime now,
  Set<String> resolvedToolCalls = const {},
}) {
  final view = switch (sessionId) {
    's-fleet' => _fleet(now),
    's-md' => _markdown(now, approved: resolvedToolCalls.contains('tc-1')),
    's-route' => _route(now),
    _ => _empty(sessionId),
  };
  final cached = runtimeId == 'rt-build';
  return Stream.value(
    ConversationEvent(
      kind: ConversationEventKind.snapshot,
      view: view,
      cached: cached,
      savedAtMs: cached
          ? now.subtract(const Duration(minutes: 9)).millisecondsSinceEpoch
          : null,
    ),
  );
}

/// 一张合成的「截图」：钢灰底上一根轨、两张单据、几行搁板。按需现画，不带资源文件。
Future<ImageData> demoImage(int entry, String digest) async {
  const w = 360;
  const h = 240;
  final image = img.Image(width: w, height: h);
  img.fill(image, color: img.ColorRgb8(236, 238, 240));
  // 轨
  img.fillRect(
    image,
    x1: 0,
    y1: 18,
    x2: w,
    y2: 24,
    color: img.ColorRgb8(204, 209, 213),
  );
  // 两张单据纸，第一张左上角有琥珀记号
  for (final x in [16, 186]) {
    img.fillRect(
      image,
      x1: x,
      y1: 22,
      x2: x + 158,
      y2: 104,
      color: img.ColorRgb8(255, 253, 246),
    );
    img.fillRect(
      image,
      x1: x + 10,
      y1: 34,
      x2: x + 90,
      y2: 40,
      color: img.ColorRgb8(90, 86, 80),
    );
    img.fillRect(
      image,
      x1: x + 10,
      y1: 50,
      x2: x + 140,
      y2: 58,
      color: img.ColorRgb8(30, 30, 30),
    );
    img.fillRect(
      image,
      x1: x + 10,
      y1: 80,
      x2: x + 74,
      y2: 96,
      color: img.ColorRgb8(255, 159, 28),
      radius: 3,
    );
    img.fillRect(
      image,
      x1: x + 82,
      y1: 80,
      x2: x + 146,
      y2: 96,
      color: img.ColorRgb8(227, 230, 233),
      radius: 3,
    );
  }
  img.fillRect(
    image,
    x1: 16,
    y1: 26,
    x2: 19,
    y2: 46,
    color: img.ColorRgb8(255, 159, 28),
  );
  // 搁板与几行会话
  img.fillRect(
    image,
    x1: 16,
    y1: 124,
    x2: w - 16,
    y2: 228,
    color: img.ColorRgb8(255, 255, 255),
    radius: 8,
  );
  for (var i = 0; i < 3; i++) {
    final y = 136 + i * 32;
    img.fillCircle(
      image,
      x: 30,
      y: y + 6,
      radius: 4,
      color: img.ColorRgb8(31, 122, 78),
    );
    img.fillRect(
      image,
      x1: 44,
      y1: y,
      x2: 44 + 150 - i * 30,
      y2: y + 7,
      color: img.ColorRgb8(30, 30, 30),
    );
    img.fillRect(
      image,
      x1: 44,
      y1: y + 12,
      x2: 140,
      y2: y + 17,
      color: img.ColorRgb8(150, 156, 162),
    );
    img.fillRect(
      image,
      x1: w - 64,
      y1: y + 2,
      x2: w - 28,
      y2: y + 9,
      color: img.ColorRgb8(31, 122, 78),
    );
  }
  return ImageData(mime: 'image/png', bytes: img.encodePng(image));
}

ConversationEntry _e(
  int index,
  EntryKind kind, {
  String text = '',
  bool thought = false,
  List<ConversationImage> images = const [],
  ToolCallView? tool,
  TaskNoteView? note,
}) => ConversationEntry(
  index: index,
  kind: kind,
  text: text,
  thought: thought,
  images: images,
  toolCall: tool,
  taskNote: note,
);

ConversationEntry _tool(
  int index,
  String id,
  String title,
  ToolKind kind,
  ToolStatus status, [
  List<ToolOutput> output = const [],
]) => _e(
  index,
  EntryKind.toolCall,
  tool: ToolCallView(
    id: id,
    title: title,
    kind: kind,
    status: status,
    output: output,
  ),
);

ToolOutput _text(String text) =>
    ToolOutput(kind: ToolOutputKind.text, text: text, truncated: false);

ToolOutput _terminal(String text, {int? exitCode, bool truncated = false}) =>
    ToolOutput(
      kind: ToolOutputKind.terminal,
      text: text,
      exitCode: exitCode,
      truncated: truncated,
    );

ToolOutput _diff(String path, String? before, String after) => ToolOutput(
  kind: ToolOutputKind.diff,
  text: '',
  diff: DiffView(path: path, oldText: before, newText: after),
  truncated: false,
);

const _model = ModelSelector(
  configId: 'model',
  currentValue: 'claude-sonnet-4-5',
  currentName: 'Claude Sonnet 4.5',
  currentProvider: 0,
  providers: [
    ModelProvider(
      id: 'anthropic',
      name: 'Anthropic',
      switchValue: 'claude-sonnet-4-5',
      selected: true,
      options: [],
    ),
    ModelProvider(
      id: 'openai',
      name: 'OpenAI',
      switchValue: 'gpt-5',
      selected: false,
      options: [],
    ),
    ModelProvider(id: 'local', name: 'Ollama', selected: false, options: []),
  ],
  options: [
    ChoiceOption(
      value: 'claude-sonnet-4-5',
      name: 'Claude Sonnet 4.5',
      selected: true,
    ),
    ChoiceOption(
      value: 'claude-opus-4-1',
      name: 'Claude Opus 4.1',
      selected: false,
    ),
    ChoiceOption(
      value: 'claude-haiku-4-5',
      name: 'Claude Haiku 4.5',
      selected: false,
    ),
  ],
);

const _configs = [
  ConfigOption(
    configId: 'thinking',
    name: '思考强度',
    currentName: '中',
    options: [
      ChoiceOption(value: 'low', name: '低', selected: false),
      ChoiceOption(value: 'medium', name: '中', selected: true),
      ChoiceOption(value: 'high', name: '高', selected: false),
    ],
  ),
  ConfigOption(
    configId: 'auto-compact',
    name: '自动压缩',
    description: '上下文快满时自动压缩历史',
    currentName: '开',
    options: [],
    boolean: true,
  ),
];

const _commands = [
  SlashCommand(name: 'compact', description: '压缩上下文'),
  SlashCommand(name: 'model', description: '切换模型'),
  SlashCommand(name: 'skill:lark-cli', description: '飞书个人身份 CLI'),
];

ConversationView _view({
  required String sessionId,
  required ConversationPhase phase,
  required List<ConversationEntry> entries,
  String? title,
  String? statusLine,
  int? turnStartedAtMs,
  int? lastTurnDurationMs,
  int? finalAnswerIndex,
  List<PlanStep> plan = const [],
  List<PermissionRequest> permissions = const [],
  Elicitation? elicitation,
  ContextUsage? usage,
}) => ConversationView(
  sessionId: sessionId,
  phase: phase,
  statusLine: statusLine,
  endReason: '',
  sessionTitle: title,
  turnStartedAtMs: turnStartedAtMs,
  lastTurnDurationMs: lastTurnDurationMs,
  plan: plan,
  pendingPermissions: permissions,
  pendingElicitation: elicitation,
  entries: entries,
  entriesOffset: 0,
  hasMoreBefore: false,
  finalAnswerIndex: finalAnswerIndex,
  supportsImage: true,
  availableCommands: _commands,
  usage: usage,
  model: _model,
  configOptions: _configs,
);

ConversationView _fleet(DateTime now) => _view(
  sessionId: 's-fleet',
  title: '多运行时连接状态机',
  phase: ConversationPhase.executingTool,
  statusLine: '运行 cargo test -p smelt-mobile',
  turnStartedAtMs: now
      .subtract(const Duration(minutes: 12, seconds: 41))
      .millisecondsSinceEpoch,
  usage: const ContextUsage(usedTokens: 84200, contextWindow: 200000),
  plan: const [
    PlanStep(
      content: '读懂现有 ConnectionHub 与 RuntimeSource',
      status: PlanStepStatus.completed,
    ),
    PlanStep(content: '新增 Fleet 与按机器的 Node', status: PlanStepStatus.completed),
    PlanStep(
      content: 'Conversations 改成按会话多订阅',
      status: PlanStepStatus.inProgress,
    ),
    PlanStep(content: '跑通 smelt-mobile 的全部测试', status: PlanStepStatus.pending),
  ],
  entries: [
    _e(
      0,
      EntryKind.user,
      text:
          '把移动端的连接从「当前运行时」改成同时连所有配对过的机器。先读一下 connection.rs，'
          '看看状态机怎么拆。截图是现在指挥台的样子。',
      images: const [
        ConversationImage(
          mime: 'image/png',
          original: ImageRef(entry: 0, digest: 'demo-pass-screenshot'),
        ),
      ],
    ),
    _e(
      1,
      EntryKind.assistant,
      thought: true,
      text:
          '先看 ConnectionHub 现在怎么拿「当前运行时」。如果 Catalog、Conversations、'
          'TerminalHub 都只经 RuntimeSource 取连接，那每台机器包一个 NodeSource 就够了，'
          '模块内部不用动。\n\n要确认的是 iroh endpoint 能不能共用。',
    ),
    _tool(
      2,
      'read-1',
      'crates/smelt-mobile/src/connection.rs',
      ToolKind.read,
      ToolStatus.completed,
      [
        _text(
          '```rust\n'
          'pub struct ConnectionHub {\n'
          '    state: Mutex<HubState>,\n'
          '    source: Arc<dyn RuntimeSource>,\n'
          '    events: broadcast::Sender<RuntimeStatus>,\n'
          '}\n\n'
          'impl ConnectionHub {\n'
          '    pub fn current(&self) -> Option<Arc<Session>> {\n'
          '        self.state.lock().session.clone()\n'
          '    }\n'
          '}\n'
          '```',
        ),
      ],
    ),
    _tool(
      3,
      'search-1',
      'rg "RuntimeSource" crates/smelt-mobile/src',
      ToolKind.search,
      ToolStatus.completed,
      [
        _text(
          'src/catalog.rs:41:    source: Arc<dyn RuntimeSource>,\n'
          'src/conversation.rs:58:    source: Arc<dyn RuntimeSource>,\n'
          'src/terminal.rs:33:    source: Arc<dyn RuntimeSource>,\n'
          'src/lib.rs:12:pub trait RuntimeSource: Send + Sync {',
        ),
      ],
    ),
    _e(
      4,
      EntryKind.assistant,
      text:
          '三个模块都只经 `RuntimeSource` 取连接，所以拆法很直接：\n\n'
          '1. 每台机器一个 `Node`，里面各自一套 `ConnectionHub` / `Catalog` / '
          '`Conversations` / `TerminalHub`；\n'
          '2. `NodeSource` 把 `current()` 指到本节点的连接；\n'
          '3. 全进程只留一个 iroh endpoint（同一把设备密钥只能挂一个）。\n\n'
          '```rust\n'
          'struct NodeSource {\n'
          '    hub: Arc<ConnectionHub>,\n'
          '    cache: Arc<Cache>,\n'
          '}\n\n'
          'impl RuntimeSource for NodeSource {\n'
          '    fn current(&self) -> Option<Arc<Session>> {\n'
          '        self.hub.current()\n'
          '    }\n'
          '}\n'
          '```\n\n'
          'endpoint 的形态按所有机器的 relay 设置决定，细节见 '
          '[iroh 的 RelayMode](https://docs.rs/iroh/latest/iroh/enum.RelayMode.html)。'
          '先写 `Fleet`。',
    ),
    _tool(
      5,
      'edit-1',
      'crates/smelt-mobile/src/fleet.rs',
      ToolKind.edit,
      ToolStatus.completed,
      [
        _diff(
          'crates/smelt-mobile/src/fleet.rs',
          null,
          'use std::collections::HashMap;\n'
              'use std::sync::Arc;\n\n'
              'use crate::node::Node;\n\n'
              '/// 进程级单例：每台登记的机器一个节点。\n'
              'pub struct Fleet {\n'
              '    nodes: RwLock<HashMap<String, Arc<Node>>>,\n'
              '    dialer: IrohDialer,\n'
              '}\n',
        ),
      ],
    ),
    _tool(
      6,
      'exec-1',
      'cargo test -p smelt-mobile fleet::',
      ToolKind.execute,
      ToolStatus.failed,
      [
        _terminal(
          '   Compiling smelt-mobile v0.10.0\n'
          '    Finished test [unoptimized + debuginfo] target(s) in 18.42s\n'
          '     Running unittests src/lib.rs\n\n'
          'running 6 tests\n'
          'test fleet::tests::adds_a_node_per_runtime ... ok\n'
          'test fleet::tests::forget_drops_the_node ... ok\n'
          'test fleet::tests::reconnect_keeps_other_nodes ... ok\n'
          'test fleet::tests::relay_shape_follows_registry ... FAILED\n'
          'test fleet::tests::pair_reuses_known_endpoint ... ok\n'
          'test fleet::tests::watch_two_sessions ... ok\n\n'
          'failures:\n\n'
          '---- fleet::tests::relay_shape_follows_registry stdout ----\n'
          "thread 'fleet::tests::relay_shape_follows_registry' panicked at src/fleet/tests.rs:88:9:\n"
          'assertion `left == right` failed\n'
          '  left: N0\n'
          ' right: Minimal { relay: "https://relay.example.com" }\n\n'
          'test result: FAILED. 5 passed; 1 failed',
          exitCode: 101,
        ),
      ],
    ),
    _e(
      7,
      EntryKind.assistant,
      text:
          '一个失败：所有机器都指定**同一个** relay 时应该用 Minimal 形态，'
          '我把「都没指定」和「都指定同一个」判反了。改一下 `shape_for`。',
    ),
    _tool(
      8,
      'edit-2',
      'crates/smelt-mobile/src/fleet.rs',
      ToolKind.edit,
      ToolStatus.completed,
      [
        _diff(
          'crates/smelt-mobile/src/fleet.rs',
          'fn shape_for(relays: &[Option<Url>]) -> EndpointShape {\n'
              '    let first = relays.first().cloned().flatten();\n'
              '    if relays.iter().all(|r| r.is_none()) {\n'
              '        return EndpointShape::Minimal { relay: first.unwrap() };\n'
              '    }\n'
              '    EndpointShape::N0\n'
              '}\n',
          'fn shape_for(relays: &[Option<Url>]) -> EndpointShape {\n'
              '    let first = relays.first().cloned().flatten();\n'
              '    match first {\n'
              '        Some(relay) if relays.iter().all(|r| r.as_ref() == Some(&relay)) => {\n'
              '            EndpointShape::Minimal { relay }\n'
              '        }\n'
              '        _ => EndpointShape::N0,\n'
              '    }\n'
              '}\n',
        ),
      ],
    ),
    _e(
      9,
      EntryKind.taskNote,
      text: '后台任务「cargo build --release」已结束，退出码 0',
      note: const TaskNoteView(
        kind: TaskNoteKind.backgroundTask,
        id: 'bg-1',
        title: 'cargo build --release',
        status: 'completed',
        exitCode: 0,
        outputTail:
            '   Compiling smelt-mobile v0.10.0\n'
            '   Compiling smelt v0.10.0\n'
            '    Finished release [optimized] target(s) in 2m 14s',
      ),
    ),
    _tool(
      10,
      'exec-2',
      'cargo test -p smelt-mobile',
      ToolKind.execute,
      ToolStatus.inProgress,
      [
        _terminal(
          '   Compiling smelt-mobile v0.10.0\n'
          '    Finished test [unoptimized + debuginfo] target(s) in 6.10s\n'
          '     Running unittests src/lib.rs\n\n'
          'running 48 tests\n'
          'test catalog::tests::sessions_are_grouped ... ok\n'
          'test conversation::tests::watch_replaces_sink ... ok',
        ),
      ],
    ),
  ],
);

/// 和指挥台那张单据（`tc-1`）是同一件事。
const _approval = PermissionRequest(
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
);

/// [approved]：指挥台上已经批了 `tc-1`，测试跑起来了，不再有待批。
ConversationView _markdown(DateTime now, {bool approved = false}) => _view(
  sessionId: 's-md',
  title: '修复 markdown 链接解析',
  phase: approved
      ? ConversationPhase.executingTool
      : ConversationPhase.awaitingApproval,
  turnStartedAtMs: now
      .subtract(const Duration(minutes: 3, seconds: 5))
      .millisecondsSinceEpoch,
  usage: const ContextUsage(usedTokens: 23900, contextWindow: 200000),
  permissions: approved ? const [] : const [_approval],
  entries: [
    _e(
      0,
      EntryKind.user,
      text:
          '链接地址里带括号时解析错了，比如 [文档](https://example.com/a_(b)) '
          '会把右括号当成链接结尾。修一下并补测试。',
    ),
    _tool(
      1,
      'read-md',
      'crates/smelt-acp-view/src/markdown.rs',
      ToolKind.read,
      ToolStatus.completed,
      [
        _text(
          'fn link_end(src: &str) -> Option<usize> {\n    src.find(\')\')\n}',
        ),
      ],
    ),
    _tool(
      2,
      'edit-md',
      'crates/smelt-acp-view/src/markdown.rs',
      ToolKind.edit,
      ToolStatus.completed,
      [
        _diff(
          'crates/smelt-acp-view/src/markdown.rs',
          'fn link_end(src: &str) -> Option<usize> {\n'
              "    src.find(')')\n"
              '}\n',
          'fn link_end(src: &str) -> Option<usize> {\n'
              '    let mut depth = 0usize;\n'
              '    for (i, ch) in src.char_indices() {\n'
              '        match ch {\n'
              "            '(' => depth += 1,\n"
              "            ')' if depth == 0 => return Some(i),\n"
              "            ')' => depth -= 1,\n"
              '            _ => {}\n'
              '        }\n'
              '    }\n'
              '    None\n'
              '}\n',
        ),
      ],
    ),
    _e(
      3,
      EntryKind.assistant,
      text: '改成按括号深度找链接结尾，并加了 `nested_parens_in_url` 测试。跑一下测试确认。',
    ),
    _tool(
      4,
      'tc-1',
      'cargo test -p smelt-acp-view',
      ToolKind.execute,
      approved ? ToolStatus.inProgress : ToolStatus.pending,
    ),
  ],
);

ConversationView _route(DateTime now) => _view(
  sessionId: 's-route',
  title: '接入 sing-box 路由',
  phase: ConversationPhase.waitingForUser,
  turnStartedAtMs: now
      .subtract(const Duration(minutes: 4))
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
  entries: [
    _e(0, EntryKind.user, text: '把 sing-box 的路由接进来，先支持按域名分流。'),
    _e(1, EntryKind.assistant, text: '订阅里自带一套规则，本地也可能有 overlay。动手前先确认以哪边为准。'),
  ],
);

ConversationView _empty(String sessionId) => ConversationView(
  sessionId: sessionId,
  phase: ConversationPhase.idle,
  endReason: '',
  plan: const [],
  pendingPermissions: const [],
  entries: const [],
  entriesOffset: 0,
  hasMoreBefore: false,
  supportsImage: true,
  availableCommands: const [],
  configOptions: const [],
);

/// 单据上印的上下文（会话最近几条），和真实内核一样取快照末尾 6 条。
List<ConversationEntry> demoRecentContext(String sessionId, DateTime now) {
  final entries = switch (sessionId) {
    's-md' => _markdown(now).entries,
    's-route' => _route(now).entries,
    's-relay' => [
      _e(0, EntryKind.user, text: '把自建 relay 部署到 build-01，用 443 端口。'),
      _tool(
        1,
        'relay-unit',
        '/etc/systemd/system/iroh-relay.service',
        ToolKind.edit,
        ToolStatus.completed,
      ),
      _tool(
        2,
        'relay-restart',
        'systemctl restart iroh-relay',
        ToolKind.execute,
        ToolStatus.failed,
        [_terminal('Job for iroh-relay.service failed.', exitCode: 1)],
      ),
      _e(
        3,
        EntryKind.assistant,
        text: '重启失败：443 端口已被 nginx 占用。要么停掉 nginx，要么把 relay 换到 8443。',
      ),
    ],
    _ => const <ConversationEntry>[],
  };
  return entries.length <= 6 ? entries : entries.sublist(entries.length - 6);
}
