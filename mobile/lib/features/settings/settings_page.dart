/// 设置：机器（顺序、详情、配对新机器）、外观、终端、关于。
///
/// 机器的一切状态都从 [FleetStore] 读，偏好从 [PreferencesStore] 读；这里只把它们摆出来，
/// 改动直接写回去，不自己另存一份。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/preferences.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../theme/terminal_theme.dart';
import '../../ui/adaptive.dart';
import '../../ui/page.dart';
import '../../ui/shelf.dart';
import '../../ui/station.dart';
import '../machines/machine_detail_page.dart';
import '../machines/machine_text.dart';
import '../machines/pairing.dart';

/// 机器详情在 [DetailHost] 里的键。
String machineDetailKey(String runtimeId) => 'machine/$runtimeId';

class SettingsPage extends StatefulWidget {
  const SettingsPage({super.key});

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  /// 拖完还没等到机队流推回新顺序时，先按拖完的样子摆，免得松手那一下列表弹回原位。
  /// 记下拖动时看到的是哪一份机队：新的一份到了（不论顺序是否如愿）就以内核为准。
  (FleetView, List<String>)? _pendingOrder;

  List<FleetRuntime> _ordered(FleetView fleet) {
    final pending = _pendingOrder;
    if (pending == null || !identical(pending.$1, fleet)) {
      return fleet.runtimes;
    }
    final byId = {for (final r in fleet.runtimes) r.id: r};
    return [for (final id in pending.$2) ?byId[id]];
  }

  Future<void> _move(FleetView fleet, int from, int to) async {
    final model = AppScope.of(context);
    final messenger = ScaffoldMessenger.of(context);
    final ids = [for (final r in _ordered(fleet)) r.id];
    final id = ids.removeAt(from);
    ids.insert(to, id);
    setState(() => _pendingOrder = (fleet, ids));
    try {
      await model.backend.moveRuntime(id, to);
    } catch (error) {
      // 没挪成：丢掉乐观顺序，回到内核的顺序，并说一声。
      if (mounted) setState(() => _pendingOrder = null);
      messenger.showSnackBar(SnackBar(content: Text(describeError(error))));
    }
  }

  @override
  Widget build(BuildContext context) {
    final model = AppScope.of(context);
    final l10n = L10n.of(context);
    return ListenableBuilder(
      listenable: Listenable.merge([model.fleet, model.prefs]),
      builder: (context, _) {
        final fleet = model.fleet.value;
        final prefs = model.prefs.value;
        return GalleyPage(
          title: l10n.settingsTitle,
          onRefresh: fleet.runtimes.isEmpty
              ? null
              : () => model.backend.verifyConnections(),
          slivers: [
            SliverToBoxAdapter(
              child: _MachinesSection(
                runtimes: _ordered(fleet),
                onMove: (from, to) => _move(fleet, from, to),
              ),
            ),
            SliverToBoxAdapter(
              child: _AppearanceSection(
                mode: prefs.themeMode,
                onChanged: (mode) =>
                    model.prefs.update(prefs.copyWith(themeMode: mode)),
              ),
            ),
            SliverToBoxAdapter(
              child: _TerminalSection(
                size: prefs.terminalFontSize,
                onChanged: (size) {
                  HapticFeedback.selectionClick();
                  model.prefs.update(prefs.copyWith(terminalFontSize: size));
                },
              ),
            ),
            const SliverToBoxAdapter(child: _AboutSection()),
          ],
        );
      },
    );
  }
}

class _MachinesSection extends StatelessWidget {
  const _MachinesSection({required this.runtimes, required this.onMove});

