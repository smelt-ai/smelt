import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../theme/theme.dart';
import '../../ui/buttons.dart';
import '../../ui/station.dart';
import 'terminal_stream.dart';

/// 浮在终端上的东西（选区条、「最新」按钮、补历史进度条）一律按深色画：终端固定深底，
/// 跟着 App 浅色模式画出来的墨黑按钮在深底上就看不见了。建一次留着，不在 build 里重建。
final ThemeData terminalOverlayTheme = buildGalleyTheme(Brightness.dark);

/// 导航栏标题：会话标题，下面一行小字「机器 · 连接状态」。
///
/// 标题和机器名在目录 / 机队变化时跟着变（改名、换标签色），只监听这两份内存状态。
class TerminalTitle extends StatelessWidget {
  const TerminalTitle({
    super.key,
    required this.runtimeId,
    required this.sessionId,
    required this.state,
  });

  final String runtimeId;
  final String sessionId;
  final TerminalStreamState state;

  @override
  Widget build(BuildContext context) {
    final model = AppScope.of(context);
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final streamLabel = switch (state) {
      TerminalStreamState.connecting => l10n.terminalStateConnecting,
      TerminalStreamState.connected => l10n.terminalStateConnected,
      TerminalStreamState.reconnecting => l10n.terminalStateReconnecting,
      TerminalStreamState.ended => l10n.terminalStateEnded,
    };
    return ListenableBuilder(
      listenable: Listenable.merge([model.catalog, model.fleet]),
      builder: (context, _) {
        final title = model.catalog.session(runtimeId, sessionId)?.title.trim();
        final runtime = model.fleet.runtime(runtimeId);
        // 机器本身没连上时，说机器的状态（重连中、离线、被移除）。只看终端流会在机器
        // 断着的时候还写「实时」。
        final stateLabel =
            runtime != null &&
                !runtime.online &&
                state != TerminalStreamState.ended
            ? linkLabel(l10n, runtime.status)
            : streamLabel;
        final meta = t.caption.copyWith(color: c.inkMuted);
        // 导航栏只有一行工具栏高，两行字在特大字号下会撑破；放大到一个上限为止（系统导航栏
        // 标题也是这样）。
        return MediaQuery.withClampedTextScaling(
          maxScaleFactor: 1.4,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                title == null || title.isEmpty ? l10n.kindTerminal : title,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
              Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  if (runtime != null) ...[
                    Flexible(
                      child: StationLabel(
                        name: runtime.name,
                        color: c.station(runtime.record.accent),
                        style: t.caption,
                      ),
                    ),
                    Text(' · ', style: meta),
                  ],
                  // 连接状态变了要读出来：重连、结束都是用户得知道的事。
                  Semantics(
                    liveRegion: true,
                    child: Text(stateLabel, style: meta, maxLines: 1),
                  ),
                ],
              ),
            ],
          ),
        );
      },
    );
  }
}

/// 终端上方的一条说明：重连中、会话结束、连不上。
///
/// 放在终端**上方**而不是浮在上面：它出现时终端让出高度、重算行数，整屏内容不会被
/// 它挡住最上面几行。
class TerminalNoticeBar extends StatelessWidget {
  const TerminalNoticeBar({super.key, required this.notice});

