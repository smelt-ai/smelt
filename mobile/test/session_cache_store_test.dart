import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/acp_snapshot.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/session_cache_store.dart';

void main() {
  test('isolates cached sessions by pairing credentials', () async {
    final directory = await Directory.systemTemp.createTemp(
      'smelt-session-cache-',
    );
    addTearDown(() => directory.delete(recursive: true));
    final store = FileSessionCacheStore(
      directoryProvider: () async => directory,
    );
    final first = store.namespaceFor('smelt+iroh://desktop', 'token-a');
    final second = store.namespaceFor('smelt+iroh://desktop', 'token-b');
    expect(first, isNot(second));

    await store.saveSessions(first, const [
      SessionSummary(
        id: 'session-1',
        title: 'Cached task',
        phase: 'running',
        status: 'running',
        agent: 'codex',
        projectRoot: '/repo/smelt',
        projectTitle: 'smelt',
      ),
    ]);

    expect((await store.load(first)).sessions.single.id, 'session-1');
    expect((await store.load(second)).sessions, isEmpty);
  });

  test('round-trips a bounded rendered snapshot', () async {
    final directory = await Directory.systemTemp.createTemp(
      'smelt-session-cache-',
    );
    addTearDown(() => directory.delete(recursive: true));
    final store = FileSessionCacheStore(
      directoryProvider: () async => directory,
    );
    final namespace = store.namespaceFor('desktop', 'token');
    final entries = <AcpEntry>[
      for (var index = 0; index < 105; index++)
        AcpEntryUser(text: 'message $index'),
      const AcpEntryToolCall(
        id: 'tool-1',
        title: 'Run tests',
        kind: ToolKind.execute,
        status: ToolCallStatus.completed,
        output: [ToolOutputText(text: '51 passed')],
      ),
    ];
    final snapshot = AcpSnapshot(
      entries: entries,
      phase: const AcpPhaseAwaitingApproval(),
      pendingPermissions: const [
        PendingPermission(
          toolCallId: 'approval-1',
          question: 'Allow command?',
          options: [
            PermissionOption(
              optionId: 'allow',
              name: 'Allow once',
              kind: 'AllowOnce',
            ),
          ],
          details: ApprovalDetailsCommand(
            command: 'flutter test',
            cwd: '/repo/smelt',
          ),
        ),
      ],
      pendingElicitation: const PendingElicitation(
        message: 'Choose mode',
        fields: [
          ElicitationField(
            key: 'mode',
            title: 'Mode',
            required: true,
            kind: ElicitationSelect([
              ElicitationOption('Fast'),
              ElicitationOption('Careful'),
            ]),
          ),
        ],
        chosen: {
          0: [1],
        },
      ),
      historySessionId: 'history-1',
      usage: const AcpUsage(usedTokens: 120, contextWindow: 1000),
      plan: const AcpPlan(
        steps: [AcpPlanStep(title: 'Verify', status: 'completed')],
      ),
      model: const AcpModel(
        configId: 'model',
        currentName: 'Codex',
        options: [
          ['codex', 'Codex'],
        ],
      ),
    );

    await store.saveSnapshot(namespace, 'session-1', snapshot);
    final restored = (await store.load(namespace)).snapshots['session-1']!;

    expect(restored.entries, hasLength(100));
    expect(restored.entriesOffset, 6);
    expect(restored.entriesTotal, 106);
    final tool = restored.entries.last as AcpEntryToolCall;
    expect(tool.kind, ToolKind.execute);
    expect((tool.output.single as ToolOutputText).text, '51 passed');
    expect(restored.pendingPermissions.single.toolCallId, 'approval-1');
    expect(restored.pendingElicitation?.chosen[0], [1]);
    expect(restored.pendingElicitation?.fields.single.required, isTrue);
    expect(restored.usage?.usedTokens, 120);
    expect(restored.plan?.steps.single.title, 'Verify');
    expect(restored.model?.currentName, 'Codex');

    await store.deleteSnapshot(namespace, 'session-1');
    expect((await store.load(namespace)).snapshots, isEmpty);
  });
}
