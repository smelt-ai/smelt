import 'package:flutter/widgets.dart' show Locale;
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';

SessionSummary _session({required String agent, String? profile}) =>
    SessionSummary(
      id: 's',
      kind: SessionKind.acp,
      title: 't',
      phase: SessionPhase.idle,
      status: SessionStatus.idle,
      agent: agent,
      projectOrder: 0,
      updatedAt: 0,
      turnStartedAt: 0,
      profileId: profile,
    );

void main() {
  // 基于 Claude 的自定义 Agent 画成 Claude 会被当成 Claude：自定义 Agent 一律画通用图标。
  test('自定义 Agent 的会话画通用图标，裸引擎会话画本家', () {
    expect(sessionGlyphAgent(_session(agent: 'claude', profile: 'p')), 'custom');
    expect(sessionGlyphAgent(_session(agent: 'claude')), 'claude');
  });

  test('自定义 Agent 的选项画通用图标，其余按引擎', () {
    const custom = AcpAgentOption(
      id: 'profile:p',
      kind: 'claude',
      label: 'Til',
      hasHistory: true,
    );
    const raw = AcpAgentOption(
      id: 'claude',
      kind: 'claude',
      label: 'Claude Code',
      hasHistory: true,
    );
    expect(optionGlyphAgent(custom), 'custom');
    expect(optionGlyphAgent(raw), 'claude');
  });

  test('自定义 Agent 的会话「谁在干活」写自定义，不写底层引擎名', () {
    final l10n = lookupL10n(const Locale('en'));
    expect(
      sessionAgentLabel(l10n, _session(agent: 'claude', profile: 'p')),
      sessionAgentLabel(l10n, _session(agent: 'custom')),
    );
    expect(
      sessionAgentLabel(l10n, _session(agent: 'claude', profile: 'p')),
      isNot(sessionAgentLabel(l10n, _session(agent: 'claude'))),
    );
  });
}
