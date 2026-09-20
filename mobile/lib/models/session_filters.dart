import '../services/gateway_service.dart';

/// 会话筛选谓词。单独成文件是为了让「哪些会话在等我」这个判断能被多处复用
/// （指挥台首屏、底部导航角标、全局待办徽标），而不是锁在 main.dart 里。
///
/// 这里只留谓词，不再有「当前选中哪个筛选」的概念：那套 SegmentedButton 已经
/// 被底部导航取代——它表面是筛选器、实际是导航（切 All 出项目树、切 Action 出
/// 平铺列表，同一个控件切出两种信息结构），是原 IA 最核心的错配。
bool sessionNeedsAction(SessionSummary session) {
  if (session.attention?.requiresAction == true) return true;
  return switch (session.status.toLowerCase()) {
    'needs_you' || 'waiting_approval' || 'needs_attention' => true,
    _ => false,
  };
}

/// 这条会话属不属于项目树。
///
/// 自动化 Run 不属于：它的工作区是 daemon 按自动化 id 分配的目录，项目分组一旦
/// 按 cwd 兜底就会凭空造出一个用户从没打开过的「项目」。Run 只在指挥台露面——
/// 「现在有什么在动、有什么等我」本来就是那一屏的职责。
bool sessionBelongsToProjectTree(SessionSummary session) =>
    session.automation == null;

bool sessionIsRunning(SessionSummary session) {
  if (session.status.toLowerCase() == 'running') return true;
  return switch (session.phase.toLowerCase()) {
    'starting' || 'running' => true,
    _ => false,
  };
}

/// 「刚完成」：跑完了、而且你还没看过。
///
/// Agent 状态只有三态，`status` 不再发 `done`。未读用摘要上的 `unread`，
/// 相位仍是 `succeeded`。看过一眼 `unread` 变 false，这一段自己清空。
bool sessionRecentlyDone(SessionSummary session) {
  if (sessionNeedsAction(session) || sessionIsRunning(session)) return false;
  if (session.status.toLowerCase() == 'done') return true;
  return session.unread && session.phase.toLowerCase() == 'succeeded';
}

/// 「跑完了，而且你已经看过」——指挥台的第四段。
///
/// 「刚完成」那一段是**看过就消失**的（协议侧 `done` 自带未读语义，读过退回
/// `idle`），结果一次跑完的会话在指挥台上凭空没了，用户失去了「最近跑的是哪个」
/// 这条线索。这一段接住它们。
///
/// 「跑完且已读」= `phase == succeeded` 且不是未读。未读走「刚完成」。
/// 运行时没了时 phase 可能是历史残留，用 `dead` / 旧的 `disconnected` 排除。
bool sessionRecentlyRan(SessionSummary session) {
  if (sessionNeedsAction(session) || sessionIsRunning(session)) return false;
  // 还没看过的归上一段「刚完成」，不要在两段里各出现一次。
  if (sessionRecentlyDone(session)) return false;
  if (session.status.toLowerCase() == 'disconnected') return false;
  if (session.phase.toLowerCase() == 'dead') return false;
  return session.phase.toLowerCase() == 'succeeded';
}

/// 兜底段：不在上面任何一段里的会话。
///
/// 前四段合起来只覆盖「有事的」——在等我、在跑、刚跑完、跑完看过了。**闲置的
/// 对话一条都不显示**：聊完一轮就掉出指挥台，智能体对话更是几乎永远闲置（它
/// 们的常态就是「上次聊完了，等我下次找它」）。于是「我昨天跟工作助手聊的那
/// 条在哪」这个问题，在指挥台上无解，只能去 Projects 里翻——而智能体对话根本
/// 不属于任何项目，那里也没有它的位置。
///
/// 加这一段等于承认：指挥台的分诊职责由**排序**承担（有事的在最上面），而不是
/// 靠把闲置会话藏起来。藏起来省下的那点屏幕，代价是用户找不回自己的对话。
///
/// **从没跑过的会话也在这一段**。这里曾按 `updated_at > 0` 把它们挡在外面，理由
/// 是「没有时间就排不出序、行尾还空着」。那个理由的前提是「名册来自 daemon 活动
/// 记录，没时间的只可能是刚开就没用过的终端 pane」——名册改以菜单为准之后，前提
/// 没了：桌面上开着但这次还没说过话的会话一律没有时间，挡掉它们会让刚重启桌面
/// 的用户看到一块空白的指挥台。
///
/// 当初担心的两点本来也不成立：时间倒序里 `0` 自然沉底，行尾的相对时间对 `0`
/// 本来就不渲染。真正缺的是同为 `0` 时的次序，由段内排序回落到名册序补上。
bool sessionIsIdle(SessionSummary session) =>
    !sessionNeedsAction(session) &&
    !sessionIsRunning(session) &&
    !sessionRecentlyDone(session) &&
    !sessionRecentlyRan(session);
