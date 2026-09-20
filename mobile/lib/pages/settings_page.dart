import 'package:flutter/material.dart';

import '../models/saved_desktop.dart';
import '../services/gateway_service.dart';
import '../services/terminal_prefs_store.dart';
import '../theme/smelt_theme.dart';

/// 设置页，对应设计稿 F。
///
/// 三件事：**连接**（常驻状态条从主区收进来的去处）、**外观**（主题、终端字号）。
/// 设计稿里还有「通知」和「语言」两组，都对应尚未落地的能力——摆一排点不动的开关
/// 比没有更糟，等分片 2 / 分片 6 落地再加。
class SettingsPage extends StatelessWidget {
  const SettingsPage({
    super.key,
    required this.desktops,
    required this.connectionState,
    required this.connectionBar,
    required this.themeMode,
    required this.onThemeModeChanged,
    required this.terminalFontSize,
    required this.onTerminalFontSizeChanged,
    required this.onSwitchDesktop,
    required this.onPair,
    required this.onDisconnect,
    this.gateway,
  });

  final SavedDesktopCollection desktops;
  final WsState connectionState;

  /// 连接异常时的横幅由 home 提供——设置页不重复实现一遍连接错误的表达。
  /// **正常联通时不画**：那条信息已经在下面的连接卡里，画两遍是噪音。
  final Widget connectionBar;

  final ThemeMode themeMode;
  final ValueChanged<ThemeMode> onThemeModeChanged;
  final double terminalFontSize;
  final ValueChanged<double> onTerminalFontSizeChanged;

  final VoidCallback onSwitchDesktop;
  final VoidCallback onPair;
  final VoidCallback onDisconnect;

  /// 全局单例不可替换，测试需要注入。
  final GatewayService? gateway;

  GatewayService get _service => gateway ?? gatewayService;

  @override
  Widget build(BuildContext context) {
    final active = desktops.activeDesktop;
    return ListView(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 32),
      children: [
        const _GroupHeader('Connection'),
        if (connectionState != WsState.connected) connectionBar,
        if (active != null) ...[
          _ConnectionCard(
            desktop: active,
            state: connectionState,
            metricsStream: _service.metricsStream,
            initialMetrics: _service.metrics,
            canSwitch: desktops.desktops.length > 1,
            onSwitch: onSwitchDesktop,
            onPair: onPair,
          ),
        ],
        const SizedBox(height: 4),
        _SettingRow(
          title: 'Pair with desktop',
          subtitle: 'Scan the QR code shown by Smelt on desktop',
          onTap: onPair,
          trailing: const Icon(Icons.chevron_right, size: 20),
        ),
        if (connectionState != WsState.disconnected)
          _SettingRow(
            title: 'Disconnect',
            subtitle: 'Keeps the pairing so you can reconnect later',
            danger: true,
            onTap: onDisconnect,
          ),

        const SizedBox(height: 20),
        const _GroupHeader('Appearance'),
        const SizedBox(height: 4),
        _FieldLabel('Theme'),
        const SizedBox(height: 8),
        _ThemeSegments(value: themeMode, onChanged: onThemeModeChanged),
        const SizedBox(height: 20),
        _FieldLabel('Terminal font size · ${terminalFontSize.toInt()}pt'),
        _FontSizeSlider(
          value: terminalFontSize,
          onChanged: onTerminalFontSizeChanged,
        ),
      ],
    );
  }
}

/// 分组标题：一个小标签 + 一道横贯的细线，跟设计稿 `.grp` 一致。
class _GroupHeader extends StatelessWidget {
  const _GroupHeader(this.label);

  final String label;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(top: 8, bottom: 10),
      child: Row(
        children: [
          Text(
            label,
            style: theme.textTheme.labelMedium?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
              fontWeight: FontWeight.w600,
            ),
          ),
          const SizedBox(width: 10),
          Expanded(child: Divider(height: 1, color: theme.dividerColor)),
        ],
      ),
    );
  }
}

class _FieldLabel extends StatelessWidget {
  const _FieldLabel(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Text(
      text,
      style: theme.textTheme.bodySmall?.copyWith(
        color: theme.colorScheme.onSurfaceVariant,
      ),
    );
  }
}

/// 连接卡：状态点 + 设备名 + 链路 chip + 链路明细 + 两个动作。
///
/// chip 和明细都只画协议**真的**给了的东西：链路类型未知或没测出延迟时那一段
/// 直接不画，不填「—」也不猜。
class _ConnectionCard extends StatelessWidget {
  const _ConnectionCard({
    required this.desktop,
    required this.state,
    required this.metricsStream,
    required this.initialMetrics,
    required this.canSwitch,
    required this.onSwitch,
    required this.onPair,
  });

  final SavedDesktop desktop;
  final WsState state;
  final Stream<ConnectionMetrics> metricsStream;
  final ConnectionMetrics initialMetrics;
  final bool canSwitch;
  final VoidCallback onSwitch;
  final VoidCallback onPair;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = theme.extension<SmeltColors>()!;
    final dotColor = switch (state) {
      WsState.connected => colors.done,
      WsState.connecting || WsState.reconnecting => colors.running,
      WsState.disconnected => colors.idle,
    };