  final List<FleetRuntime> runtimes;
  final void Function(int from, int to) onMove;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    // 在这里取 DetailHost，不在行里取：拖动时那一行的替身画在根 Overlay 里，
    // 那里不在 DetailHost 下面。
    final host = DetailHost.of(context);
    final pairRow = ShelfRow(
      leading: Icon(LucideIcons.scanLine, size: Strokes.icon, color: c.ink),
      title: Text(l10n.settingsPairNew),
      titleStyle: t.body,
      onTap: () => startPairing(context),
    );

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ShelfHeader(title: l10n.settingsMachines, first: true),
        Shelf(
          children: [
            if (runtimes.isNotEmpty)
              ReorderableListView.builder(
                shrinkWrap: true,
                physics: const NeverScrollableScrollPhysics(),
                // 不要默认的拖动把手：整行长按就能拖，行尾留给系统图标和箭头。
                buildDefaultDragHandles: false,
                itemCount: runtimes.length,
                onReorderItem: onMove,
                proxyDecorator: (child, index, animation) => Material(
                  color: c.shelf,
                  elevation: Space.sm,
                  shadowColor: c.shadow,
                  borderRadius: const BorderRadius.all(Radii.control),
                  child: child,
                ),
                itemBuilder: (context, index) {
                  final runtime = runtimes[index];
                  return ReorderableDelayedDragStartListener(
                    key: ValueKey(runtime.id),
                    index: index,
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        if (index > 0)
                          Padding(
                            padding: const EdgeInsetsDirectional.only(
                              start: Space.lg,
                            ),
                            child: Divider(
                              height: Strokes.hairline,
                              color: c.hairline,
                            ),
                          ),
                        _MachineRow(
                          runtime: runtime,
                          selected:
                              host.selectedKey == machineDetailKey(runtime.id),
                          onOpen: () => host.open(
                            machineDetailKey(runtime.id),
                            (_) => MachineDetailPage(runtimeId: runtime.id),
                          ),
                        ),
                      ],
                    ),
                  );
                },
              ),
            pairRow,
          ],
        ),
        if (runtimes.isEmpty)
          _Footnote(text: l10n.settingsNoMachines)
        else if (runtimes.length > 1)
          _Footnote(text: l10n.settingsReorderHint),
      ],
    );
  }
}

class _MachineRow extends StatelessWidget {
  const _MachineRow({
    required this.runtime,
    required this.selected,
    required this.onOpen,
  });

  final FleetRuntime runtime;

  /// 平板上它的详情正开在右栏。
  final bool selected;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final os = runtime.record.os;
    return ColoredBox(
      color: selected ? c.control : Colors.transparent,
      child: ShelfRow(
        leading: StationTape(color: c.station(runtime.record.accent)),
        title: Text(runtime.name),
        subtitle: Text(
          linkLabel(l10n, runtime.status),
          style: t.meta.copyWith(
            color: runtime.needsAttention ? c.danger : c.inkMuted,
          ),
        ),
        trailing: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Semantics(
              label: osLabel(l10n, os),
              child: Icon(
                osIcon(os),
                size: Strokes.iconSmall,
                color: c.inkMuted,
              ),
            ),
            const SizedBox(width: Space.sm),
            Icon(
              LucideIcons.chevronRight,
              size: Strokes.iconSmall,
              color: c.inkFaint,
            ),
          ],
        ),
        semanticLabel: l10n.settingsOpenMachine(runtime.name),
        onTap: onOpen,
      ),
    );
  }
}

class _AppearanceSection extends StatelessWidget {
  const _AppearanceSection({required this.mode, required this.onChanged});

  final ThemeMode mode;
  final ValueChanged<ThemeMode> onChanged;

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ShelfHeader(title: l10n.settingsAppearance),
        Shelf(
          children: [
            Padding(
              padding: const EdgeInsets.all(Space.md),
              child: SegmentedButton<ThemeMode>(
                showSelectedIcon: false,
                expandedInsets: EdgeInsets.zero,
                segments: [
                  ButtonSegment(
                    value: ThemeMode.system,
                    icon: const Icon(LucideIcons.sunMoon),
                    label: Text(l10n.settingsThemeSystem),
                  ),
                  ButtonSegment(
                    value: ThemeMode.light,
                    icon: const Icon(LucideIcons.sun),
                    label: Text(l10n.settingsThemeLight),
                  ),
                  ButtonSegment(
                    value: ThemeMode.dark,
                    icon: const Icon(LucideIcons.moon),
                    label: Text(l10n.settingsThemeDark),
                  ),
                ],
                selected: {mode},
                onSelectionChanged: (selection) => onChanged(selection.first),
              ),
            ),
          ],
        ),
      ],
    );
  }
}

/// 终端字号：只在 [Preferences.terminalFontSteps] 的档位间一步一步走。字号一变终端就要
/// 重算行列并向远端发 resize，连续滑杆等于对着远端狂发 resize。下方用终端自己的底色和
/// 字体预览这一档的实际大小。
class _TerminalSection extends StatelessWidget {
  const _TerminalSection({required this.size, required this.onChanged});

