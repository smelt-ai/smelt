/// 机器在界面上的几种说法：系统、标签色名、配对失败原因、中继设置是否混用。
///
/// 只做「同一份数据怎么说给人听」，不做业务判断。
library;

import 'package:flutter/widgets.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';

/// 运行时报的系统名（`macos`、`linux`…）规整成小写再比，老运行时可能报别的写法。
String? _osKey(String? os) => os?.trim().toLowerCase();

IconData osIcon(String? os) => switch (_osKey(os)) {
  'macos' || 'darwin' => LucideIcons.apple,
  'linux' => LucideIcons.squareTerminal,
  'windows' => LucideIcons.monitor,
  _ => LucideIcons.laptopMinimal,
};

String osLabel(L10n l10n, String? os) => switch (_osKey(os)) {
  'macos' || 'darwin' => l10n.machinesOsMacos,
  'linux' => l10n.machinesOsLinux,
  'windows' => l10n.machinesOsWindows,
  // 认不出的就原样给出（比「未知」有信息量），完全没有才说未知。
  final other? when other.isNotEmpty => os!.trim(),
  _ => l10n.machinesOsUnknown,
};

/// 色块的读屏名。顺序与 `Primitives.stationLight / stationDark` 一一对应；
/// 色板以后加色时多出来的按序号念。
String accentName(L10n l10n, int index) => switch (index) {
  0 => l10n.machinesAccentBlue,
  1 => l10n.machinesAccentTeal,
  2 => l10n.machinesAccentViolet,
  3 => l10n.machinesAccentPink,
  4 => l10n.machinesAccentOlive,
  5 => l10n.machinesAccentIndigo,
  6 => l10n.machinesAccentSky,
  7 => l10n.machinesAccentBrown,
  _ => l10n.machinesAccentOther(index + 1),
};

/// 各机器的中继设置是否混用：有的指定了、有的没指定，或指定了不止一个。
///
/// 内核的 iroh endpoint 形态由全部机器的中继设置决定（docs/mobile-redesign.md「iroh
/// endpoint」）：都不指定、或都指定同一个时没问题；其它组合下自建中继的域名走 iroh 自带的
/// 解析器，iOS 开着系统代理时可能连不上。界面据此决定要不要写那行说明。
bool relaySetupMixed(FleetView fleet) =>
    {for (final r in fleet.runtimes) r.record.relayUrl}.length > 1;

/// 配对失败的原因，按错误类别说清楚该怎么办。配对码一次性，内核不自动重试。
String pairingFailureText(L10n l10n, Object error) {
  if (error is! SmeltError) {
    return l10n.machinesPairErrorOther(error.toString());
  }
  return switch (error.kind) {
    SmeltErrorKind.invalidPairing => l10n.machinesPairErrorExpired,
    SmeltErrorKind.incompatible => l10n.machinesPairErrorIncompatible,
    SmeltErrorKind.offline => l10n.machinesPairErrorOffline,
    SmeltErrorKind.unpaired => l10n.machinesPairErrorRejected,
    SmeltErrorKind.cancelled => l10n.machinesPairErrorCancelled,
    SmeltErrorKind.other => l10n.machinesPairErrorOther(error.message),
  };
}
