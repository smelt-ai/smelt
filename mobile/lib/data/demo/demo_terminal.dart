/// 终端页的演示数据（**合成**：命令、crate 名、耗时都是编的，只用来看界面长什么样）。
///
/// 附着后先推 `Ready`，再像真的一样一行一行推一段带 ANSI 颜色的 `cargo build` 输出，
/// 最后停在提示符上；之后键入的字原样回显（回车换行再出提示符）。不模拟 shell。
library;

import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import '../models.dart';

const _esc = '\x1b';
const _bold = '$_esc[1m';
const _boldGreen = '$_esc[1;32m';
const _boldYellow = '$_esc[1;33m';
const _boldBlue = '$_esc[1;34m';
const _cyan = '$_esc[36m';
const _dim = '$_esc[2m';
const _reset = '$_esc[0m';

const _prompt = '$_boldGreen~/code/smelt$_reset $_cyan(main)$_reset \$ ';

String _compiling(String crate, String version) =>
    '$_boldGreen   Compiling$_reset $crate v$version\r\n';

/// 一行一段：每段之间隔一小会儿推，看得出是在「跑」。
final _script = <String>[
  '${_prompt}cargo build --release\r\n',
  '$_boldGreen    Updating$_reset crates.io index\r\n',
  _compiling('proc-macro2', '1.0.95'),
  _compiling('unicode-ident', '1.0.18'),
  _compiling('serde', '1.0.219'),
  _compiling('tokio', '1.45.1'),
  _compiling('iroh', '0.35.0'),
  _compiling('rusqlite', '0.36.0'),
  _compiling(
    'smelt-protocol',
    '0.10.0 (/Users/demo/code/smelt/crates/smelt-protocol)',
  ),
  '${_boldYellow}warning$_reset$_bold: unused variable: `attempt`$_reset\r\n'
      '  $_boldBlue-->$_reset crates/smelt-mobile/src/connection.rs:212:13\r\n'
      '   $_boldBlue|$_reset\r\n'
      '${_boldBlue}212$_reset $_boldBlue|$_reset         let attempt = backoff.next();\r\n'
      '   $_boldBlue|$_reset             $_boldYellow^^^^^^^$_reset '
      '${_boldYellow}help: prefix it with an underscore: `_attempt`$_reset\r\n'
      '   $_boldBlue|$_reset\r\n'
      '   $_boldBlue=$_reset ${_bold}note$_reset: `#[warn(unused_variables)]` on by default\r\n'
      '\r\n',
  _compiling(
    'smelt-mobile',
    '0.10.0 (/Users/demo/code/smelt/crates/smelt-mobile)',
  ),
  '${_boldYellow}warning$_reset$_bold: `smelt-mobile` (lib) generated 1 warning$_reset\r\n',
  _compiling('smeltd', '0.10.0 (/Users/demo/code/smelt/crates/smeltd)'),
  '$_boldGreen    Finished$_reset `release` profile [optimized] target(s) in 1m 42s\r\n',
  '${_dim}docs: https://github.com/smelt-ai/smelt$_reset\r\n',
  _prompt,
];

const _lineInterval = Duration(milliseconds: 180);

/// 每个会话当前的演示附着。同一会话再次附着替换旧的（旧流结束），与内核一致。
final _attached = <String, StreamController<TerminalEvent>>{};

TerminalEvent _data(String text) => TerminalEvent(
  kind: TerminalEventKind.data,
  data: Uint8List.fromList(utf8.encode(text)),
  fatal: false,
);

Stream<TerminalEvent> demoTerminal(
  String sessionId,
  TerminalGeometry geometry,
) {
  final previous = _attached.remove(sessionId);
  if (previous != null) unawaited(previous.close());

  Timer? timer;
  late final StreamController<TerminalEvent> controller;
  controller = StreamController<TerminalEvent>(
    onListen: () {
      controller
        ..add(
          TerminalEvent(
            kind: TerminalEventKind.ready,
            ready: TerminalReady(
              cols: geometry.cols,
              rows: geometry.rows,
              replayBytes: 0,
              historyLines: 0,
              scrollbackLines: 0,
            ),
            fatal: false,
          ),
        )
        ..add(
          const TerminalEvent(
            kind: TerminalEventKind.replayComplete,
            fatal: false,
          ),
        );
      var next = 0;
      timer = Timer.periodic(_lineInterval, (t) {
        if (next >= _script.length) {
          t.cancel();
          return;
        }
        controller.add(_data(_script[next++]));
      });
    },
    // 页面退出（取消订阅）时停掉计时器：不留悬空的定时器。
    onCancel: () {
      timer?.cancel();
      if (identical(_attached[sessionId], controller)) {
        _attached.remove(sessionId);
      }
    },
  );
  _attached[sessionId] = controller;
  return controller.stream;
}

/// 回显，像行规程那样：回车换行后再给一个提示符，退格擦掉一格，^C 打个 `^C` 换行；
/// 方向键这类转义序列不回显（真 shell 也不会把它们原样打出来）。
void demoTerminalInput(String sessionId, String data) {
  final controller = _attached[sessionId];
  if (controller == null || controller.isClosed) return;
  if (data.startsWith(_esc)) return;
  final echo = data
      .replaceAll('\x03', '^C\r')
      .replaceAll('\x7f', '\b \b')
      .replaceAll('\r', '\r\n$_prompt');
  controller.add(_data(echo));
}
