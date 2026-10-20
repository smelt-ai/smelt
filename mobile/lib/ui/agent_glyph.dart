import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

/// agent 身份图标：跟桌面同一份单色 SVG（LobeHub Icons，MIT，见
/// `assets/agent-icons/THIRD_PARTY_NOTICES.md`），颜色由调用方给。
///
/// 认不出是哪家时：ACP 会话画机器人，终端会话画终端，**不能**反过来——把一条 ACP 会话
/// 画成终端是错误信息，不是信息缺失。
class AgentGlyph extends StatelessWidget {
  const AgentGlyph({
    super.key,
    required this.agent,
    required this.color,
    this.size = 18,
    this.terminal = false,
  });

  /// agent 标识（`claude`、`codex`、自定义的 `claude-quant`……）。
  final String agent;
  final Color color;
  final double size;

  /// 终端会话：认不出 agent 时画终端。
  final bool terminal;

  @override
  Widget build(BuildContext context) {
    final asset = agentIconAsset(agent);
    if (asset == null) {
      return Icon(
        terminal ? LucideIcons.squareTerminal : LucideIcons.bot,
        size: size,
        color: color,
      );
    }
    return SvgPicture.asset(
      asset,
      width: size,
      height: size,
      colorFilter: ColorFilter.mode(color, BlendMode.srcIn),
    );
  }
}

/// 图标名单从资产清单读，不在代码里抄一份（桌面按同一约定拼路径，两边齐全由
/// `smelt-core` 的 `every_kind_has_an_icon_in_the_desktop_and_mobile_bundles` 钉住）。
const agentIconDir = 'assets/agent-icons';

Set<String>? _iconIds;

Set<String> get agentIconIds => _iconIds ?? const {};

/// 启动时读一次资产清单并缓存：清单是异步的，图标要同步画。读失败就全体退到兜底图标。
Future<void> loadAgentIcons([AssetBundle? bundle]) async {
  if (_iconIds != null) return;
  try {
    final manifest = await AssetManifest.loadFromAssetBundle(
      bundle ?? rootBundle,
    );
    const prefix = '$agentIconDir/agent-';
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

/// 认不出返回 null。服务端对普通长度的 id 用包含匹配（自定义 agent 常是 `claude-quant`），
/// 短 id `pi` 用边界匹配，不误中 `copilot`；长 id 先试。
String? agentIconAsset(String agent) {
  final key = agent.trim().toLowerCase();
  if (key.isEmpty) return null;
  final ids = agentIconIds;
  if (ids.contains(key)) return '$agentIconDir/agent-$key.svg';
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

/// 单据头、读屏标签里用的 agent 名字：认得出用品牌名，认不出原样首字母大写。
String agentDisplayName(String agent) {
  final key = agent.trim().toLowerCase();
  const names = {
    'claude': 'Claude',
    'codex': 'Codex',
    'copilot': 'Copilot',
    'grok': 'Grok',
    'pi': 'Pi',
    'cursor': 'Cursor',
    'opencode': 'OpenCode',
    'kiro': 'Kiro',
    'crush': 'Crush',
    'dsh': 'DSH',
    'antigravity': 'Antigravity',
  };
  final known = names[key];
  if (known != null) return known;
  for (final entry in names.entries) {
    if (entry.key.length > 2 && key.contains(entry.key)) return entry.value;
  }
  if (key.isEmpty || key == 'other') return 'Agent';
  return key[0].toUpperCase() + key.substring(1);
}
