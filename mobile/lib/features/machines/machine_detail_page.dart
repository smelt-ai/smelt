/// 一台机器的详情：改名、标签色、链路、重连、重新配对、忘记。
///
/// 数据全从 [FleetStore] 来，动作直接调 [Backend]，结果经机队流回到这里——页面不自己
/// 记一份机器状态，免得和内核对不上。
library;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart' hide ConnectionState;
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../../ui/shelf.dart';
import '../../ui/station.dart';
import 'machine_text.dart';
import 'pairing.dart';

class MachineDetailPage extends StatefulWidget {
  const MachineDetailPage({super.key, required this.runtimeId});

  final String runtimeId;

  @override
  State<MachineDetailPage> createState() => _MachineDetailPageState();
}

class _MachineDetailPageState extends State<MachineDetailPage> {
  bool _reconnecting = false;

  void _say(String message) {
    ScaffoldMessenger.of(context)
      ..clearSnackBars()
      ..showSnackBar(SnackBar(content: Text(message)));
  }

  Future<void> _rename(RuntimeRecord record) async {
    final model = AppScope.of(context);
    final l10n = L10n.of(context);
    final name = await showDialog<String>(
      context: context,
      builder: (_) => _RenameDialog(record: record),
    );
    if (name == null || name == record.name || !mounted) return;
    try {
      await model.backend.renameRuntime(record.runtimeId, name);
    } catch (error) {
      if (mounted) _say(l10n.machinesRenameFailed(describeError(error)));
    }
  }

  Future<void> _setAccent(RuntimeRecord record, int accent) async {
    if (accent == record.accent) return;
    final model = AppScope.of(context);
    final l10n = L10n.of(context);
    HapticFeedback.selectionClick();
    try {
      await model.backend.setRuntimeAccent(record.runtimeId, accent);
    } catch (error) {
      if (mounted) _say(l10n.machinesAccentFailed(describeError(error)));
    }
  }

  Future<void> _reconnect(FleetRuntime runtime) async {
    final model = AppScope.of(context);
    final l10n = L10n.of(context);
    setState(() => _reconnecting = true);
    try {
      await model.backend.reconnectRuntime(runtime.id);
      if (mounted) _say(l10n.machinesReconnected(runtime.name));
    } on SmeltError catch (error) {
      if (!mounted) return;
      switch (error.kind) {
        // 离线：内核已转入后台重连，说一句就好。
        case SmeltErrorKind.offline:
          _say(l10n.machinesReconnectOffline(runtime.name));
        // 被撤销、版本不兼容：页面顶部的说明会跟着机队状态出来，不再弹一遍。
        case SmeltErrorKind.unpaired ||
            SmeltErrorKind.incompatible ||
            SmeltErrorKind.cancelled:
          break;
        case SmeltErrorKind.invalidPairing || SmeltErrorKind.other:
          _say(l10n.machinesReconnectFailed(error.message));
      }
    } catch (error) {
      if (mounted) _say(l10n.machinesReconnectFailed(describeError(error)));
    } finally {
      if (mounted) setState(() => _reconnecting = false);
    }
  }

  Future<void> _forget(RuntimeRecord record) async {
    final model = AppScope.of(context);
    final l10n = L10n.of(context);
    final messenger = ScaffoldMessenger.of(context);
    final navigator = Navigator.of(context);
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        title: Text(l10n.machinesForgetTitle(record.name)),
        content: Text(l10n.machinesForgetBody(record.name)),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(dialogContext, false),
            child: Text(l10n.commonCancel),
          ),
          GalleyButton.danger(
            label: l10n.machinesForgetConfirm,
            onPressed: () => Navigator.pop(dialogContext, true),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    try {
      await model.backend.forgetRuntime(record.runtimeId);
    } catch (error) {
      if (mounted) _say(l10n.machinesForgetFailed(describeError(error)));
      return;
    }
    messenger
      ..clearSnackBars()
      ..showSnackBar(
        SnackBar(content: Text(l10n.machinesForgotten(record.name))),
      );
    // 手机上退回设置页；平板详情栏里这一页是栈底，退不了，就留着「已不在」的说明。
    if (mounted) await navigator.maybePop();
  }

  @override
  Widget build(BuildContext context) {
    final model = AppScope.of(context);
    return ValueListenableBuilder<FleetView>(
      valueListenable: model.fleet,
      builder: (context, fleet, _) {
        final runtime = model.fleet.runtime(widget.runtimeId);
        final c = GalleyColors.of(context);
        if (runtime == null) {
          return Scaffold(
            backgroundColor: c.ground,
            appBar: AppBar(),
            body: Center(
              child: EmptyNote(
                icon: LucideIcons.unlink,
                title: L10n.of(context).machinesGone,
              ),
            ),
          );
        }
        return Scaffold(
          backgroundColor: c.ground,
          appBar: AppBar(
            backgroundColor: c.ground,
            title: _Title(runtime: runtime),
          ),
          body: _body(context, runtime, relayMixed: relaySetupMixed(fleet)),
        );
      },
    );
  }