  final TerminalNotice notice;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final (icon, text, bg, fg) = switch (notice) {
      // 会话结束是正常收尾，不是故障：用中性底。
      TerminalNotice(closed: true) => (
        LucideIcons.circleSlash,
        l10n.terminalSessionEnded,
        c.control,
        c.ink,
      ),
      TerminalNotice(fatal: true, :final message) => (
        LucideIcons.circleAlert,
        message ?? l10n.terminalConnectionLost,
        c.dangerWash,
        c.danger,
      ),
      TerminalNotice(:final message) => (
        LucideIcons.wifiOff,
        message ?? l10n.terminalConnectionDropped,
        c.control,
        c.ink,
      ),
    };
    return Semantics(
      liveRegion: true,
      child: ColoredBox(
        color: bg,
        child: Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: Space.page,
            vertical: Space.sm,
          ),
          child: Row(
            children: [
              Icon(icon, size: Strokes.iconSmall, color: fg),
              const SizedBox(width: Space.sm),
              Expanded(
                child: Text(
                  text,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: t.meta.copyWith(color: fg),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 终端上方那条说明的内容。页面只存事件的事实，文案在画的时候按语言取。
@immutable
class TerminalNotice {
  const TerminalNotice.closed() : closed = true, fatal = true, message = null;

  const TerminalNotice.error(this.message, {required this.fatal})
    : closed = false;

  final bool closed;
  final bool fatal;

  /// 内核给的一句话；null 时用本地化的通用说法。
  final String? message;
}

/// 选区操作条：浮在终端底部（不进布局：多出来的高度会改行数，主屏 CLI 会因此重印
/// 整段对话）。复制是这里的主动作；选区正好是一个 http(s) 链接时多一个「打开链接」。
class TerminalSelectionBar extends StatelessWidget {
  const TerminalSelectionBar({
    super.key,
    required this.onCopy,
    required this.onSelectAll,
    this.onPaste,
    this.onOpen,
  });

  final VoidCallback onCopy;
  final VoidCallback onSelectAll;

  /// 能写的时候才有：会话结束、还没连上时不给粘贴。
  final VoidCallback? onPaste;
  final VoidCallback? onOpen;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    const gap = SizedBox(width: Space.xs);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: c.raised,
        border: Border.all(color: c.hairline),
        borderRadius: const BorderRadius.all(Radii.shelf),
        boxShadow: [BoxShadow(color: c.shadow, blurRadius: Space.lg)],
      ),
      child: SingleChildScrollView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.all(Space.xs),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            GalleyButton.solid(
              label: l10n.commonCopy,
              icon: LucideIcons.copy,
              onPressed: onCopy,
            ),
            gap,
            if (onOpen != null) ...[
              GalleyButton.quiet(
                label: l10n.terminalOpenLink,
                icon: LucideIcons.externalLink,
                onPressed: onOpen,
              ),
              gap,
            ],
            if (onPaste != null) ...[
              GalleyButton.quiet(
                label: l10n.terminalPaste,
                icon: LucideIcons.clipboardPaste,
                onPressed: onPaste,
              ),
              gap,
            ],
            GalleyButton.quiet(
              label: l10n.terminalSelectAll,
              icon: LucideIcons.textSelect,
              onPressed: onSelectAll,
            ),
          ],
        ),
      ),
    );
  }
}

/// 「最新」：视图离开了最新输出（往上翻了，或补完历史停在中间）时的出口。补完历史后
/// 用户停在几千行之外，手动拖回去不现实。
class TerminalJumpToLatest extends StatelessWidget {
  const TerminalJumpToLatest({super.key, required this.onPressed});

  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    return Semantics(
      hint: l10n.terminalJumpToLatestHint,
      child: GalleyButton.solid(
        label: l10n.terminalJumpToLatest,
        icon: LucideIcons.arrowDownToLine,
        onPressed: onPressed,
      ),
    );
  }
}

/// 字号档位菜单。终端是等宽网格，字号一变就要重算行列并向 PTY 发 resize，所以只给
/// 档位、不给滑杆。每一档用它自己的字号渲染，选之前就看得出差别。
class TerminalFontSizeButton extends StatelessWidget {
  const TerminalFontSizeButton({
    super.key,
    required this.value,
    required this.steps,
    required this.onSelected,
  });

  final double value;
  final List<double> steps;
  final ValueChanged<double> onSelected;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    return PopupMenuButton<double>(
      tooltip: l10n.terminalTextSize,
      icon: const Icon(LucideIcons.aLargeSmall),
      initialValue: value,
      onSelected: onSelected,
      itemBuilder: (context) => [
        for (final size in steps)
          PopupMenuItem<double>(
            value: size,
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    l10n.terminalTextSizePoints(size.round()),
                    style: t.code.copyWith(fontSize: size, color: c.ink),
                  ),
                ),
                if (size == value)
                  Icon(LucideIcons.check, size: Strokes.icon, color: c.ink),
              ],
            ),
          ),
      ],
    );
  }
}
