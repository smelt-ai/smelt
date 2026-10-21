// ignore: unused_import
import 'package:intl/intl.dart' as intl;
import 'l10n.dart';

// ignore_for_file: type=lint

/// The translations for Chinese (`zh`).
class L10nZh extends L10n {
  L10nZh([String locale = 'zh']) : super(locale);

  @override
  String get agentsTitle => '智能体';

  @override
  String get agentsSegmentAgents => '智能体';

  @override
  String get agentsSegmentAutomations => '自动化';

  @override
  String get agentsNoMachinesTitle => '还没有配对机器';

  @override
  String get agentsNoMachinesBody => '在「设置」里配对一台机器，它的智能体和自动化会出现在这里。';

  @override
  String get agentsOfflineNote => '这台机器连上后再显示。';

  @override
  String agentsLoadFailed(String message) {
    return '没能读取：$message';
  }

  @override
  String get agentsEmptyAgents => '这台机器上还没有智能体，可以在桌面端 Smelt 里创建。';

  @override
  String get agentsEmptyAutomations => '这台机器上还没有自动化，可以在桌面端 Smelt 里创建。';

  @override
  String get agentsReadOnly => '智能体和自动化在桌面端 Smelt 里编辑。';

  @override
  String agentsGone(String machine) {
    return '$machine 上已经没有这一项了。';
  }

  @override
  String agentsMachineOffline(String machine) {
    return '$machine 离线，连上后才能操作。';
  }

  @override
  String agentsEngine(String engine) {
    return '执行引擎：$engine';
  }

  @override
  String get agentsPrompt => '长期指令';

  @override
  String get agentsNoPrompt => '没有设置长期指令。';

  @override
  String get agentsPlugins => '插件';

  @override
  String get agentsContext => '上下文';

  @override
  String get agentsStartConversation => '开始对话';

  @override
  String get agentsStarting => '正在开始…';

  @override
  String agentsStartFailed(String message) {
    return '没能开始对话：$message';
  }

  @override
  String get agentsStartPending => '对话已经建好，但还没出现在列表里，稍后到「会话」里找。';

  @override
  String get agentsCannotStart => '这台机器上开不了这个智能体的对话：它的引擎在这里不可用。';

  @override
  String agentsNextRun(String when) {
    return '下次 $when';
  }

  @override
  String get agentsNextRunTitle => '下次运行';

  @override
  String get agentsPaused => '已暂停';

  @override
  String get agentsDue => '即将运行';

  @override
  String agentsInMinutes(int n) {
    return '$n 分钟后';
  }

  @override
  String agentsToday(String time) {
    return '今天 $time';
  }

  @override
  String agentsTomorrow(String time) {
    return '明天 $time';
  }

  @override
  String agentsYesterday(String time) {
    return '昨天 $time';
  }

  @override
  String agentsOnDate(String date, String time) {
    return '$date $time';
  }

  @override
  String get agentsNeverRun => '还没运行过';

  @override
  String agentsScheduleDaily(String time) {
    return '每天 $time';
  }

  @override
  String agentsScheduleEveryMinutes(int n) {
    return '每 $n 分钟';
  }

  @override
  String agentsScheduleEveryHours(int n) {
    return '每 $n 小时';
  }

  @override
  String agentsScheduleWeekly(String days, String time) {
    return '$days $time';
  }

  @override
  String agentsScheduleDuring(String days, String start, String end, int n) {
    return '$days $start–$end 每 $n 分钟';
  }

  @override
  String get agentsDaysEveryDay => '每天';

  @override
  String get agentsDaysWeekdays => '工作日';

  @override
  String get agentsDaysWeekends => '周末';

  @override
  String get agentsDaysNone => '不在任何一天';

  @override
  String get agentsDaysSeparator => '、';

  @override
  String agentsWeekday(String day) {
    String _temp0 = intl.Intl.selectLogic(day, {
      'mon': '周一',
      'tue': '周二',
      'wed': '周三',
      'thu': '周四',
      'fri': '周五',
      'sat': '周六',
      'other': '周日',
    });
    return '$_temp0';
  }

  @override
  String agentsTriggerEvent(String topic) {
    return '事件「$topic」发生时';
  }

  @override
  String get agentsTriggerWebhook => '外部调用（webhook）';