    return Container(
      padding: const EdgeInsets.fromLTRB(13, 12, 13, 12),
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainer,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: theme.dividerColor),
      ),
      child: StreamBuilder<ConnectionMetrics>(
        stream: metricsStream,
        initialData: initialMetrics,
        builder: (context, snapshot) {
          final metrics = snapshot.data ?? initialMetrics;
          final chip = connectionChipLabel(state, metrics);
          final detail = connectionPathDetail(state, metrics);
          return Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Container(
                    width: 8,
                    height: 8,
                    decoration: BoxDecoration(
                      color: dotColor,
                      shape: BoxShape.circle,
                    ),
                  ),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Text(
                      desktop.name,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.titleSmall?.copyWith(
                        fontWeight: FontWeight.w500,
                      ),
                    ),
                  ),
                  if (chip != null) ...[const SizedBox(width: 8), _Chip(chip)],
                ],
              ),
              if (detail != null) ...[
                const SizedBox(height: 10),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    SizedBox(
                      width: 46,
                      child: Text(
                        'Path',
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: theme.colorScheme.onSurfaceVariant,
                        ),
                      ),
                    ),
                    Expanded(
                      child: Text(detail, style: theme.textTheme.bodySmall),
                    ),
                  ],
                ),
              ],
              const SizedBox(height: 12),
              Row(
                children: [
                  if (canSwitch)
                    OutlinedButton(
                      onPressed: onSwitch,
                      child: const Text('Switch device'),
                    ),
                  if (canSwitch) const SizedBox(width: 8),
                  OutlinedButton(
                    onPressed: onPair,
                    child: const Text('Re-pair'),
                  ),
                ],
              ),
            ],
          );
        },
      ),
    );
  }
}

/// 链路 chip 的文案。没连上、或既认不出链路又没有延迟时返回 null（不画）。
String? connectionChipLabel(WsState state, ConnectionMetrics metrics) {
  if (state != WsState.connected) return null;
  final kind = switch (metrics.kind) {
    ConnectionPathKind.lan => 'LAN',
    ConnectionPathKind.p2p => 'P2P',
    ConnectionPathKind.relay => 'Relay',
    ConnectionPathKind.direct => 'Direct',
    ConnectionPathKind.unknown => null,
  };
  final latency = metrics.latencyMs;
  if (kind == null && latency == null) return null;
  if (kind == null) return '${latency}ms';
  if (latency == null) return kind;
  return '$kind · ${latency}ms';
}

/// 链路明细。认不出就不画——「未知」这个词对用户没有信息量。
String? connectionPathDetail(WsState state, ConnectionMetrics metrics) {
  if (state != WsState.connected) return null;
  return switch (metrics.kind) {
    ConnectionPathKind.lan => 'Direct over LAN',
    ConnectionPathKind.p2p => 'iroh direct · no relay',
    ConnectionPathKind.relay => 'Forwarded by iroh relay',
    ConnectionPathKind.direct => 'Direct',
    ConnectionPathKind.unknown => null,
  };
}

class _Chip extends StatelessWidget {
  const _Chip(this.label);

  final String label;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(6),
      ),
      child: Text(
        label,
        style: theme.textTheme.labelSmall?.copyWith(
          color: theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

/// 设计稿 `.setrow`：标题 + 说明 + 右侧控件，底部一道细线。
class _SettingRow extends StatelessWidget {
  const _SettingRow({
    required this.title,
    this.subtitle,
    this.trailing,
    this.onTap,
    this.danger = false,
  });

  final String title;
  final String? subtitle;
  final Widget? trailing;
  final VoidCallback? onTap;
  final bool danger;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = theme.extension<SmeltColors>()!;
    final titleColor = danger ? colors.danger : null;
    return InkWell(
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.symmetric(vertical: 12, horizontal: 2),
        decoration: BoxDecoration(
          border: Border(bottom: BorderSide(color: theme.dividerColor)),
        ),
        child: Row(
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    title,
                    style: theme.textTheme.bodyMedium?.copyWith(
                      color: titleColor,
                    ),
                  ),
                  if (subtitle != null) ...[
                    const SizedBox(height: 2),
                    Text(
                      subtitle!,
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: theme.colorScheme.onSurfaceVariant,
                      ),
                    ),
                  ],
                ],
              ),
            ),
            if (trailing != null) ...[
              const SizedBox(width: 12),
              IconTheme.merge(
                data: IconThemeData(color: theme.colorScheme.onSurfaceVariant),
                child: trailing!,
              ),
            ],
          ],
        ),
      ),
    );
  }
}

class _ThemeSegments extends StatelessWidget {
  const _ThemeSegments({required this.value, required this.onChanged});

  final ThemeMode value;
  final ValueChanged<ThemeMode> onChanged;

  @override
  Widget build(BuildContext context) {
    return SegmentedButton<ThemeMode>(
      showSelectedIcon: false,
      segments: const [
        ButtonSegment(value: ThemeMode.light, label: Text('Light')),
        ButtonSegment(value: ThemeMode.dark, label: Text('Dark')),
        ButtonSegment(value: ThemeMode.system, label: Text('System')),
      ],
      selected: {value},
      onSelectionChanged: (selection) => onChanged(selection.first),
    );
  }
}

/// 终端字号。
///
/// 设计稿画的是连续滑杆，这里用**离散档位**的滑杆：字号变化会重算终端 cols/rows
/// 并向 PTY 发 resize，连续拖动等于对着远端狂发 resize。`divisions` 让手柄吸附到
/// `TerminalPrefs.steps`，观感仍是滑杆。
class _FontSizeSlider extends StatelessWidget {
  const _FontSizeSlider({required this.value, required this.onChanged});

  final double value;
  final ValueChanged<double> onChanged;

  @override
  Widget build(BuildContext context) {
    final steps = TerminalPrefs.steps;
    final index = steps.indexOf(value);
    return Slider(
      value: (index < 0 ? steps.indexOf(TerminalPrefs.defaultFontSize) : index)
          .toDouble(),
      min: 0,
      max: (steps.length - 1).toDouble(),
      divisions: steps.length - 1,
      label: '${steps[index < 0 ? 0 : index].toInt()}pt',
      onChanged: (next) => onChanged(steps[next.round()]),
    );
  }
}
