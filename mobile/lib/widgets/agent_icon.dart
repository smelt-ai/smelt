import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_svg/flutter_svg.dart';

import '../services/gateway_service.dart';
import 'project_avatar.dart';

/// 会话行的身份图标，跟桌面保持一致。
///
/// 桌面的做法见 `crates/smelt/src/session_list.rs` 的 `provider_icon`，它上面那句
/// 注释写得很清楚：
///
/// > agent 身份图标统一走本地单色 SVG，**颜色由会话状态驱动**：图标同时回答
/// > 「这是哪家的会话」和「当前处于什么状态」。
///
/// 所以这里也是一个图标扛两件事——形状是 who，颜色是 state。移动端原来两件事都
/// 没答：ACP 会话一律是个通用的对话气泡。
///
/// 图标本身是 `fill="currentColor"` 的单色 SVG（LobeHub Icons，MIT），跟桌面同一
/// 份源文件，见 `mobile/assets/agent-icons/THIRD_PARTY_NOTICES.md`。
class AgentIcon extends StatelessWidget {
  const AgentIcon({
    super.key,
    required this.session,
    this.size = 18,
    this.color,
  });

  final SessionSummary session;
  final double size;

  /// 不给就按会话状态染色（跟桌面一致）。
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final tint = color ?? sessionStatusColor(context, session);
    final label = agentIconLabel(session);

    // 终端会话也可能有 agent 身份：在内嵌终端里跑的 CLI（`codex`、`opencode`…）
    // 网关会把它认出来下发。桌面就是这么画的（`session_list/row.rs` 的
    // `provider_icon`：认得出画那家，认不出才画终端）——移动端原来一律画终端
    // 图标，等于把「这是 Codex 在干活」这条信息丢掉了。
    if (session.kind == SessionKind.terminal &&
        agentIconAsset(session.agent) == null) {
      return Semantics(
        label: label,
        child: Icon(Icons.terminal, size: size, color: tint),
      );
    }

    return Semantics(
      label: label,
      child: AgentGlyph(
        agent: session.agent,
        size: size,
        color: tint,
        // 认不出的裸终端在上面已经拦掉了；走到这里还认不出，说明是一条 ACP
        // 会话——那就该是机器人，不是终端。
        fallback: session.kind == SessionKind.terminal
            ? Icons.terminal
            : Icons.smart_toy_outlined,
      ),
    );
  }
}

/// 只按 agent 标识取图标，不牵扯会话状态。
///
/// 会话行之外还有别的地方需要「这是哪家 agent」——最典型的是新建会话时的 agent
/// 选择器。那里原来所有 agent 共用一个通用机器人图标，等于让用户在一列一模一样的
/// 行里选身份。
class AgentGlyph extends StatelessWidget {
  const AgentGlyph({
    super.key,
    required this.agent,
    this.size = 20,
    this.color,
    this.fallback = Icons.smart_toy_outlined,
  });

  /// agent 标识。自定义 agent 传 `kind`（`claude-quant` 的 kind 是 `claude`）
  /// 比传 `id` 更准。
  final String agent;
  final double size;
  final Color? color;

  /// 认不出是哪家时画什么。默认是机器人，**不能**默认成终端图标——那会把一条
  /// ACP 会话画成终端，是错误信息而不是信息缺失。
  final IconData fallback;

  @override
  Widget build(BuildContext context) {
    final tint = color ?? Theme.of(context).colorScheme.onSurfaceVariant;
    final asset = agentIconAsset(agent);

    // 认不出是哪家（协议兜底值是 "other"，也可能是用户自定义 agent）。
    if (asset == null) {
      return Icon(fallback, size: size, color: tint);
    }

    return SvgPicture.asset(
      asset,
      width: size,
      height: size,
      colorFilter: ColorFilter.mode(tint, BlendMode.srcIn),
    );
  }
}

/// 有哪些 agent 有图标，从资产目录读出来，不在代码里再抄一份名单。
///
/// 桌面那边图标路径是按稳定 id 拼的（`agent_kind.rs` 的 `icon_asset`），这里沿用
/// 同一个约定。名单一旦写进代码，新增一家 agent 就变成「Rust 改一处、Dart 再改
/// 一处」，而漏掉 Dart 那处的表现只是图标掉回兜底——没人会因此报错。
/// 两份 bundle 的图标是否齐全，由 `smelt-core` 的
/// `every_kind_has_an_icon_in_the_desktop_and_mobile_bundles` 钉住。
const agentIconDir = 'assets/agent-icons';

Set<String>? _iconIds;

/// 已发现的 agent 图标 id。[loadAgentIcons] 跑完之前是空集。
Set<String> get agentIconIds => _iconIds ?? const {};

/// 启动时读一次资产清单。清单是异步的，而图标要同步渲染，所以结果缓存下来。
///
/// 读失败不抛：图标是装饰，拿不到就全体退到兜底图标，不该拖垮启动。
Future<void> loadAgentIcons([AssetBundle? bundle]) async {
  if (_iconIds != null) return;
  try {
    final manifest = await AssetManifest.loadFromAssetBundle(
      bundle ?? rootBundle,
    );
    final prefix = '$agentIconDir/agent-';
    _iconIds = manifest
        .listAssets()
        .where((key) => key.startsWith(prefix) && key.endsWith('.svg'))
        .map((key) => key.substring(prefix.length, key.length - 4))
        .toSet();
  } catch (_) {
    _iconIds = const {};
  }
}

@visibleForTesting
void resetAgentIconsForTest() => _iconIds = null;

/// 认不出就返回 null，由调用方决定退到哪个通用图标。
///
/// 服务端对普通长度的 id 保留包含匹配（`agent_kind.rs` 的
/// `command_contains_identifier`），所以这里也不只做相等——自定义 agent 的 id 常常
/// 是 `claude-quant` 这种带后缀的形态。短 id `pi` 使用边界匹配，不会误命中
/// `copilot` 或 `api-key`。
String? agentIconAsset(String agent) {
  final key = agent.trim().toLowerCase();
  if (key.isEmpty) return null;

  final ids = agentIconIds;
  if (ids.contains(key)) return '$agentIconDir/agent-$key.svg';

  // 长 id 优先：`copilot` 必须在 `pi` 之前被试过，否则边界匹配的短 id 有机会
  // 抢到一个更差的解释。
  final ordered = ids.toList()..sort((a, b) => b.length.compareTo(a.length));
  for (final id in ordered) {
    final hit = id.length > 2
        ? key.contains(id)
        : RegExp(
            '(^|[^a-z0-9_])${RegExp.escape(id)}([^a-z0-9_]|\$)',
          ).hasMatch(key);
    if (hit) return '$agentIconDir/agent-$id.svg';
  }
  return null;
}

/// 读屏用的标签。图标是纯视觉的，不给标签等于对读屏用户不存在。
///
/// 图标同时编码 who（形状）和 state（颜色），标签就得同时说这两件事。颜色对读屏
/// 用户根本不存在，会话行里又不再写状态短语了——状态只剩这一条通路，不能漏。
String agentIconLabel(SessionSummary session) {
  final who = switch (session) {
    _ when session.kind == SessionKind.terminal => 'Terminal session',
    _ when session.agent.trim().isEmpty => 'Agent conversation',
    _ => '${session.agent.trim()} conversation',
  };
  return '$who, ${sessionStatusLabel(session)}';
}
