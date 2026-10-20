import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/stores.dart';

SessionSummary _session(String id) => SessionSummary(
  id: id,
  kind: SessionKind.acp,
  title: id,
  phase: SessionPhase.idle,
  status: SessionStatus.idle,
  agent: 'claude',
  projectOrder: 0,
  updatedAt: 0,
);

void main() {
  group('CatalogStore', () {
    test('按机器分开存会话列表，提醒带上机器 id', () async {
      final store = CatalogStore();
      final attention = <(String, AttentionItem)>[];
      final sub = store.attention.listen(attention.add);

      store.apply(
        CatalogEvent(
          runtimeId: 'a',
          kind: CatalogEventKind.sessions,
          sessions: SessionList(sessions: [_session('s1')], cached: false),
        ),
      );
      store.apply(
        CatalogEvent(
          runtimeId: 'b',
          kind: CatalogEventKind.sessions,
          sessions: SessionList(sessions: [_session('s1')], cached: true),
        ),
      );
      store.apply(
        const CatalogEvent(
          runtimeId: 'b',
          kind: CatalogEventKind.attention,
          attention: AttentionItem(
            sessionId: 's1',
            title: 't',
            message: 'm',
            kind: AttentionKind.approval,
          ),
        ),
      );
      await Future<void>.delayed(Duration.zero);

      expect(store.list('a')!.cached, isFalse);
      expect(store.list('b')!.cached, isTrue);
      expect(store.session('a', 's1'), isNotNull);
      expect(store.session('a', 'missing'), isNull);
      expect(attention.single.$1, 'b');

      store.forget('a');
      expect(store.list('a'), isNull);
      await sub.cancel();
      store.dispose();
    });
  });

  group('Preferences', () {
    test('认不出的版本、主题名和字号回到默认值', () {
      expect(
        Preferences.fromJson({'version': 99, 'themeMode': 'dark'}),
        const Preferences(),
      );
      final p = Preferences.fromJson({
        'version': Preferences.version,
        'themeMode': 'bogus',
        'terminalFontSize': 7,
      });
      expect(p.themeMode, ThemeMode.system);
      expect(p.terminalFontSize, Preferences.defaultTerminalFontSize);
    });

    test('落盘后能读回', () async {
      final dir = await Directory.systemTemp.createTemp('smelt-prefs');
      addTearDown(() => dir.delete(recursive: true));
      final store = await PreferencesStore.load(
        directoryProvider: () async => dir,
      );
      store.update(
        const Preferences(themeMode: ThemeMode.dark, terminalFontSize: 16),
      );
      // 写是排队的：再读一次会等它落盘。
      final again = await PreferencesStore.load(
        directoryProvider: () async => dir,
      );
      await Future<void>.delayed(const Duration(milliseconds: 50));
      final reread = await PreferencesStore.load(
        directoryProvider: () async => dir,
      );
      expect(reread.value.themeMode, ThemeMode.dark);
      expect(reread.value.terminalFontSize, 16);
      again.dispose();
      reread.dispose();
      store.dispose();
    });
  });

  group('PassStore', () {
    test('记下的章可以按键取回', () {
      final store = PassStore();
      store.stamp('a/s1', StampKind.denied);
      expect(store.stamps['a/s1']!.kind, StampKind.denied);
      store.dispose();
    });
  });
}
