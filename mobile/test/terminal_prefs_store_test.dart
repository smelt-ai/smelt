import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/pages/terminal_session_page.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/terminal_prefs_store.dart';
import 'package:xterm/xterm.dart';

const _session = SessionSummary(
  id: 'terminal-1',
  kind: SessionKind.terminal,
  title: 'Shell',
  phase: 'running',
  agent: 'terminal',
);

class _FakeTerminalPrefsStore implements TerminalPrefsStore {
  _FakeTerminalPrefsStore([this._prefs = const TerminalPrefs()]);

  TerminalPrefs _prefs;
  int saves = 0;

  TerminalPrefs get current => _prefs;

  @override
  Future<TerminalPrefs> load() async => _prefs;

  @override
  Future<void> save(TerminalPrefs prefs) async {
    saves++;
    _prefs = prefs;
  }
}

class _ExplodingStore implements TerminalPrefsStore {
  @override
  Future<TerminalPrefs> load() async => throw StateError('no disk');

  @override
  Future<void> save(TerminalPrefs prefs) async => throw StateError('no disk');
}

void main() {
  test('a font size outside the known steps degrades to the default', () {
    // 越界或损坏的值不该把终端渲染成 0.5pt。
    expect(
      TerminalPrefs.fromJson({'fontSize': 0.5}).fontSize,
      TerminalPrefs.defaultFontSize,
    );
    expect(
      TerminalPrefs.fromJson({'fontSize': 'big'}).fontSize,
      TerminalPrefs.defaultFontSize,
    );
    expect(
      TerminalPrefs.fromJson(const {}).fontSize,
      TerminalPrefs.defaultFontSize,
    );
  });

  test('a known step round-trips through json', () {
    const prefs = TerminalPrefs(fontSize: 18);
    expect(TerminalPrefs.fromJson(prefs.toJson()), prefs);
  });

  test('the default matches the size that used to be hard-coded', () {
    // 老用户升级后看到的字号不该变。
    expect(TerminalPrefs.defaultFontSize, 13);
    expect(const TerminalPrefs().fontSize, 13);
  });

  test('every offered step is accepted by the parser', () {
    for (final size in TerminalPrefs.steps) {
      expect(TerminalPrefs.fromJson({'fontSize': size}).fontSize, size);
    }
  });

  testWidgets('the terminal renders at the persisted font size', (
    tester,
  ) async {
    final store = _FakeTerminalPrefsStore(const TerminalPrefs(fontSize: 18));
    await tester.pumpWidget(
      MaterialApp(
        home: TerminalSessionPage(session: _session, prefsStore: store),
      ),
    );
    await tester.pump();

    expect(
      tester.widget<TerminalView>(find.byType(TerminalView)).textStyle.fontSize,
      18,
    );
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('picking a size applies it and writes it back', (tester) async {
    final store = _FakeTerminalPrefsStore();
    await tester.pumpWidget(
      MaterialApp(
        home: TerminalSessionPage(session: _session, prefsStore: store),
      ),
    );
    await tester.pump();

    await tester.tap(find.text('Aa'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('16 pt').last);
    await tester.pumpAndSettle();

    expect(
      tester.widget<TerminalView>(find.byType(TerminalView)).textStyle.fontSize,
      16,
    );
    expect(store.current.fontSize, 16);
    expect(store.saves, 1);
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('a store that cannot be read leaves the default in place', (
    tester,
  ) async {
    // 拿不到磁盘（测试环境本来就没有 path_provider 的平台通道）时，终端仍要能打开。
    await tester.pumpWidget(
      MaterialApp(
        home: TerminalSessionPage(
          session: _session,
          prefsStore: _ExplodingStore(),
        ),
      ),
    );
    await tester.pump();

    expect(
      tester.widget<TerminalView>(find.byType(TerminalView)).textStyle.fontSize,
      TerminalPrefs.defaultFontSize,
    );
    await tester.pumpWidget(const SizedBox.shrink());
  });

  test('the fake store records saves so the page can be asserted on', () async {
    final store = _FakeTerminalPrefsStore();
    await store.save(const TerminalPrefs(fontSize: 16));
    expect(store.saves, 1);
    expect((await store.load()).fontSize, 16);
    expect(store.current.fontSize, 16);
  });
}
