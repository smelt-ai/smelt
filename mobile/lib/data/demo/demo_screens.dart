/// 演示模式的直达入口：`tool/demo_screenshots.sh` 把屏幕名写进 App 数据目录，
/// 启动后直接打开那一屏。模拟器没有点按命令，截图审查靠它走到每一屏。
///
/// 只在 `--dart-define=SMELT_DEMO=true` 时生效，生产构建走不到这里。
library;

import 'package:flutter/material.dart';

import '../../features/conversation/conversation_page.dart';
import '../stores.dart';
import '../../features/home_shell.dart';
import '../../features/launch/new_session_sheet.dart';
import '../../features/machines/machine_detail_page.dart';
import '../../features/pass/triage_page.dart';
import '../../features/terminal/terminal_page.dart';

abstract interface class DemoShell {
  BuildContext get context;
  BuildContext get innerContext;
  void selectTab(HomeTab tab);
  void openDetail(String key, WidgetBuilder builder);
}

void openDemoScreen(DemoShell shell, String screen) {
  final context = shell.context;
  switch (screen) {
    case 'sessions':
      shell.selectTab(HomeTab.sessions);
    case 'agents':
      shell.selectTab(HomeTab.agents);
    case 'settings':
      shell.selectTab(HomeTab.settings);
    case 'triage':
      Navigator.of(context).push(
        MaterialPageRoute<void>(
          fullscreenDialog: true,
          builder: (_) => TriagePage(initialIndex: 0, onOpenSession: (_, _) {}),
        ),
      );
    case 'conversation':
      shell.openDetail(
        'rt-mbp/s-fleet',
        (_) =>
            const ConversationPage(runtimeId: 'rt-mbp', sessionId: 's-fleet'),
      );
    case 'approval':
      shell.openDetail(
        'rt-mbp/s-md',
        (_) => const ConversationPage(runtimeId: 'rt-mbp', sessionId: 's-md'),
      );
    case 'stamped':
      // 在指挥台上批掉 tc-1，再打开那段对话：时间线里那条工具调用下面留着章。
      final model = AppScope.of(shell.innerContext);
      model.pass.decide('rt-mbp', 's-md', 'tc-1', StampKind.allowed);
      model.backend
          .respondApproval(
            'rt-mbp',
            's-md',
            toolCallId: 'tc-1',
            optionId: 'allow',
          )
          .then((_) {
            shell.openDetail(
              'rt-mbp/s-md',
              (_) => const ConversationPage(
                runtimeId: 'rt-mbp',
                sessionId: 's-md',
              ),
            );
          });
    case 'terminal':
      shell.openDetail(
        'rt-mbp/s-zsh',
        (_) => const TerminalPage(runtimeId: 'rt-mbp', sessionId: 's-zsh'),
      );
    case 'terminal-offline':
      // 机器在重连时打开它的终端：标题写机器的状态，不写「实时」。
      shell.openDetail(
        'rt-build/s-build',
        (_) => const TerminalPage(runtimeId: 'rt-build', sessionId: 's-build'),
      );
    case 'machine':
      shell.selectTab(HomeTab.settings);
      shell.openDetail(
        'machine/rt-mini',
        (_) => const MachineDetailPage(runtimeId: 'rt-mini'),
      );
    case 'launch':
      showNewSessionSheet(shell.innerContext);
  }
}