  @override
  String get agentsTriggerNone => '没有触发条件';

  @override
  String get agentsRunQueued => '排队中';

  @override
  String get agentsRunRunning => '运行中';

  @override
  String get agentsRunAwaitingApproval => '等待批准';

  @override
  String get agentsRunWaitingForUser => '等你回复';

  @override
  String get agentsRunCompleted => '成功';

  @override
  String get agentsRunFailed => '失败';

  @override
  String get agentsRunCancelled => '已取消';

  @override
  String get agentsRunSkipped => '已跳过';

  @override
  String get agentsSourceManual => '手动运行';

  @override
  String get agentsSourceScheduled => '定时';

  @override
  String get agentsSourceWebhook => '外部调用';

  @override
  String get agentsSourceEvent => '事件';

  @override
  String get agentsEnabled => '启用';

  @override
  String agentsEnableFailed(String name, String message) {
    return '没能启用「$name」：$message';
  }

  @override
  String agentsDisableFailed(String name, String message) {
    return '没能暂停「$name」：$message';
  }

  @override
  String get agentsTrigger => '触发条件';

  @override
  String get agentsAction => '动作';

  @override
  String agentsActionAgent(String name) {
    return '由智能体「$name」执行';
  }

  @override
  String get agentsActionAgentMissing => '执行它的智能体已被删除';

  @override
  String get agentsActionShell => '运行一条 shell 命令';

  @override
  String get agentsLastRun => '最近一次运行';

  @override
  String get agentsRunStartedAt => '开始';

  @override
  String get agentsRunFinishedAt => '结束';

  @override
  String get agentsRunDuration => '耗时';

  @override
  String get agentsRunSource => '来源';

  @override
  String get agentsOpenRun => '打开这次运行的会话';

  @override
  String get agentsRunNow => '立即运行一次';

  @override
  String agentsRunNowStarted(String name) {
    return '已开始运行「$name」';
  }

  @override
  String agentsRunNowFailed(String name, String message) {
    return '没能运行「$name」：$message';
  }

  @override
  String get appName => 'Smelt';

  @override
  String get navPass => '指挥台';

  @override
  String get navSessions => '会话';

  @override
  String get navAgents => '智能体';

  @override
  String get navSettings => '设置';

  @override
  String get commonCancel => '取消';

  @override
  String get commonDone => '完成';

  @override
  String get commonSave => '保存';

  @override
  String get commonRetry => '重试';

  @override
  String get commonOpen => '打开';

  @override
  String get commonClose => '关闭';

  @override
  String get commonDelete => '删除';

  @override
  String get commonRename => '改名';

  @override
  String get commonCopy => '复制';

  @override
  String get commonCopied => '已复制';

  @override
  String get commonMore => '更多';

  @override
  String get commonNew => '新建';

  @override
  String get commonAllow => '允许';

  @override
  String get commonDeny => '拒绝';

  @override
  String get commonAnswer => '回答';

  @override
  String get commonView => '查看';

  @override
  String get commonLoading => '加载中…';

  @override
  String get commonName => '名字';

  @override
  String machineOnlineLan(int ms) {
    return '局域网 · $ms ms';
  }

  @override
  String machineOnlineDirect(int ms) {
    return '直连 · $ms ms';
  }

  @override
  String machineOnlineRelay(int ms) {
    return '中继 · $ms ms';
  }

  @override
  String get machineOnline => '在线';

  @override
  String get machineConnecting => '连接中…';

  @override
  String get machineReconnecting => '重连中…';

  @override
  String get machineSyncing => '同步中…';

  @override
  String get machineOffline => '离线';

  @override
  String machineStatusCached(String status, String time) {
    return '$status · 显示的是 $time 的数据';
  }

  @override
  String get machineRevoked => '这台手机已被这台机器移除';

  @override
  String get machineIncompatible => '这台机器需要升级';

  @override
  String get timeJustNow => '刚刚';

  @override
  String timeMinutesAgo(int n) {
    return '$n 分钟前';
  }

  @override
  String timeHoursAgo(int n) {
    return '$n 小时前';
  }

  @override
  String timeDaysAgo(int n) {
    return '$n 天前';
  }

  @override
  String get phaseConnecting => '启动中';

  @override
  String get phaseThinking => '思考中';

