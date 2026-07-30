import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/acp_snapshot.dart';

void main() {
  group('AcpSnapshot', () {
    test('parses the current smeltd snapshot schema', () {
      final snapshot = AcpSnapshot.fromJson({
        'snapshot': {
          'entries_offset': 0,
          'entries': [
            {'User': 'Show the history'},
            {
              'UserWithImages': {
                'text': 'Inspect this screenshot',
                'images': [
                  {'mime': 'image/png', 'data_b64': 'aW1hZ2U='},
                ],
              },
            },
            {
              'Assistant': {'text': 'History restored.', 'thought': false},
            },
          ],
          'phase': 'Idle',
          'pending_permissions': [
            {
              'tool_call_id': 'tool-1',
              'question': 'Allow this command?',
              'options': [
                {
                  'option_id': 'allow-once',
                  'name': 'Allow once',
                  'kind': 'AllowOnce',
                },
              ],
              'details': {
                'kind': 'command',
                'command': 'cargo test',
                'cwd': '/workspace',
                'reason': 'Verify the change',
              },
            },
          ],
          'pending_elicitation': null,
          'status_line': null,
          'acp_session_id': 'agent-session-id',
          'supports_image': true,
          'available_commands': [
            ['compact', 'Compact conversation'],
          ],
          'usage': [120, 200000],
          'plan': {
            'entries': [
              {'content': 'Restore history', 'status': 'Completed'},
            ],
          },
          'model': {
            'config_id': 'model',
            'current_name': 'Codex',
            'options': [
              ['codex', 'Codex'],
            ],
          },
          'config_options': [
            {
              'config_id': 'mode',
              'name': 'Permission mode',
              'description': 'Controls tool approvals',
              'current_name': 'Default',
              'options': [
                ['default', 'Default'],
                ['full', 'Full access'],
              ],
            },
          ],
          'turn_started_at_ms': 1000,
          'last_turn_duration_ms': 2500,
          'completed_unread': false,
          'should_persist': true,
        },
      });

      expect(snapshot.entries, hasLength(3));
      expect((snapshot.entries.first as AcpEntryUser).text, 'Show the history');
      final imageEntry = snapshot.entries[1] as AcpEntryUserWithImages;
      expect(imageEntry.text, 'Inspect this screenshot');
      expect(imageEntry.images.single.mimeType, 'image/png');
      expect(
        (snapshot.entries.last as AcpEntryAssistant).text,
        'History restored.',
      );
      expect(snapshot.acpSessionId, 'agent-session-id');
      expect(snapshot.pendingPermissions.single.toolCallId, 'tool-1');
      final details = snapshot.pendingPermissions.single.details;
      expect(details, isA<ApprovalDetailsCommand>());
      expect((details as ApprovalDetailsCommand).command, 'cargo test');
      expect(snapshot.usage?.usedTokens, 120);
      expect(snapshot.usage?.contextWindow, 200000);
      expect(snapshot.plan?.steps.single.title, 'Restore history');
      expect(snapshot.plan?.steps.single.status, 'Completed');
      expect(snapshot.model?.configId, 'model');
      expect(snapshot.configOptions.single.configId, 'mode');
      expect(snapshot.turnStartedAtMs, 1000);
      expect(snapshot.lastTurnDurationMs, 2500);
    });

    test('replaces the snapshot tail at entries_offset', () {
      final original = AcpSnapshot(
        entries: const [
          AcpEntryUser(text: 'first'),
          AcpEntryAssistant(text: 'old answer'),
          AcpEntryUser(text: 'old tail'),
        ],
        phase: const AcpPhaseRunning(),
      );
      final update = AcpSnapshot(
        entriesOffset: 2,
        entries: const [AcpEntryAssistant(text: 'new tail')],
        phase: const AcpPhaseIdle(),
      );

      final merged = original.merge(update);

      expect(merged.entries, hasLength(3));
      expect((merged.entries[0] as AcpEntryUser).text, 'first');
      expect((merged.entries[1] as AcpEntryAssistant).text, 'old answer');
      expect((merged.entries[2] as AcpEntryAssistant).text, 'new tail');
      expect(merged.phase, isA<AcpPhaseIdle>());
    });

    test('drops an untrusted prefix when entries_offset is out of range', () {
      final original = AcpSnapshot(
        entries: const [AcpEntryUser(text: 'local entry')],
        phase: const AcpPhaseRunning(),
      );
      final update = AcpSnapshot(
        entriesOffset: 3,
        entries: const [AcpEntryAssistant(text: 'server entry')],
        phase: const AcpPhaseIdle(),
      );

      final merged = original.merge(update);

      expect(merged.entries, hasLength(1));
      expect((merged.entries.single as AcpEntryAssistant).text, 'server entry');
    });

    test('parses elicitation fields and selected options', () {
      final snapshot = AcpSnapshot.fromJson({
        'snapshot': {
          'entries': [],
          'phase': 'AwaitingChoice',
          'pending_elicitation': {
            'message': 'Choose a notification channel',
            'fields': [
              {
                'key': 'question_0',
                'title': 'Channel',
                'kind': {
                  'Select': [
                    {'label': 'Gitea Issue'},
                    {'label': 'Bark'},
                  ],
                },
              },
            ],
            'chosen': {
              '0': [1],
            },
            'text_values': <String, dynamic>{},
          },
        },
      });

      final elicitation = snapshot.pendingElicitation!;
      expect(elicitation.message, 'Choose a notification channel');
      expect(elicitation.chosen[0], [1]);
      final kind = elicitation.fields.single.kind as ElicitationSelect;
      expect(kind.options.map((option) => option.label), [
        'Gitea Issue',
        'Bark',
      ]);
    });
  });
}