  Widget _body(
    BuildContext context,
    FleetRuntime runtime, {
    required bool relayMixed,
  }) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final record = runtime.record;
    final hostname = record.hostname?.trim();
    final revoked = runtime.status.error?.kind == SmeltErrorKind.unpaired;

    return ListView(
      padding: EdgeInsets.only(
        bottom: MediaQuery.paddingOf(context).bottom + Space.xxl,
      ),
      children: [
        if (runtime.needsAttention)
          _AttentionNote(
            text: revoked
                ? l10n.machinesRevokedBody(record.name)
                : l10n.machinesIncompatibleBody(record.name),
            // 被撤销只能重新配对；版本不兼容先升级再重连，重新配对是兜底。
            action: GalleyButton.solid(
              label: l10n.machinesRepair,
              icon: LucideIcons.scanLine,
              onPressed: () => startPairing(context),
            ),
          ),
        ShelfHeader(title: l10n.machinesName, first: !runtime.needsAttention),
        Shelf(
          children: [
            ShelfRow(
              title: Text(record.name),
              subtitle: hostname == null || hostname == record.name
                  ? null
                  : Text(l10n.machinesHostname(hostname)),
              trailing: Icon(
                LucideIcons.pencil,
                size: Strokes.iconSmall,
                color: c.inkFaint,
              ),
              semanticLabel: l10n.commonRename,
              onTap: () => _rename(record),
            ),
          ],
        ),
        ShelfHeader(title: l10n.machinesAccent),
        Shelf(
          children: [
            _AccentPicker(
              selected: record.accent,
              onSelected: (accent) => _setAccent(record, accent),
            ),
          ],
        ),
        _Footnote(text: l10n.machinesAccentFooter),
        ShelfHeader(title: l10n.machinesLink),
        Shelf(children: _linkRows(context, runtime)),
        // 中继设置混用时 iOS 上自建中继可能受系统代理影响（见 relaySetupMixed）。只在
        // iOS、且这台机器自己指定了中继时说：没指定中继的机器走默认预设，不受影响。
        if (relayMixed &&
            record.relayUrl != null &&
            defaultTargetPlatform == TargetPlatform.iOS)
          _Footnote(text: l10n.machinesRelayMixedNote),
        const SizedBox(height: Space.section),
        Shelf(
          children: [
            ShelfRow(
              leading: Icon(
                LucideIcons.refreshCw,
                size: Strokes.icon,
                color: c.ink,
              ),
              title: Text(
                _reconnecting
                    ? l10n.machinesReconnectBusy
                    : l10n.machinesReconnect,
              ),
              titleStyle: t.body,
              trailing: _reconnecting
                  ? const SizedBox.square(
                      dimension: Strokes.iconSmall,
                      child: CircularProgressIndicator.adaptive(strokeWidth: 2),
                    )
                  : null,
              onTap: _reconnecting ? null : () => _reconnect(runtime),
            ),
          ],
        ),
        const SizedBox(height: Space.section),
        Shelf(
          children: [
            ShelfRow(
              leading: Icon(
                LucideIcons.trash2,
                size: Strokes.icon,
                color: c.danger,
              ),
              title: Text(
                l10n.machinesForget,
                style: TextStyle(color: c.danger),
              ),
              titleStyle: t.body,
              onTap: () => _forget(record),
            ),
          ],
        ),
      ],
    );
  }

  List<Widget> _linkRows(BuildContext context, FleetRuntime runtime) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final record = runtime.record;
    final status = runtime.status;
    // 没连上时的路径和延迟是上一次的残值，画出来等于骗人：只在已连上时给。
    final link = status.state == ConnectionState.connected ? status.link : null;
    final rtt = link?.rttMs;
    // 被撤销 / 要升级时页面顶部已经用人话说了原因，不再列内核的原始错误。
    final error =
        status.state == ConnectionState.connected || runtime.needsAttention
        ? null
        : status.error;
    final os = record.os;
    final hostname = record.hostname?.trim();

    return [
      // 已连上时路径和延迟各占一行，状态行只说「在线」，不把同一件事说两遍。
      _InfoRow(
        label: l10n.machinesStatus,
        value: status.state == ConnectionState.connected
            ? l10n.machineOnline
            : linkLabel(l10n, status),
        valueColor: runtime.needsAttention ? c.danger : null,
      ),
      if (link != null)
        _InfoRow(
          label: l10n.machinesPath,
          value: switch (link.path) {
            LinkPath.lan => l10n.machinesPathLan,
            LinkPath.peerToPeer => l10n.machinesPathDirect,
            LinkPath.relay => l10n.machinesPathRelay,
            LinkPath.unknown => l10n.machinesPathUnknown,
          },
        ),
      if (rtt != null)
        _InfoRow(
          label: l10n.machinesRtt,
          value: l10n.machinesRttValue(rtt),
          // 延迟是测量值，用等宽数字；字号跟同排正文一致。
          valueStyle: t.monoTime.copyWith(fontSize: t.body.fontSize),
        ),
      if (error != null && error.message.trim().isNotEmpty)
        _InfoRow(label: l10n.machinesLastError, value: error.message.trim()),
      _InfoRow(
        label: l10n.machinesEndpoint,
        value: record.endpointId,
        valueStyle: t.code.copyWith(fontSize: t.meta.fontSize),
        stacked: true,
        copyLabel: l10n.machinesEndpointCopy,
      ),
      _InfoRow(
        label: l10n.machinesRelay,
        value: record.relayUrl ?? l10n.machinesRelayDefault,
        stacked: record.relayUrl != null,
      ),
      _InfoRow(
        label: l10n.machinesVersion,
        value: record.version.trim().isEmpty
            ? l10n.machinesVersionUnknown
            : record.version,
      ),
      _InfoRow(
        label: l10n.machinesSystem,
        value: [
          osLabel(l10n, os),
          if (hostname != null && hostname.isNotEmpty) hostname,
        ].join(' · '),
        leading: Icon(osIcon(os), size: Strokes.iconSmall, color: c.inkMuted),
      ),
    ];
  }
}