  @override
  String get phaseExecutingTool => '执行工具';

  @override
  String get phaseAwaitingApproval => '等审批';

  @override
  String get phaseWaitingForUser => '等你回答';

  @override
  String get phaseSucceeded => '完成';

  @override
  String get phaseFailed => '失败';

  @override
  String get phaseIdle => '空闲';

  @override
  String get phaseDead => '已结束';

  @override
  String get kindConversation => '对话';

  @override
  String get kindTerminal => '终端';

  @override
  String get conversationUntitled => '未命名对话';

  @override
  String get conversationModelAndSettings => '模型与配置';

  @override
  String get conversationSkills => '技能';

  @override
  String get conversationCopyInfo => '复制会话信息';

  @override
  String conversationInfoSession(String id) {
    return '会话 $id';
  }

  @override
  String get conversationLoading => '正在打开对话…';

  @override
  String get conversationUnavailableTitle => '打不开这段对话';

  @override
  String get conversationUnavailableDefault => '这段对话没有在电脑上运行。';

  @override
  String get conversationErrorDefault => '对话加载失败。';

  @override
  String get conversationCachedSaved => '显示的是上次保存的数据';

  @override
  String get conversationEmptyTitle => '还没有消息';

  @override
  String get conversationEmptyBody => '发一条消息开始吧。';

  @override
  String get conversationLoadEarlier => '加载更早的消息';

  @override
  String get conversationJumpToLatest => '回到最新';

  @override
  String get conversationInterrupted => '已中断';

  @override
  String get conversationThought => '思考';

  @override
  String get conversationThinkingPreview => '思考中…';

  @override
  String get conversationShowThought => '展开思考';

  @override
  String get conversationHideThought => '收起思考';

  @override
  String get conversationCopyAnswer => '复制回答';

  @override
  String conversationTook(String duration) {
    return '用时 $duration';
  }

  @override
  String get conversationCompleted => '已完成';

  @override
  String get conversationCompletionFailed => '未能完成';

  @override
  String get conversationTaskCompleted => '任务已完成。';

  @override
  String conversationToolCalls(int count) {
    return '$count 次工具调用';
  }

  @override
  String conversationToolFailedCount(int count) {
    return '$count 次失败';
  }

  @override
  String get conversationToolRead => '读取';

  @override
  String get conversationToolEdit => '编辑';

  @override
  String get conversationToolDelete => '删除';

  @override
  String get conversationToolMove => '移动';

  @override
  String get conversationToolSearch => '搜索';

  @override
  String get conversationToolExecute => '运行';

  @override
  String get conversationToolThink => '思考';

  @override
  String get conversationToolFetch => '抓取';

  @override
  String get conversationToolSwitchMode => '模式';

  @override
  String get conversationToolCollaborate => '委派';

  @override
  String get conversationToolReview => '审阅';

  @override
  String get conversationToolImage => '图片';

  @override
  String get conversationToolCompact => '压缩';

  @override
  String get conversationToolWait => '等待';

  @override
  String get conversationToolOther => '工具';

  @override
  String get conversationToolPending => '未开始';

  @override
  String get conversationToolInProgress => '进行中';

  @override
  String get conversationToolDone => '完成';

  @override
  String get conversationToolFailed => '失败';

  @override
  String get conversationShowOutput => '展开输出';

  @override
  String get conversationHideOutput => '收起输出';

  @override
  String conversationExitCode(int code) {
    return '退出码 $code';
  }

  @override
  String get conversationOutputTruncated => '输出已截断';

  @override
  String conversationShowAllLines(int count) {
    return '显示全部 $count 行';
  }

  @override
  String get conversationCollapse => '收起';

  @override
  String conversationUnchangedLines(int count) {
    return '$count 行未改动';
  }

  @override
  String get conversationNewFile => '新文件';

  @override
  String conversationPlan(int done, int total) {
    return '计划 · $done/$total';
  }

  @override
  String get conversationPlanPending => '待做';

  @override
  String get conversationPlanInProgress => '进行中';

  @override
  String get conversationPlanCompleted => '已完成';

  @override
  String get conversationTaskBackground => '后台任务';

  @override
  String get conversationTaskPeerMessage => '消息';

  @override
  String get conversationTaskSubagent => '子智能体';