  final double size;
  final ValueChanged<double> onChanged;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    const steps = Preferences.terminalFontSteps;
    // 存下来的字号总在档位里（Preferences 读入时校验过），找不到就当默认档。
    final found = steps.indexOf(size);
    final index = found >= 0
        ? found
        : steps.indexOf(Preferences.defaultTerminalFontSize);
    final terminal = SmeltTerminalTheme.dark;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ShelfHeader(title: l10n.settingsTerminal),
        Shelf(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.lg,
                Space.xs,
                Space.xs,
                Space.xs,
              ),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      l10n.settingsTerminalFontSize,
                      style: t.body.copyWith(color: c.ink),
                    ),
                  ),
                  IconButton(
                    tooltip: l10n.settingsTerminalFontSmaller,
                    icon: const Icon(LucideIcons.minus),
                    onPressed: index > 0
                        ? () => onChanged(steps[index - 1])
                        : null,
                  ),
                  Semantics(
                    liveRegion: true,
                    child: Text(
                      l10n.settingsTerminalFontValue(steps[index].round()),
                      style: t.monoTime.copyWith(color: c.ink),
                    ),
                  ),
                  IconButton(
                    tooltip: l10n.settingsTerminalFontLarger,
                    icon: const Icon(LucideIcons.plus),
                    onPressed: index < steps.length - 1
                        ? () => onChanged(steps[index + 1])
                        : null,
                  ),
                ],
              ),
            ),
            Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.lg,
                Space.xs,
                Space.lg,
                Space.lg,
              ),
              child: ExcludeSemantics(
                child: ClipRRect(
                  borderRadius: const BorderRadius.all(Radii.control),
                  child: ColoredBox(
                    color: terminal.background,
                    child: Padding(
                      padding: const EdgeInsets.symmetric(
                        horizontal: Space.md,
                        vertical: Space.sm,
                      ),
                      child: Text(
                        l10n.settingsTerminalPreview,
                        maxLines: 1,
                        overflow: TextOverflow.clip,
                        softWrap: false,
                        style: t.code.copyWith(
                          fontSize: steps[index],
                          height: SmeltTerminalTheme.lineHeight,
                          color: terminal.foreground,
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ],
        ),
      ],
    );
  }
}

class _AboutSection extends StatelessWidget {
  const _AboutSection();

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ShelfHeader(title: l10n.settingsAbout),
        Shelf(
          children: [
            // 版本号要等共享层提供（不为它加 package_info 依赖），先只写 App 名。
            ShelfRow(
              title: Text(l10n.appName),
              subtitle: Text(
                l10n.settingsAboutTagline,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
              ),
            ),
            if (AppScope.of(context).deviceId case final id?)
              ShelfRow(
                leading: Icon(
                  LucideIcons.smartphone,
                  size: Strokes.icon,
                  color: c.ink,
                ),
                title: Text(l10n.settingsDeviceId),
                titleStyle: t.body,
                subtitle: Text(
                  id,
                  style: t.code.copyWith(color: c.inkMuted),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
                trailing: Icon(
                  LucideIcons.copy,
                  size: Strokes.iconSmall,
                  color: c.inkFaint,
                ),
                // 电脑端的设备列表里要认出、撤销这台手机时对照用。
                onTap: () {
                  Clipboard.setData(ClipboardData(text: id));
                  ScaffoldMessenger.of(
                    context,
                  ).showSnackBar(SnackBar(content: Text(l10n.commonCopied)));
                },
              ),
            ShelfRow(
              leading: Icon(
                LucideIcons.scrollText,
                size: Strokes.icon,
                color: c.ink,
              ),
              title: Text(l10n.settingsLicenses),
              titleStyle: t.body,
              trailing: Icon(
                LucideIcons.chevronRight,
                size: Strokes.iconSmall,
                color: c.inkFaint,
              ),
              // 走详情去处：平板上进右侧详情栏，手机上压栈，和机器详情一致。
              onTap: () => DetailHost.of(context).open(
                'licenses',
                (_) => LicensePage(applicationName: l10n.appName),
              ),
            ),
          ],
        ),
      ],
    );
  }
}

/// 搁板下的一行小字。
class _Footnote extends StatelessWidget {
  const _Footnote({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(
        Space.page + Space.xs,
        Space.sm,
        Space.page + Space.xs,
        0,
      ),
      child: Text(text, style: t.caption.copyWith(color: c.inkMuted)),
    );
  }
}