/// 标题：机器名前一小段它的标签色。
class _Title extends StatelessWidget {
  const _Title({required this.runtime});

  final FleetRuntime runtime;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        StationTape(color: c.station(runtime.record.accent), height: 16),
        const SizedBox(width: Space.sm),
        Flexible(
          child: Text(
            runtime.name,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
          ),
        ),
      ],
    );
  }
}

/// 被撤销 / 要升级时页面顶部的说明。不是单据（不是哪个 agent 在等你），只是一段话加一个
/// 动作，放在钢底上，用失败红的图标提示「这台停了」。
class _AttentionNote extends StatelessWidget {
  const _AttentionNote({required this.text, required this.action});

  final String text;
  final Widget action;

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
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Padding(
                padding: const EdgeInsets.only(top: Space.xxs),
                child: Icon(
                  LucideIcons.circleAlert,
                  size: Strokes.icon,
                  color: c.danger,
                ),
              ),
              const SizedBox(width: Space.sm),
              Expanded(
                child: Text(text, style: t.body.copyWith(color: c.ink)),
              ),
            ],
          ),
          const SizedBox(height: Space.md),
          action,
        ],
      ),
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

/// 链路详情的一行：左边名目，右边值。长值（endpoint、中继地址）[stacked] 时换到下一行，
/// 不截断——排查连接时被截掉的那半截往往就是要看的。
class _InfoRow extends StatelessWidget {
  const _InfoRow({
    required this.label,
    required this.value,
    this.valueStyle,
    this.valueColor,
    this.stacked = false,
    this.copyLabel,
    this.leading,
  });

  final String label;
  final String value;
  final TextStyle? valueStyle;
  final Color? valueColor;
  final bool stacked;

  /// 有它就能点按复制值，读屏念这句。
  final String? copyLabel;
  final Widget? leading;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    // 名目用正文墨色、值用次要灰：和系统设置里的「名目 — 值」一行同一个读法。
    final labelText = Text(label, style: t.body.copyWith(color: c.ink));
    final valueText = Text(
      value,
      textAlign: stacked ? TextAlign.start : TextAlign.end,
      style: (valueStyle ?? t.body).copyWith(color: valueColor ?? c.inkMuted),
    );

    final Widget content = stacked
        ? Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              labelText,
              const SizedBox(height: Space.xxs),
              valueText,
            ],
          )
        : Row(
            crossAxisAlignment: CrossAxisAlignment.baseline,
            textBaseline: TextBaseline.alphabetic,
            children: [
              labelText,
              const SizedBox(width: Space.lg),
              Expanded(
                child: Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  crossAxisAlignment: CrossAxisAlignment.center,
                  children: [
                    if (leading != null) ...[
                      leading!,
                      const SizedBox(width: Space.xs + Space.xxs),
                    ],
                    Flexible(child: valueText),
                  ],
                ),
              ),
            ],
          );

    final copyable = copyLabel != null;
    final row = ConstrainedBox(
      constraints: const BoxConstraints(minHeight: Space.touch),
      child: Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Space.lg,
          vertical: Space.md - Space.xxs,
        ),
        child: Row(
          children: [
            Expanded(child: content),
            if (copyable) ...[
              const SizedBox(width: Space.md),
              Icon(
                LucideIcons.copy,
                size: Strokes.iconSmall,
                color: c.inkFaint,
              ),
            ],
          ],
        ),
      ),
    );
    if (!copyable) return MergeSemantics(child: row);
    return Semantics(
      button: true,
      hint: copyLabel,
      child: InkWell(
        onTap: () async {
          final messenger = ScaffoldMessenger.of(context);
          await Clipboard.setData(ClipboardData(text: value));
          messenger
            ..clearSnackBars()
            ..showSnackBar(SnackBar(content: Text(l10n.commonCopied)));
        },
        child: row,
      ),
    );
  }
}