  @override
  String get conversationTaskRunning => '运行中';

  @override
  String get conversationTaskCompletedStatus => '已结束';

  @override
  String get conversationTaskFailed => '失败';

  @override
  String get conversationTaskTimedOut => '超时';

  @override
  String get conversationTaskStopped => '已停止';

  @override
  String get conversationImageOpen => '查看图片';

  @override
  String get conversationImageClose => '关闭图片';

  @override
  String get conversationImageUnavailable => '图片不可用';

  @override
  String get conversationImageUndecodable => '这张图片无法显示。';

  @override
  String conversationImageLoadFailed(String message) {
    return '原图加载失败：$message';
  }

  @override
  String get conversationStatusEnded => '会话已结束';

  @override
  String conversationHint(String agent) {
    return '给 $agent 发消息';
  }

  @override
  String get conversationHintOffline => '机器离线，草稿会保留';

  @override
  String get conversationHintTicket => '先处理上方的单据';

  @override
  String get conversationHintEnded => '这段会话已结束';

  @override
  String get conversationHintSending => '等机器确认收到…';

  @override
  String get conversationSend => '发送';

  @override
  String get conversationStop => '停止本回合';

  @override
  String get conversationAttachImage => '添加图片';

  @override
  String get conversationNoImageSupport => '这个 agent 不接收图片';

  @override
  String get conversationCommands => '命令与技能';

  @override
  String get conversationRemoveImage => '移除图片';

  @override
  String conversationImagesSkipped(int count) {
    return '$count 张图片超过 5 MB，已跳过';
  }

  @override
  String conversationAttachFailed(String message) {
    return '图片添加失败：$message';
  }

  @override
  String get conversationUnconfirmed => '没等到确认，消息可能已经送达。看一眼对话再重发。';

  @override
  String conversationRejected(String message) {
    return '没有发出：$message';
  }

  @override
  String conversationDraftSaveFailed(String message) {
    return '草稿保存失败：$message';
  }

  @override
  String get conversationResendTitle => '再发一次？';

  @override
  String get conversationResendBody => '上一次可能已经送到 agent 那里了。先看看对话，再决定要不要重发。';

  @override
  String get conversationResendConfirm => '再发一次';

  @override
  String conversationContextUsage(int percent, String used, String total) {
    return '上下文 $percent% · 已用 $used / $total tokens';
  }

  @override
  String get conversationModel => '模型';

  @override
  String get conversationProvider => '服务商';

  @override
  String get conversationOn => '开';

  @override
  String get conversationOff => '关';

  @override
  String get conversationSkillsEmpty => '这段对话没有加载技能。';

  @override
  String get conversationSkillsNote => '技能在电脑上管理。';

  @override
  String get conversationCommandsTitle => '命令';

  @override
  String get conversationNoSettings => '这个 agent 没有可在这里切换的配置。';

  @override
  String get conversationTicketApproval => '审批';

  @override
  String get conversationTicketQuestion => '询问';

  @override
  String conversationTicketCount(int index, int count) {
    return '第 $index / $count 张';
  }

  @override
  String get conversationTicketCollapse => '把单据收下去';

  @override
  String get conversationTicketExpand => '展开单据';

  @override
  String conversationActionFailed(String message) {
    return '没发出去：$message';
  }

  @override
  String get launchTitle => '新建会话';

  @override
  String get launchBack => '返回';

  @override
  String get launchStepMachine => '选机器';

  @override
  String get launchStepProject => '选项目';

  @override
  String get launchNoMachineOnline => '没有在线的机器。会话开在机器本身上，要先连上它。';

  @override
  String launchMachineUnavailable(String status) {
    return '$status · 连上后才能新建';
  }

  @override
  String get launchLoadFailed => '这台机器的项目没取到';

  @override
  String get launchNoProjects => '这台机器上没有打开的项目。先在电脑上的 Smelt 里打开一个。';

  @override
  String get launchNoProject => '不选项目';

  @override
  String get launchNoProjectHint => '在智能体自己的空间里和它对话';

  @override
  String get launchSectionPinned => '常用';

  @override
  String get launchSectionConversation => '对话';

  @override
  String get launchSectionTerminal => '终端';

  @override
  String get launchSectionAgents => '智能体';

  @override
  String launchAgentSubtitle(String engine) {
    return '智能体 · $engine';
  }

