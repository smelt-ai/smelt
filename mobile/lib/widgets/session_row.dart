import 'package:flutter/material.dart';
import 'package:flutter/semantics.dart';
import 'package:flutter_slidable/flutter_slidable.dart';

import '../theme/smelt_theme.dart';

import '../services/gateway_service.dart';
import 'agent_icon.dart';

/// 会话行的标题。空标题要给个能读的兜底，而且终端和对话的兜底不一样。
String sessionListTitle(SessionSummary session) {
  final title = session.title.trim();
  if (title.isNotEmpty) return title;
  return session.kind == SessionKind.terminal ? 'Terminal' : 'ACP conversation';
}

/// 会话行的副标题：agent 此刻在干什么。没有就返回 null，别拿空串占位。
String? sessionListSubtitle(SessionSummary session) {
  final detail = session.detail?.trim();
  return detail == null || detail.isEmpty ? null : detail;
}

/// 指挥台和 Projects 共用的会话行。
///
/// 两屏原来各写了一套：指挥台是 `InkWell + Row`（行高约 40、标题 bodyMedium），
/// Projects 是 `ListTile`（行高 56 起、标题 bodyLarge、副标题还能折两行）。同一个
/// 会话在两屏字号、行高、缩进都对不上，来回切时要重新适应一次。差异全都不是有意
/// 设计的，只是先后写出来的。
///
/// 真正该保留的差异只有三处，所以做成参数：
/// - [showProject]：指挥台是跨项目的分诊列表，项目名是定位信息；Projects 里行已经
///   在项目分组下面了，再画一遍是纯重复。
/// - [leading]：指挥台 Running 段要让图标呼吸，需要包一层动效。
/// - [trailing]：指挥台放相对时间，Projects 放未读点和操作菜单。
///
/// 状态**不出现在文案里**。agent 图标已经用颜色表达了状态（形状是 who、颜色是
/// state，跟桌面同一套），再写一遍 `Idle` 是同一条信息画两遍。给读屏用户的那份
/// 状态信息走 [Semantics]，不占视觉空间。
class SessionRow extends StatelessWidget {
  const SessionRow({
    super.key,
    required this.session,
    required this.onTap,
    this.leading,
    this.trailing,
    this.showProject = false,
    this.padding = const EdgeInsets.symmetric(horizontal: 4, vertical: 9),
    this.onDelete,
    this.agentName,
  });

  final SessionSummary session;
  final VoidCallback onTap;
  final Widget? leading;
  final Widget? trailing;
  final bool showProject;
  final EdgeInsetsGeometry padding;

  /// 智能体对话的展示名。不传就从最近一次拉到的目录里配——留出注入口是为了让
  /// 单测不必去动那个 final 全局单例。
  final String? agentName;

  /// 给了就能删：组件自己负责挂左滑手势和对应的无障碍动作，调用方只表态「这行
  /// 允许删除」。删除入口原来是行尾一个只有单项的「⋯」菜单，占着 32pt 宽还多一次
  /// 点击；左滑是移动端列表删除的通用手势，也把那一格还给了标题。
  ///
  /// 传 null 表示不可删（比如只读配对）——那时连手势都不挂，滑不动比滑出一个点
  /// 不动的按钮更好懂。
  final VoidCallback? onDelete;

  /// leading 槽宽。图标是 18pt，槽给 20pt 留出居中余量；两屏共用同一个值，标题
  /// 左边缘才对得齐。
  static const double leadingSlot = 20;
  static const double _gap = 8;
  static const _slidableGroup = 'session-actions';

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.colorScheme.onSurfaceVariant;
    final title = sessionListTitle(session);
    final project = session.projectTitle?.trim();
    final detail = sessionListSubtitle(session);

    // 副标题按「项目 · 在干什么」拼；缺哪段就省哪段，不留孤零零的分隔点。
    //
    // 项目名只在 [showProject] 时画，且跟标题完全相同时省掉（终端会话的标题默认取
    // 目录名，常常就等于项目名，那时重复一遍纯属噪音）。
    //
    // 这里原来用的是「标题里包含项目名就不画」，那是错的：它假设服务端会给标题追加
    // 项目后缀，但服务端的标题依次取 `custom_title`（用户手打）→ `launch_label` →
    // 路径名，**从不追加项目名**（见 `crates/smelt-remote-gateway/src/lib.rs`）。
    // 于是这条规则只在用户碰巧把项目名写进自己起的标题时才触发，行结构随机缺一段；
    // 而且 `contains` 太松——项目名叫 `api` 之类的话几乎每行都会被误吞。
    final wantsProject =
        showProject &&
        project != null &&
        project.isNotEmpty &&
        project.toLowerCase() != title.toLowerCase();