/// 标签色：色板里每种颜色一个色块，单选。选中的色块上打勾并加一圈描边——不只靠颜色
/// 区分；读屏念颜色名和「已选中」。
class _AccentPicker extends StatelessWidget {
  const _AccentPicker({required this.selected, required this.onSelected});

  final int selected;
  final ValueChanged<int> onSelected;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final count = c.stations.length;
    final current = selected % count;
    Widget swatch(int i) => _Swatch(
      color: c.stations[i],
      ring: c.ink,
      check: c.shelf,
      selected: i == current,
      label: accentName(l10n, i),
      onTap: () => onSelected(i),
    );
    return Padding(
      padding: const EdgeInsets.symmetric(
        horizontal: Space.md,
        vertical: Space.sm,
      ),
      // 放得下就一行；放不下就对半分两行，每行等距排开——不让 Wrap 折出一行六个、
      // 一行两个的残行。
      child: LayoutBuilder(
        builder: (context, constraints) {
          final perRow = constraints.maxWidth >= count * Space.touch
              ? count
              : (count / 2).ceil();
          return Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              for (var start = 0; start < count; start += perRow)
                Row(
                  mainAxisAlignment: MainAxisAlignment.spaceBetween,
                  children: [
                    for (var i = start; i < start + perRow && i < count; i++)
                      swatch(i),
                  ],
                ),
            ],
          );
        },
      ),
    );
  }
}

class _Swatch extends StatelessWidget {
  const _Swatch({
    required this.color,
    required this.ring,
    required this.check,
    required this.selected,
    required this.label,
    required this.onTap,
  });

  final Color color;
  final Color ring;
  final Color check;
  final bool selected;
  final String label;
  final VoidCallback onTap;

  static const _dot = 30.0;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      label: label,
      selected: selected,
      inMutuallyExclusiveGroup: true,
      button: true,
      child: InkResponse(
        onTap: onTap,
        radius: Space.touch / 2,
        child: SizedBox.square(
          dimension: Space.touch,
          child: Center(
            child: Container(
              width: _dot + Space.sm,
              height: _dot + Space.sm,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                border: Border.all(
                  color: selected ? ring : Colors.transparent,
                  width: Strokes.hairline * 2,
                ),
              ),
              alignment: Alignment.center,
              child: Container(
                width: _dot,
                height: _dot,
                decoration: BoxDecoration(color: color, shape: BoxShape.circle),
                child: selected
                    ? Icon(
                        LucideIcons.check,
                        size: Strokes.iconSmall,
                        color: check,
                      )
                    : null,
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 改名对话框。清空后保存表示「用回主机名」；没有主机名可回时不许存空名字。
class _RenameDialog extends StatefulWidget {
  const _RenameDialog({required this.record});

  final RuntimeRecord record;

  @override
  State<_RenameDialog> createState() => _RenameDialogState();
}

class _RenameDialogState extends State<_RenameDialog> {
  late final _text = TextEditingController(text: widget.record.name)
    ..selection = TextSelection(
      baseOffset: 0,
      extentOffset: widget.record.name.length,
    );

  String? get _hostname {
    final h = widget.record.hostname?.trim();
    return h == null || h.isEmpty ? null : h;
  }

  /// 要存的名字；存不了返回 null。
  String? get _result {
    final typed = _text.text.trim();
    return typed.isEmpty ? _hostname : typed;
  }

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  void _save() {
    final name = _result;
    if (name != null) Navigator.pop(context, name);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final hostname = _hostname;
    return AlertDialog(
      title: Text(l10n.machinesRenameTitle),
      content: TextField(
        controller: _text,
        autofocus: true,
        textInputAction: TextInputAction.done,
        onSubmitted: (_) => _save(),
        decoration: InputDecoration(
          labelText: l10n.machinesName,
          hintText: hostname,
          helperText: hostname == null
              ? null
              : l10n.machinesRenameEmptyHint(hostname),
          helperMaxLines: 2,
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: Text(l10n.commonCancel),
        ),
        ListenableBuilder(
          listenable: _text,
          builder: (context, _) => FilledButton(
            onPressed: _result == null ? null : _save,
            child: Text(l10n.commonSave),
          ),
        ),
      ],
    );
  }
}