  @override
  String get launchNoActions => '这里还没有可启动的。升级电脑上的 Smelt 后再试。';

  @override
  String launchStarting(String label) {
    return '正在启动 $label…';
  }

  @override
  String get launchWaiting => '已启动，正在打开…';

  @override
  String launchFailedTitle(String label) {
    return '$label 没能启动';
  }

  @override
  String launchFailed(String message) {
    return '会话没能启动：$message';
  }

  @override
  String launchNotListed(String machine) {
    return '已在 $machine 上启动，但还没出现在列表里。稍后去「会话」里找它。';
  }

  @override
  String get machinesOsMacos => 'macOS';

  @override
  String get machinesOsLinux => 'Linux';

  @override
  String get machinesOsWindows => 'Windows';

  @override
  String get machinesOsUnknown => '未知系统';

  @override
  String get machinesName => '名字';

  @override
  String machinesHostname(String hostname) {
    return '主机名：$hostname';
  }

  @override
  String get machinesRenameTitle => '给机器改名';

  @override
  String machinesRenameEmptyHint(String hostname) {
    return '留空则用主机名（$hostname）';
  }

  @override
  String machinesRenameFailed(String error) {
    return '改名没成功：$error';
  }

  @override
  String get machinesAccent => '标签色';

  @override
  String get machinesAccentFooter => '这台机器上的每条会话都带这个颜色。';

  @override
  String get machinesAccentBlue => '蓝';

  @override
  String get machinesAccentTeal => '青';

  @override
  String get machinesAccentViolet => '紫';

  @override
  String get machinesAccentPink => '粉';

  @override
  String get machinesAccentOlive => '橄榄绿';

  @override
  String get machinesAccentIndigo => '靛蓝';

  @override
  String get machinesAccentSky => '天蓝';

  @override
  String get machinesAccentBrown => '棕';

  @override
  String machinesAccentOther(int n) {
    return '颜色 $n';
  }

  @override
  String machinesAccentFailed(String error) {
    return '换色没成功：$error';
  }

  @override
  String get machinesLink => '链路';

  @override
  String get machinesStatus => '状态';

  @override
  String get machinesPath => '路径';

  @override
  String get machinesPathLan => '局域网';

  @override
  String get machinesPathDirect => '直连';

  @override
  String get machinesPathRelay => '中继';

  @override
  String get machinesPathUnknown => '还没选出';

  @override
  String get machinesRtt => '往返延迟';

  @override
  String machinesRttValue(int ms) {
    return '$ms ms';
  }

  @override
  String get machinesEndpoint => 'Endpoint ID';

  @override
  String get machinesEndpointCopy => '复制 Endpoint ID';

  @override
  String get machinesRelay => '中继服务器';

  @override
  String get machinesRelayDefault => '默认公共中继';

  @override
  String get machinesVersion => '运行时版本';

  @override
  String get machinesVersionUnknown => '还没连上过';

  @override
  String get machinesSystem => '系统';

  @override
  String get machinesLastError => '最近一次失败';

  @override
  String get machinesRelayMixedNote =>
      '你的几台机器中继设置不一致，iOS 开着系统代理或 VPN 时，自建中继可能连不上。';

  @override
  String get machinesReconnect => '立即重连';

  @override
  String get machinesReconnectBusy => '重连中…';

  @override
  String machinesReconnected(String name) {
    return '已连上 $name';
  }

  @override
  String machinesReconnectOffline(String name) {
    return '还是连不上 $name，后台会继续重试。';
  }

  @override
  String machinesReconnectFailed(String error) {
    return '重连没成功：$error';
  }

  @override
  String machinesRevokedBody(String name) {
    return '$name 不再接受这台手机：可能在那台机器上被移除了，或它的设备列表被重置了。在那台机器上生成新的配对码，重新配对即可，这里的名字和标签色会保留。';
  }

  @override
  String machinesIncompatibleBody(String name) {
    return '这个 App 和 $name 上的 Smelt 版本不兼容。升级较旧的一端后点立即重连；还不行再重新配对。';
  }

  @override
  String get machinesRepair => '重新配对';

  @override
  String get machinesForget => '忘记这台机器';

  @override
  String machinesForgetTitle(String name) {
    return '忘记 $name？';
  }