    // 自动化 Run 没有项目，那个位置让给「哪条自动化」。标题是 agent 自己起的
    // （「汇总昨日 PR」），单看认不出这条是半夜自己冒出来的还是用户开的；⚙ 和
    // 自动化名合起来才回答「这不是我起的」。触发方式不进行内——那是决策时才需要
    // 的信息，卡片上有。
    //
    // 智能体对话同理：它也没有项目（工作区是智能体自己的 space），那一格放智能体
    // 名。指挥台现在是一份跨项目 + 跨智能体的全量列表，行上没有归属就只剩一串
    // 认不出来源的标题。名字配不上（目录还没拉到、定义已删）时退回通用标签，
    // 不把 uuid 印到界面上。
    final automation = session.automation;
    final agentConversation = session.isAgentConversation
        ? (agentName ??
              gatewayService.agentDefinitionName(session.agentDefinitionId) ??
              'Agent')
        : null;
    final subtitle = [
      if (automation != null)
        automation.automationName
      else if (agentConversation != null)
        agentConversation
      else if (wantsProject)
        project,
      ?detail,
    ].join(' · ');

    final row = Semantics(
      // 左滑是隐藏手势，读屏软件既滑不动也看不见按钮。删除必须还有第二条路。
      customSemanticsActions: onDelete == null
          ? null
          : {const CustomSemanticsAction(label: 'Delete'): onDelete!},
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(8),
        child: Padding(
          padding: padding,
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              SizedBox(
                width: leadingSlot,
                // 状态短语不出现在文案里，但 `AgentIcon` 的语义标签带着它——颜色
                // 对读屏用户不存在，那是仅剩的一条通路。
                child: Center(child: leading ?? AgentIcon(session: session)),
              ),
              const SizedBox(width: _gap),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodyMedium,
                    ),
                    if (subtitle.isNotEmpty) ...[
                      const SizedBox(height: 2),
                      Row(
                        children: [
                          if (automation != null) ...[
                            Icon(
                              Icons.settings_suggest_outlined,
                              size: 12,
                              color: muted,
                            ),
                            const SizedBox(width: 3),
                          ],
                          Expanded(
                            child: Text(
                              subtitle,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: theme.textTheme.labelSmall?.copyWith(
                                color: muted,
                              ),
                            ),
                          ),
                        ],
                      ),
                    ],
                  ],
                ),
              ),
              if (trailing != null) ...[const SizedBox(width: _gap), trailing!],
            ],
          ),
        ),
      ),
    );

    if (onDelete == null) return row;

    return Slidable(
      key: ValueKey(session.id),
      // 同一时刻只展开一行；配合列表外层的 SlidableAutoCloseBehavior 生效。
      // 两行同时敞着的话，紧接着的那一下点击很难说清打给谁。
      groupTag: _slidableGroup,
      endActionPane: ActionPane(
        motion: const DrawerMotion(),
        extentRatio: 0.28,
        // 刻意不给 DismissiblePane：删除会结束终端进程，不可逆的动作不该能被
        // 一次划到底的手势直接完成。滑出按钮 → 点 → 确认，三步都留着。
        children: [
          SlidableAction(
            onPressed: (_) => onDelete!(),
            backgroundColor: context.smeltColors.danger,
            foregroundColor: Colors.white,
            // 只给图标，不给文字标签。会话行高只有 40pt 上下（没有副标题时更矮），
            // 图标叠文字要 48pt 以上，`SlidableAction` 会把文字直接裁掉。
            // 垃圾桶本身够明确，读屏那条路也不靠这个按钮——它们走的是行上的
            // `CustomSemanticsAction`，根本滑不出这里。
            icon: Icons.delete_outline,
          ),
        ],
      ),
      child: row,
    );
  }
}

/// 包住一列 [SessionRow]，让「展开一行时自动收起别行」生效。
///
/// 这层是薄包装，存在的意义是把 `flutter_slidable` 的依赖圈在本文件里——换掉滑动
/// 方案时只改这一个文件，调用方不用跟着动。
class SessionRowGroup extends StatelessWidget {
  const SessionRowGroup({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => SlidableAutoCloseBehavior(child: child);
}
