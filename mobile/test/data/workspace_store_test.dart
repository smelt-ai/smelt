import 'package:flutter/widgets.dart' show Locale;
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/ui/agent_glyph.dart';

SessionSummary _session({
  required String agent,
  String? definition,
  String? profile,
}) => SessionSummary(
  id: 's',
  kind: SessionKind.acp,
  title: 't',
  phase: SessionPhase.idle,
  status: SessionStatus.idle,
  agent: agent,
  projectOrder: 0,
  updatedAt: 0,
  turnStartedAt: 0,
  agentDefinitionId: definition,
  profileId: profile,
);

void main() {
  // 会话页、指挥台、对话页共用这一份：智能体和自定义 Agent 写它们自己的名字。
  test('按工作区目录给智能体和自定义 Agent 的会话配名字', () async {
    final store = WorkspaceStore(DemoBackend());
    final l10n = lookupL10n(const Locale('en'));
    final custom = _session(agent: 'claude', profile: 'til');
    final product = _session(agent: 'pi', definition: 'def-assistant');
    final raw = _session(agent: 'claude');

    // 目录还没取到：退回引擎名，自定义 Agent 写「自定义」。
    expect(store.agentName('rt-mbp', custom), isNull);
    expect(store.agentDisplay('rt-mbp', custom), agentDisplayName('custom'));

    await store.load('rt-mbp');
    expect(store.agentName('rt-mbp', custom), 'Til');
    expect(store.agentName('rt-mbp', product), '工作助手');
    expect(store.agentName('rt-mbp', raw), isNull);
    expect(store.agentLabel(l10n, 'rt-mbp', custom), 'Til');
    expect(store.agentDisplay('rt-mbp', raw), agentDisplayName('claude'));
    expect(store.agentName('rt-mbp', _session(agent: 'claude', profile: 'gone')), isNull);
  });
}