  @override
  String machinesForgetBody(String name) {
    return '只从这台手机上删除 $name 的记录、草稿和离线缓存。这台手机能不能连它，由 $name 自己管理：要撤销授权，请在那台机器的设备列表里移除这台手机。以后可以重新配对。';
  }

  @override
  String get machinesForgetConfirm => '忘记';

  @override
  String machinesForgotten(String name) {
    return '已忘记 $name';
  }

  @override
  String machinesForgetFailed(String error) {
    return '没能忘记这台机器：$error';
  }

  @override
  String get machinesGone => '这台机器已不在这台手机上。';

  @override
  String get machinesPairTitle => '配对机器';

  @override
  String get machinesPairGuide =>
      '扫电脑上 Smelt 的配对码（设置 → 手机远程），或服务器上「smeltd pair」打出来的二维码。';

  @override
  String get machinesPairPaste => '粘贴配对串';

  @override
  String get machinesPairPasteHint => 'smelt+iroh://…';

  @override
  String get machinesPairFromClipboard => '从剪贴板粘贴';

  @override
  String get machinesPairSubmit => '配对';

  @override
  String get machinesPairTorchOn => '打开手电筒';

  @override
  String get machinesPairTorchOff => '关闭手电筒';

  @override
  String get machinesPairNotACode => '这不是 Smelt 的配对码。';

  @override
  String get machinesPairBusyNew => '正在配对新机器…';

  @override
  String machinesPairBusyAgain(String name) {
    return '正在重新配对 $name…';
  }

  @override
  String get machinesPairBusyBody => '其它机器的连接不受影响。';

  @override
  String machinesPaired(String name) {
    return '已连上 $name';
  }

  @override
  String get machinesPairFailed => '没配上';

  @override
  String get machinesPairErrorExpired => '这个配对码已过期或已用过。在那台机器上重新生成一个再扫。';

  @override
  String get machinesPairErrorIncompatible =>
      '这个 App 和那台机器上的 Smelt 版本不兼容。升级较旧的一端，再用新的配对码配对。';

  @override
  String get machinesPairErrorOffline =>
      '连不上那台机器。确认它没睡眠、两边都能上网，再试一次；配对码过期了就重新生成一个。';

  @override
  String get machinesPairErrorRejected => '那台机器拒绝了这台手机。在它上面重新生成配对码再试。';

  @override
  String get machinesPairErrorCancelled => '配对被打断了，再试一次。';

  @override
  String machinesPairErrorOther(String error) {
    return '配对失败：$error';
  }

  @override
  String get machinesPairScanAgain => '再扫一次';

  @override
  String get machinesCameraDenied => 'Smelt 用不了相机。到系统设置里允许访问相机，或者改为粘贴配对串。';

  @override
  String get machinesCameraUnavailable => '相机不可用，可以改为粘贴配对串。';

  @override
  String get passTitle => '指挥台';

  @override
  String get passNewSession => '新建会话';

  @override
  String get passRailEmpty => '没有等你处理的事。';

  @override
  String passTicketsWaiting(int count) {
    return '$count 件事等你';
  }

  @override
  String passTicketNumber(int n) {
    return '#$n';
  }

  @override
  String passWantsToRun(String agent) {
    return '$agent 想运行';
  }

  @override
  String passWantsTo(String agent) {
    return '$agent 请求';
  }

  @override
  String passHasQuestion(String agent) {
    return '$agent 有个问题';
  }

  @override
  String passFailed(String agent) {
    return '$agent 出错停下了';
  }

  @override
  String get passContextYou => '你';

  @override
  String get passLoadingDetails => '正在取它要什么…';

  @override
  String get passMoreOptions => '更多选项';

  @override
  String passStampAllowed(String time) {
    return '已允许 · $time';
  }

  @override
  String passStampDenied(String time) {
    return '已拒绝 · $time';
  }

  @override
  String passStampAnswered(String time) {
    return '已回答 · $time';
  }

  @override
  String get passStationIdle => '这台机器上没有在做的事。';

  @override
  String get passSeeAllSessions => '全部会话';

  @override
  String get passRunning => '在做';

  @override
  String get passJustFinished => '刚完成';

  @override
  String get passRecent => '最近';

  @override
  String get passRepair => '重新配对';

  @override
  String get passNoMachinesTitle => '配对第一台机器';

  @override
  String get passNoMachinesBody =>
      '在 Mac 上打开 Smelt → 设置 → 手机远程，或在服务器上运行 `smeltd pair`，然后扫码。';

  @override
  String get passScanCode => '扫描配对码';

  @override
  String passTriageTitle(int index, int count) {
    return '第 $index / $count 张';
  }

  @override
  String get passTriageDone => '都处理完了';

  @override
  String get passTriageDoneBody => '轨上的单子已经清空。';

  @override
  String get passOpenConversation => '打开对话';

  @override
  String passWorkingDir(String dir) {
    return '在 $dir';
  }

  @override
  String passGrantRoot(String path) {
    return '授权访问 $path';
  }

  @override
  String passActionFailed(String message) {
    return '没发出去：$message';
  }

  @override
  String get approvalAllowOnce => '允许一次';

  @override
  String get approvalAllowAlways => '总是允许';

  @override
  String get approvalRejectOnce => '拒绝';

  @override
  String get approvalRejectAlways => '总是拒绝';

  @override
  String get elicitationSubmit => '提交回答';

  @override
  String get elicitationDismiss => '跳过';

  @override
  String get elicitationRequired => '必填';

  @override
  String get elicitationOpenLink => '打开链接';

  @override
  String get elicitationTextHint => '输入你的回答';

  @override
  String get sessionsTitle => '会话';

  @override
  String get sessionsNewSession => '新建会话';

  @override
  String get sessionsFilterAll => '全部';

  @override
  String get sessionsFilterMachines => '显示哪台机器的会话';

  @override
  String get sessionsSearchHint => '搜索标题、项目、agent';

  @override
  String get sessionsSearchClear => '清除搜索';

  @override
  String get sessionsAgentChats => '智能体对话';

  @override
  String get sessionsAutomationRuns => '自动化运行';

  @override
  String get sessionsNoProject => '无项目';

  @override
  String get sessionsIdleProjects => '没有开着会话的项目';

  @override
  String get sessionsHistoryRow => '历史会话';

  @override
  String get sessionsMachineEmpty => '这台机器上没有会话。';

  @override
  String get sessionsMachineWaiting => '正在等这台机器的会话列表…';

  @override
  String get sessionsNoMachinesTitle => '还没有机器';

  @override
  String get sessionsNoMachinesBody => '在指挥台或设置里配对一台机器，它的会话就会出现在这里。';

  @override
  String sessionsNoMatchesTitle(String query) {
    return '没有匹配「$query」的会话';
  }

  @override
  String get sessionsNoMatchesBody => '搜索范围是标题、项目和 agent。';

  @override
  String get sessionsUntitledConversation => '未命名对话';

  @override
  String get sessionsUntitledTerminal => '终端';

  @override
  String get sessionsDeleteTerminalTitle => '删除这个终端？';

  @override
  String sessionsDeleteTerminalBody(String title, String machine) {
    return '「$title」会在 $machine 上关闭，里面的 shell 进程随之结束。';
  }

  @override
  String get sessionsDeleteConversationTitle => '删除这段对话？';

  @override
  String sessionsDeleteConversationBody(String title, String machine) {
    return '「$title」会在 $machine 上停下并移出会话列表。agent 的对话记录仍在历史会话里。';
  }

  @override
  String sessionsDeleteFailed(String message) {
    return '删除失败：$message';
  }

  @override
  String get sessionsHistoryTitle => '历史会话';

  @override
  String get sessionsHistoryNoAgents => '这台机器上没有可以续接对话的 agent。';

  @override
  String get sessionsHistoryEmptyTitle => '没有可续接的对话';

  @override
  String sessionsHistoryEmptyBody(String agent, String project) {
    return '$agent 在 $project 里没有保存的对话。';
  }

  @override
  String get sessionsHistoryLoadFailed => '历史会话没取到';

  @override
  String sessionsHistoryMessages(int count) {
    return '$count 条消息';
  }

  @override
  String sessionsHistoryOriginal(String title) {
    return '原名「$title」';
  }

  @override
  String get sessionsHistoryRenameTitle => '给对话改名';

  @override
  String get sessionsHistoryRenameHelp => '留空就用回 agent 自己起的标题。';

  @override
  String get sessionsHistoryResetName => '恢复原名';

  @override
  String sessionsHistoryRenameFailed(String message) {
    return '改名失败：$message';
  }

  @override
  String sessionsHistoryResumeFailed(String message) {
    return '续接失败：$message';
  }

  @override
  String get sessionsHistoryResuming => '正在续接…';

  @override
  String get settingsTitle => '设置';

  @override
  String get settingsMachines => '机器';

  @override
  String get settingsPairNew => '配对新机器';

  @override
  String get settingsNoMachines => '还没有机器。配对一台，它上面的会话就会出现在这里。';

  @override
  String get settingsReorderHint => '长按一台机器拖动，调整顺序。';

  @override
  String settingsOpenMachine(String name) {
    return '$name 的详情';
  }

  @override
  String get settingsAppearance => '外观';

  @override
  String get settingsThemeSystem => '跟随系统';

  @override
  String get settingsThemeLight => '浅色';

  @override
  String get settingsThemeDark => '深色';

  @override
  String get settingsTerminal => '终端';

  @override
  String get settingsTerminalFontSize => '字号';

  @override
  String settingsTerminalFontValue(int size) {
    return '$size pt';
  }

  @override
  String get settingsTerminalFontSmaller => '字号调小';

  @override
  String get settingsTerminalFontLarger => '字号调大';

  @override
  String get settingsTerminalPreview => '~/code/smelt \$ cargo test';

  @override
  String get settingsAbout => '关于';

  @override
  String get settingsAboutTagline => '在手机上批准、回答、跟进你机器上的 agent。';

  @override
  String get settingsDeviceId => '本机设备 ID';

  @override
  String get settingsLicenses => '开源许可';

  @override
  String get shellPickAgentHint => '选一个智能体或自动化，在这里看详情。';

  @override
  String get terminalStateConnecting => '连接中…';

  @override
  String get terminalStateConnected => '实时';

  @override
  String get terminalStateReconnecting => '重连中…';

  @override
  String get terminalStateEnded => '已结束';

  @override
  String get terminalCopyOutput => '复制输出';

  @override
  String get terminalCopySelection => '复制选中内容';

  @override
  String get terminalTextSize => '字号';

  @override
  String terminalTextSizePoints(int size) {
    return '$size pt';
  }

  @override
  String get terminalShowKeyboard => '显示键盘';

  @override
  String get terminalHideKeyboard => '收起键盘';

  @override
  String get terminalKeyboardUnavailable => '现在不能输入';

  @override
  String get terminalNothingToCopy => '没有可复制的内容';

  @override
  String get terminalCopyFailed => '复制失败';

  @override
  String get terminalNothingToPaste => '剪贴板是空的';

  @override
  String get terminalPasteFailed => '读不到剪贴板';

  @override
  String get terminalOpenLinkFailed => '打不开这个链接';

  @override
  String get terminalSelectAll => '全选';

  @override
  String get terminalPaste => '粘贴';

  @override
  String get terminalOpenLink => '打开链接';

  @override
  String get terminalJumpToLatest => '最新';

  @override
  String get terminalJumpToLatestHint => '回到最新输出';

  @override
  String get terminalLoadingHistory => '正在加载更早的输出';

  @override
  String get terminalSessionEnded => '这个终端会话已结束。';

  @override
  String get terminalConnectionLost => '终端连接丢了，可能已在那台机器上关掉。';

  @override
  String get terminalConnectionDropped => '连接断开，正在重连…';

  @override
  String get terminalKeyEscape => 'Esc 键';

  @override
  String get terminalKeyCtrl => 'Ctrl，作用于下一个按键';

  @override
  String get terminalKeyTab => 'Tab 键';

  @override
  String get terminalKeyShiftTab => 'Shift+Tab';

  @override
  String get terminalKeyInterrupt => '中断（Ctrl+C）';

  @override
  String get terminalKeyLeft => '左';

  @override
  String get terminalKeyDown => '下';

  @override
  String get terminalKeyUp => '上';

  @override
  String get terminalKeyRight => '右';

  @override
  String get terminalKeyNewLine => 'Shift+Enter（换行）';

  @override
  String get terminalKeyPageUp => '上翻页';

  @override
  String get terminalKeyPageDown => '下翻页';
}
