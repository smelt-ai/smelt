// ignore: unused_import
import 'package:intl/intl.dart' as intl;
import 'l10n.dart';

// ignore_for_file: type=lint

/// The translations for English (`en`).
class L10nEn extends L10n {
  L10nEn([String locale = 'en']) : super(locale);

  @override
  String get agentsTitle => 'Agents';

  @override
  String get agentsSegmentAgents => 'Agents';

  @override
  String get agentsSegmentAutomations => 'Automations';

  @override
  String get agentsNoMachinesTitle => 'No machines yet';

  @override
  String get agentsNoMachinesBody =>
      'Pair a machine in Settings. Its agents and automations show up here.';

  @override
  String get agentsOfflineNote => 'Shows up once this machine is back online.';

  @override
  String agentsLoadFailed(String message) {
    return 'Couldn\'t load: $message';
  }

  @override
  String get agentsEmptyAgents =>
      'No agents on this machine yet. Create them in Smelt on the desktop.';

  @override
  String get agentsEmptyAutomations =>
      'No automations on this machine yet. Create them in Smelt on the desktop.';

  @override
  String get agentsReadOnly =>
      'Agents and automations are edited in Smelt on the desktop.';

  @override
  String agentsGone(String machine) {
    return 'This item is no longer on $machine.';
  }

  @override
  String agentsMachineOffline(String machine) {
    return '$machine is offline. Actions are available once it reconnects.';
  }

  @override
  String agentsEngine(String engine) {
    return 'Runs on $engine';
  }

  @override
  String get agentsPrompt => 'Standing instructions';

  @override
  String get agentsNoPrompt => 'No standing instructions.';

  @override
  String get agentsPlugins => 'Plugins';

  @override
  String get agentsContext => 'Context';

  @override
  String get agentsStartConversation => 'Start conversation';

  @override
  String get agentsStarting => 'Starting…';

  @override
  String agentsStartFailed(String message) {
    return 'Couldn\'t start the conversation: $message';
  }

  @override
  String get agentsStartPending =>
      'The conversation was created but hasn\'t shown up yet. Find it in Sessions shortly.';

  @override
  String get agentsCannotStart =>
      'This machine can\'t start a conversation with this agent: its engine isn\'t available here.';

  @override
  String agentsNextRun(String when) {
    return 'Next $when';
  }

  @override
  String get agentsNextRunTitle => 'Next run';

  @override
  String get agentsPaused => 'Paused';

  @override
  String get agentsDue => 'due now';

  @override
  String agentsInMinutes(int n) {
    String _temp0 = intl.Intl.pluralLogic(
      n,
      locale: localeName,
      other: 'in $n min',
      one: 'in 1 min',
    );
    return '$_temp0';
  }

  @override
  String agentsToday(String time) {
    return 'today $time';
  }

  @override
  String agentsTomorrow(String time) {
    return 'tomorrow $time';
  }

  @override
  String agentsYesterday(String time) {
    return 'yesterday $time';
  }

  @override
  String agentsOnDate(String date, String time) {
    return '$date $time';
  }

  @override
  String get agentsNeverRun => 'Never run';

  @override
  String agentsScheduleDaily(String time) {
    return 'Daily at $time';
  }

  @override
  String agentsScheduleEveryMinutes(int n) {
    String _temp0 = intl.Intl.pluralLogic(
      n,
      locale: localeName,
      other: 'Every $n min',
      one: 'Every minute',
    );
    return '$_temp0';
  }

  @override
  String agentsScheduleEveryHours(int n) {
    String _temp0 = intl.Intl.pluralLogic(
      n,
      locale: localeName,
      other: 'Every $n h',
      one: 'Every hour',
    );
    return '$_temp0';
  }

  @override
  String agentsScheduleWeekly(String days, String time) {
    return '$days at $time';
  }

  @override
  String agentsScheduleDuring(String days, String start, String end, int n) {
    return '$days $start–$end, every $n min';
  }

  @override
  String get agentsDaysEveryDay => 'Every day';

  @override
  String get agentsDaysWeekdays => 'Weekdays';

  @override
  String get agentsDaysWeekends => 'Weekends';

  @override
  String get agentsDaysNone => 'No days';

  @override
  String get agentsDaysSeparator => ', ';

  @override
  String agentsWeekday(String day) {
    String _temp0 = intl.Intl.selectLogic(day, {
      'mon': 'Mon',
      'tue': 'Tue',
      'wed': 'Wed',
      'thu': 'Thu',
      'fri': 'Fri',
      'sat': 'Sat',
      'other': 'Sun',
    });
    return '$_temp0';
  }

  @override
  String agentsTriggerEvent(String topic) {
    return 'When “$topic” happens';
  }

  @override
  String get agentsTriggerWebhook => 'Webhook call';

  @override
  String get agentsTriggerNone => 'No trigger';

  @override
  String get agentsRunQueued => 'Queued';

  @override
  String get agentsRunRunning => 'Running';

  @override
  String get agentsRunAwaitingApproval => 'Waiting for approval';

  @override
  String get agentsRunWaitingForUser => 'Waiting for you';

  @override
  String get agentsRunCompleted => 'Succeeded';

  @override
  String get agentsRunFailed => 'Failed';

  @override
  String get agentsRunCancelled => 'Cancelled';

  @override
  String get agentsRunSkipped => 'Skipped';

  @override
  String get agentsSourceManual => 'Run by hand';

  @override
  String get agentsSourceScheduled => 'On schedule';

  @override
  String get agentsSourceWebhook => 'Webhook';

  @override
  String get agentsSourceEvent => 'Event';

  @override
  String get agentsEnabled => 'Enabled';

  @override
  String agentsEnableFailed(String name, String message) {
    return 'Couldn\'t turn on “$name”: $message';
  }

  @override
  String agentsDisableFailed(String name, String message) {
    return 'Couldn\'t pause “$name”: $message';
  }

  @override
  String get agentsTrigger => 'Trigger';

  @override
  String get agentsAction => 'Action';

  @override
  String agentsActionAgent(String name) {
    return 'Agent “$name” does the work';
  }

  @override
  String get agentsActionAgentMissing => 'The agent that ran this was deleted';

  @override
  String get agentsActionShell => 'Runs a shell command';

  @override
  String get agentsLastRun => 'Last run';

  @override
  String get agentsRunStartedAt => 'Started';

  @override
  String get agentsRunFinishedAt => 'Finished';

  @override
  String get agentsRunDuration => 'Took';

  @override
  String get agentsRunSource => 'Triggered by';

  @override
  String get agentsOpenRun => 'Open this run\'s session';

  @override
  String get agentsRunNow => 'Run once now';

  @override
  String agentsRunNowStarted(String name) {
    return '“$name” started';
  }

  @override
  String agentsRunNowFailed(String name, String message) {
    return 'Couldn\'t run “$name”: $message';
  }

  @override
  String get appName => 'Smelt';

  @override
  String get navPass => 'Now';

  @override
  String get navSessions => 'Sessions';

  @override
  String get navAgents => 'Agents';

  @override
  String get navSettings => 'Settings';

  @override
  String get commonCancel => 'Cancel';

  @override
  String get commonDone => 'Done';

  @override
  String get commonSave => 'Save';

  @override
  String get commonRetry => 'Retry';

  @override
  String get commonOpen => 'Open';

  @override
  String get commonClose => 'Close';

  @override
  String get commonDelete => 'Delete';

  @override
  String get commonRename => 'Rename';

  @override
  String get commonCopy => 'Copy';

  @override
  String get commonCopied => 'Copied';

  @override
  String get commonMore => 'More';

  @override
  String get commonNew => 'New';

  @override
  String get commonAllow => 'Allow';

  @override
  String get commonDeny => 'Deny';

  @override
  String get commonAnswer => 'Answer';

  @override
  String get commonView => 'View';

  @override
  String get commonLoading => 'Loading…';

  @override
  String get commonName => 'Name';

  @override
  String machineOnlineLan(int ms) {
    return 'Local · $ms ms';
  }

  @override
  String machineOnlineDirect(int ms) {
    return 'Direct · $ms ms';
  }

  @override
  String machineOnlineRelay(int ms) {
    return 'Relay · $ms ms';
  }

  @override
  String get machineOnline => 'Online';

  @override
  String get machineConnecting => 'Connecting…';

  @override
  String get machineReconnecting => 'Reconnecting…';

  @override
  String get machineSyncing => 'Syncing…';

  @override
  String get machineOffline => 'Offline';

  @override
  String machineStatusCached(String status, String time) {
    return '$status · showing data from $time';
  }

  @override
  String get machineRevoked => 'This phone was removed from this machine';

  @override
  String get machineIncompatible => 'Machine needs an update';

  @override
  String get timeJustNow => 'just now';

  @override
  String timeMinutesAgo(int n) {
    return '$n min ago';
  }

  @override
  String timeHoursAgo(int n) {
    return '$n h ago';
  }

  @override
  String timeDaysAgo(int n) {
    return '$n d ago';
  }

  @override
  String get phaseConnecting => 'Starting';

  @override
  String get phaseThinking => 'Thinking';

  @override
  String get phaseExecutingTool => 'Running a tool';

  @override
  String get phaseAwaitingApproval => 'Waiting for approval';

  @override
  String get phaseWaitingForUser => 'Waiting for you';

  @override
  String get phaseSucceeded => 'Done';

  @override
  String get phaseFailed => 'Failed';

  @override
  String get phaseIdle => 'Idle';

  @override
  String get phaseDead => 'Ended';

  @override
  String get kindConversation => 'Conversation';

  @override
  String get kindTerminal => 'Terminal';

  @override
  String get conversationUntitled => 'Untitled conversation';

  @override
  String get conversationModelAndSettings => 'Model & settings';

  @override
  String get conversationSkills => 'Skills';

  @override
  String get conversationCopyInfo => 'Copy session info';

  @override
  String conversationInfoSession(String id) {
    return 'Session $id';
  }

  @override
  String get conversationLoading => 'Opening conversation…';

  @override
  String get conversationUnavailableTitle => 'Can\'t open this conversation';

  @override
  String get conversationUnavailableDefault =>
      'This conversation isn\'t running on the machine.';

  @override
  String get conversationErrorDefault => 'Couldn\'t load this conversation.';

  @override
  String get conversationCachedSaved => 'Showing the last saved data';

  @override
  String get conversationEmptyTitle => 'No messages yet';

  @override
  String get conversationEmptyBody => 'Send a message to get started.';

  @override
  String get conversationLoadEarlier => 'Load earlier messages';

  @override
  String get conversationJumpToLatest => 'Back to latest';

  @override
  String get conversationInterrupted => 'Interrupted';

  @override
  String get conversationThought => 'Thought';

  @override
  String get conversationThinkingPreview => 'Thinking…';

  @override
  String get conversationShowThought => 'Show thought';

  @override
  String get conversationHideThought => 'Hide thought';

  @override
  String get conversationCopyAnswer => 'Copy answer';

  @override
  String conversationTook(String duration) {
    return 'Took $duration';
  }

  @override
  String get conversationCompleted => 'Completed';

  @override
  String get conversationCompletionFailed => 'Completion failed';

  @override
  String get conversationTaskCompleted => 'Task completed.';

  @override
  String conversationToolCalls(int count) {
    String _temp0 = intl.Intl.pluralLogic(
      count,
      locale: localeName,
      other: '$count tool calls',
      one: '1 tool call',
    );
    return '$_temp0';
  }

  @override
  String conversationToolFailedCount(int count) {
    return '$count failed';
  }

  @override
  String get conversationToolRead => 'Read';

  @override
  String get conversationToolEdit => 'Edit';

  @override
  String get conversationToolDelete => 'Delete';

  @override
  String get conversationToolMove => 'Move';

  @override
  String get conversationToolSearch => 'Search';

  @override
  String get conversationToolExecute => 'Run';

  @override
  String get conversationToolThink => 'Think';

  @override
  String get conversationToolFetch => 'Fetch';

  @override
  String get conversationToolSwitchMode => 'Mode';

  @override
  String get conversationToolCollaborate => 'Delegate';

  @override
  String get conversationToolReview => 'Review';

  @override
  String get conversationToolImage => 'Image';

  @override
  String get conversationToolCompact => 'Compact';

  @override
  String get conversationToolWait => 'Wait';

  @override
  String get conversationToolOther => 'Tool';

  @override
  String get conversationToolPending => 'Not started';

  @override
  String get conversationToolInProgress => 'Running';

  @override
  String get conversationToolDone => 'Done';

  @override
  String get conversationToolFailed => 'Failed';

  @override
  String get conversationShowOutput => 'Show output';

  @override
  String get conversationHideOutput => 'Hide output';

  @override
  String conversationExitCode(int code) {
    return 'Exit code $code';
  }

  @override
  String get conversationOutputTruncated => 'Output truncated';

  @override
  String conversationShowAllLines(int count) {
    return 'Show all $count lines';
  }

  @override
  String get conversationCollapse => 'Collapse';

  @override
  String conversationUnchangedLines(int count) {
    String _temp0 = intl.Intl.pluralLogic(
      count,
      locale: localeName,
      other: '$count unchanged lines',
      one: '1 unchanged line',
    );
    return '$_temp0';
  }

  @override
  String get conversationNewFile => 'New file';

  @override
  String conversationPlan(int done, int total) {
    return 'Plan · $done/$total';
  }

  @override
  String get conversationPlanPending => 'To do';

  @override
  String get conversationPlanInProgress => 'In progress';

  @override
  String get conversationPlanCompleted => 'Done';

  @override
  String get conversationTaskBackground => 'Background task';

  @override
  String get conversationTaskPeerMessage => 'Message';

  @override
  String get conversationTaskSubagent => 'Subagent';

  @override
  String get conversationTaskRunning => 'Running';

  @override
  String get conversationTaskCompletedStatus => 'Finished';

  @override
  String get conversationTaskFailed => 'Failed';

  @override
  String get conversationTaskTimedOut => 'Timed out';

  @override
  String get conversationTaskStopped => 'Stopped';

  @override
  String get conversationImageOpen => 'Open image';

  @override
  String get conversationImageClose => 'Close image';

  @override
  String get conversationImageUnavailable => 'Image not available';

  @override
  String get conversationImageUndecodable => 'This image can\'t be displayed.';

  @override
  String conversationImageLoadFailed(String message) {
    return 'Couldn\'t load the original: $message';
  }

  @override
  String get conversationStatusEnded => 'Session ended';

  @override
  String conversationHint(String agent) {
    return 'Message $agent';
  }

  @override
  String get conversationHintOffline =>
      'Machine is offline. Your draft is kept.';

  @override
  String get conversationHintTicket => 'Handle the ticket above first';

  @override
  String get conversationHintEnded => 'This session has ended';

  @override
  String get conversationHintSending => 'Waiting for the machine to confirm…';

  @override
  String get conversationSend => 'Send';

  @override
  String get conversationStop => 'Stop this turn';

  @override
  String get conversationAttachImage => 'Add image';

  @override
  String get conversationNoImageSupport => 'This agent can\'t take images';

  @override
  String get conversationCommands => 'Commands and skills';

  @override
  String get conversationRemoveImage => 'Remove image';

  @override
  String conversationImagesSkipped(int count) {
    String _temp0 = intl.Intl.pluralLogic(
      count,
      locale: localeName,
      other: '$count images were over 5 MB and were skipped',
      one: '1 image was over 5 MB and was skipped',
    );
    return '$_temp0';
  }

  @override
  String conversationAttachFailed(String message) {
    return 'Couldn\'t add the image: $message';
  }

  @override
  String get conversationUnconfirmed =>
      'Delivery wasn\'t confirmed. Check the conversation, then send again.';

  @override
  String conversationRejected(String message) {
    return 'Not sent: $message';
  }

  @override
  String conversationDraftSaveFailed(String message) {
    return 'Couldn\'t save the draft: $message';
  }

  @override
  String get conversationResendTitle => 'Send again?';

  @override
  String get conversationResendBody =>
      'The last attempt may already have reached the agent. Check the conversation before sending it again.';

  @override
  String get conversationResendConfirm => 'Send again';

  @override
  String conversationContextUsage(int percent, String used, String total) {
    return 'Context $percent% · $used of $total tokens';
  }

  @override
  String get conversationModel => 'Model';

  @override
  String get conversationProvider => 'Provider';

  @override
  String get conversationOn => 'On';

  @override
  String get conversationOff => 'Off';

  @override
  String get conversationSkillsEmpty =>
      'No skills are loaded in this conversation.';

  @override
  String get conversationSkillsNote => 'Skills are managed on the computer.';

  @override
  String get conversationCommandsTitle => 'Commands';

  @override
  String get conversationNoSettings =>
      'This agent has no settings to change here.';

  @override
  String get conversationTicketApproval => 'Approval';

  @override
  String get conversationTicketQuestion => 'Question';

  @override
  String conversationTicketCount(int index, int count) {
    return '$index of $count';
  }

  @override
  String get conversationTicketCollapse => 'Fold the ticket down';

  @override
  String get conversationTicketExpand => 'Open the ticket';

  @override
  String conversationActionFailed(String message) {
    return 'Couldn\'t send: $message';
  }

  @override
  String get launchTitle => 'New session';

  @override
  String get launchBack => 'Back';

  @override
  String get launchStepMachine => 'Choose a machine';

  @override
  String get launchStepProject => 'Choose a project';

  @override
  String get launchNoMachineOnline =>
      'No machine is online. A session starts on the machine itself, so it has to be connected.';

  @override
  String launchMachineUnavailable(String status) {
    return '$status · available once it reconnects';
  }

  @override
  String get launchLoadFailed => 'Couldn\'t load this machine\'s projects';

  @override
  String get launchNoProjects =>
      'No projects open on this machine. Open one in Smelt on the computer first.';

  @override
  String get launchNoProject => 'No project';

  @override
  String get launchNoProjectHint =>
      'Talk to one of your agents in its own space';

  @override
  String get launchSectionPinned => 'Pinned';

  @override
  String get launchSectionConversation => 'Conversations';

  @override
  String get launchSectionTerminal => 'Terminals';

  @override
  String get launchSectionAgents => 'Agents';

  @override
  String launchAgentSubtitle(String engine) {
    return 'Agent · $engine';
  }

  @override
  String get launchNoActions =>
      'Nothing to start here yet. Update Smelt on the computer, then try again.';

  @override
  String launchStarting(String label) {
    return 'Starting $label…';
  }

  @override
  String get launchWaiting => 'Started. Opening it…';

  @override
  String launchFailedTitle(String label) {
    return 'Couldn\'t start $label';
  }

  @override
  String launchFailed(String message) {
    return 'Couldn\'t start the session: $message';
  }

  @override
  String launchNotListed(String machine) {
    return 'It started on $machine but hasn\'t shown up yet. Look for it in Sessions in a moment.';
  }

  @override
  String get machinesOsMacos => 'macOS';

  @override
  String get machinesOsLinux => 'Linux';

  @override
  String get machinesOsWindows => 'Windows';

  @override
  String get machinesOsUnknown => 'Unknown system';

  @override
  String get machinesName => 'Name';

  @override
  String machinesHostname(String hostname) {
    return 'Host name: $hostname';
  }

  @override
  String get machinesRenameTitle => 'Rename machine';

  @override
  String machinesRenameEmptyHint(String hostname) {
    return 'Leave empty to use the host name ($hostname)';
  }

  @override
  String machinesRenameFailed(String error) {
    return 'Couldn\'t rename: $error';
  }

  @override
  String get machinesAccent => 'Label color';

  @override
  String get machinesAccentFooter =>
      'Every session from this machine carries this color.';

  @override
  String get machinesAccentBlue => 'Blue';

  @override
  String get machinesAccentTeal => 'Teal';

  @override
  String get machinesAccentViolet => 'Violet';

  @override
  String get machinesAccentPink => 'Pink';

  @override
  String get machinesAccentOlive => 'Olive';

  @override
  String get machinesAccentIndigo => 'Indigo';

  @override
  String get machinesAccentSky => 'Sky';

  @override
  String get machinesAccentBrown => 'Brown';

  @override
  String machinesAccentOther(int n) {
    return 'Color $n';
  }

  @override
  String machinesAccentFailed(String error) {
    return 'Couldn\'t change the color: $error';
  }

  @override
  String get machinesLink => 'Connection';

  @override
  String get machinesStatus => 'Status';

  @override
  String get machinesPath => 'Path';

  @override
  String get machinesPathLan => 'Local network';

  @override
  String get machinesPathDirect => 'Direct';

  @override
  String get machinesPathRelay => 'Relay';

  @override
  String get machinesPathUnknown => 'Not chosen yet';

  @override
  String get machinesRtt => 'Round trip';

  @override
  String machinesRttValue(int ms) {
    return '$ms ms';
  }

  @override
  String get machinesEndpoint => 'Endpoint ID';

  @override
  String get machinesEndpointCopy => 'Copy endpoint ID';

  @override
  String get machinesRelay => 'Relay server';

  @override
  String get machinesRelayDefault => 'Default public relays';

  @override
  String get machinesVersion => 'Runtime version';

  @override
  String get machinesVersionUnknown => 'Not connected yet';

  @override
  String get machinesSystem => 'System';

  @override
  String get machinesLastError => 'Last error';

  @override
  String get machinesRelayMixedNote =>
      'Your machines use different relay settings, so on iOS a self-hosted relay may be unreachable while a system proxy or VPN is on.';

  @override
  String get machinesReconnect => 'Reconnect now';

  @override
  String get machinesReconnectBusy => 'Reconnecting…';

  @override
  String machinesReconnected(String name) {
    return 'Connected to $name';
  }

  @override
  String machinesReconnectOffline(String name) {
    return 'Still can\'t reach $name. It keeps retrying in the background.';
  }

  @override
  String machinesReconnectFailed(String error) {
    return 'Couldn\'t reconnect: $error';
  }

  @override
  String machinesRevokedBody(String name) {
    return '$name no longer accepts this phone: it was removed on that machine, or its device list was reset. Make a new pairing code there and pair again. The name and label color stay.';
  }

  @override
  String machinesIncompatibleBody(String name) {
    return 'This app and Smelt on $name are on incompatible versions. Update the older one, then reconnect. If it still fails, pair again.';
  }

  @override
  String get machinesRepair => 'Pair again';

  @override
  String get machinesForget => 'Forget this machine';

  @override
  String machinesForgetTitle(String name) {
    return 'Forget $name?';
  }

  @override
  String machinesForgetBody(String name) {
    return 'Removes $name from this phone, along with its drafts and offline cache. Whether this phone may connect is managed on $name itself: to revoke access, remove this phone from its device list. You can pair again later.';
  }

  @override
  String get machinesForgetConfirm => 'Forget';

  @override
  String machinesForgotten(String name) {
    return 'Forgot $name';
  }

  @override
  String machinesForgetFailed(String error) {
    return 'Couldn\'t forget the machine: $error';
  }

  @override
  String get machinesGone => 'This machine is no longer on this phone.';

  @override
  String get machinesPairTitle => 'Pair a machine';

  @override
  String get machinesPairGuide =>
      'Scan the code from Smelt on your computer (Settings → Mobile Remote), or the one printed by “smeltd pair” on a server.';

  @override
  String get machinesPairPaste => 'Paste pairing code';

  @override
  String get machinesPairPasteHint => 'smelt+iroh://…';

  @override
  String get machinesPairFromClipboard => 'Paste from clipboard';

  @override
  String get machinesPairSubmit => 'Pair';

  @override
  String get machinesPairTorchOn => 'Turn on flashlight';

  @override
  String get machinesPairTorchOff => 'Turn off flashlight';

  @override
  String get machinesPairNotACode => 'That isn\'t a Smelt pairing code.';

  @override
  String get machinesPairBusyNew => 'Pairing with a new machine…';

  @override
  String machinesPairBusyAgain(String name) {
    return 'Pairing with $name again…';
  }

  @override
  String get machinesPairBusyBody => 'Your other machines stay connected.';

  @override
  String machinesPaired(String name) {
    return 'Connected to $name';
  }

  @override
  String get machinesPairFailed => 'Couldn\'t pair';

  @override
  String get machinesPairErrorExpired =>
      'This pairing code has expired or was already used. Make a new one on the machine and scan it.';

  @override
  String get machinesPairErrorIncompatible =>
      'This app and Smelt on that machine are on incompatible versions. Update the older one, then pair with a new code.';

  @override
  String get machinesPairErrorOffline =>
      'Couldn\'t reach the machine. Check that it\'s awake and both sides are online, then try again. If the code has expired, make a new one.';

  @override
  String get machinesPairErrorRejected =>
      'The machine turned this phone away. Make a new pairing code on it and try again.';

  @override
  String get machinesPairErrorCancelled =>
      'Pairing was interrupted. Try again.';

  @override
  String machinesPairErrorOther(String error) {
    return 'Pairing failed: $error';
  }

  @override
  String get machinesPairScanAgain => 'Scan again';

  @override
  String get machinesCameraDenied =>
      'Smelt can\'t use the camera. Allow camera access in the system settings, or paste the pairing code instead.';

  @override
  String get machinesCameraUnavailable =>
      'The camera isn\'t available. Paste the pairing code instead.';

  @override
  String get passTitle => 'Now';

  @override
  String get passNewSession => 'New session';

  @override
  String get passRailEmpty => 'Nothing is waiting for you.';

  @override
  String passTicketsWaiting(int count) {
    String _temp0 = intl.Intl.pluralLogic(
      count,
      locale: localeName,
      other: '$count waiting for you',
      one: '1 waiting for you',
    );
    return '$_temp0';
  }

  @override
  String passTicketNumber(int n) {
    return '#$n';
  }

  @override
  String passWantsToRun(String agent) {
    return '$agent wants to run';
  }

  @override
  String passWantsTo(String agent) {
    return '$agent asks';
  }

  @override
  String passHasQuestion(String agent) {
    return '$agent has a question';
  }

  @override
  String passFailed(String agent) {
    return '$agent stopped with an error';
  }

  @override
  String get passContextYou => 'You';

  @override
  String get passLoadingDetails => 'Fetching what it needs…';

  @override
  String get passMoreOptions => 'More options';

  @override
  String passStampAllowed(String time) {
    return 'Allowed · $time';
  }

  @override
  String passStampDenied(String time) {
    return 'Denied · $time';
  }

  @override
  String passStampAnswered(String time) {
    return 'Answered · $time';
  }

  @override
  String get passStationIdle => 'Nothing running here.';

  @override
  String get passSeeAllSessions => 'All sessions';

  @override
  String get passRunning => 'Running';

  @override
  String get passJustFinished => 'Just finished';

  @override
  String get passRecent => 'Recent';

  @override
  String get passRepair => 'Pair again';

  @override
  String get passNoMachinesTitle => 'Pair your first machine';

  @override
  String get passNoMachinesBody =>
      'On your Mac, open Smelt → Settings → Mobile Remote, or run `smeltd pair` on a server, then scan the code.';

  @override
  String get passScanCode => 'Scan pairing code';

  @override
  String passTriageTitle(int index, int count) {
    return '$index of $count';
  }

  @override
  String get passTriageDone => 'All caught up';

  @override
  String get passTriageDoneBody => 'Every ticket has been handled.';

  @override
  String get passOpenConversation => 'Open conversation';

  @override
  String passWorkingDir(String dir) {
    return 'In $dir';
  }

  @override
  String passGrantRoot(String path) {
    return 'Grants access to $path';
  }

  @override
  String passActionFailed(String message) {
    return 'Couldn\'t send: $message';
  }

  @override
  String get approvalAllowOnce => 'Allow once';

  @override
  String get approvalAllowAlways => 'Always allow';

  @override
  String get approvalRejectOnce => 'Deny';

  @override
  String get approvalRejectAlways => 'Always deny';

  @override
  String get elicitationSubmit => 'Send answer';

  @override
  String get elicitationDismiss => 'Skip';

  @override
  String get elicitationRequired => 'Required';

  @override
  String get elicitationOpenLink => 'Open link';

  @override
  String get elicitationTextHint => 'Type your answer';

  @override
  String get sessionsTitle => 'Sessions';

  @override
  String get sessionsNewSession => 'New session';

  @override
  String get sessionsFilterAll => 'All';

  @override
  String get sessionsFilterMachines => 'Show sessions from';

  @override
  String get sessionsSearchHint => 'Search titles, projects, agents';

  @override
  String get sessionsSearchClear => 'Clear search';

  @override
  String get sessionsAgentChats => 'Agent conversations';

  @override
  String get sessionsAutomationRuns => 'Automation runs';

  @override
  String get sessionsNoProject => 'No project';

  @override
  String get sessionsIdleProjects => 'Projects without open sessions';

  @override
  String get sessionsHistoryRow => 'History';

  @override
  String get sessionsMachineEmpty => 'No sessions on this machine.';

  @override
  String get sessionsMachineWaiting =>
      'Waiting for this machine\'s session list…';

  @override
  String get sessionsNoMachinesTitle => 'No machines yet';

  @override
  String get sessionsNoMachinesBody =>
      'Pair a machine from Now or Settings, and its sessions show up here.';

  @override
  String sessionsNoMatchesTitle(String query) {
    return 'Nothing matches “$query”';
  }

  @override
  String get sessionsNoMatchesBody =>
      'Search looks at titles, projects and agents.';

  @override
  String get sessionsUntitledConversation => 'Untitled conversation';

  @override
  String get sessionsUntitledTerminal => 'Terminal';

  @override
  String get sessionsDeleteTerminalTitle => 'Delete this terminal?';

  @override
  String sessionsDeleteTerminalBody(String title, String machine) {
    return '“$title” closes on $machine and its shell process ends.';
  }

  @override
  String get sessionsDeleteConversationTitle => 'Delete this conversation?';

  @override
  String sessionsDeleteConversationBody(String title, String machine) {
    return '“$title” stops on $machine and leaves the session list. The agent\'s transcript stays in History.';
  }

  @override
  String sessionsDeleteFailed(String message) {
    return 'Couldn\'t delete: $message';
  }

  @override
  String get sessionsHistoryTitle => 'History';

  @override
  String get sessionsHistoryNoAgents =>
      'This machine has no agents to resume conversations with.';

  @override
  String get sessionsHistoryEmptyTitle => 'Nothing to resume';

  @override
  String sessionsHistoryEmptyBody(String agent, String project) {
    return '$agent has no saved conversations in $project.';
  }

  @override
  String get sessionsHistoryLoadFailed => 'Couldn\'t load history';

  @override
  String sessionsHistoryMessages(int count) {
    String _temp0 = intl.Intl.pluralLogic(
      count,
      locale: localeName,
      other: '$count messages',
      one: '1 message',
    );
    return '$_temp0';
  }

  @override
  String sessionsHistoryOriginal(String title) {
    return 'Originally “$title”';
  }

  @override
  String get sessionsHistoryRenameTitle => 'Rename conversation';

  @override
  String get sessionsHistoryRenameHelp =>
      'Leave empty to go back to the agent\'s own title.';

  @override
  String get sessionsHistoryResetName => 'Use original name';

  @override
  String sessionsHistoryRenameFailed(String message) {
    return 'Couldn\'t rename: $message';
  }

  @override
  String sessionsHistoryResumeFailed(String message) {
    return 'Couldn\'t resume: $message';
  }

  @override
  String get sessionsHistoryResuming => 'Resuming…';

  @override
  String get settingsTitle => 'Settings';

  @override
  String get settingsMachines => 'Machines';

  @override
  String get settingsPairNew => 'Pair a new machine';

  @override
  String get settingsNoMachines =>
      'No machines yet. Pair one and its sessions show up here.';

  @override
  String get settingsReorderHint =>
      'Touch and hold a machine to drag it into a new order.';

  @override
  String settingsOpenMachine(String name) {
    return 'Details for $name';
  }

  @override
  String get settingsAppearance => 'Appearance';

  @override
  String get settingsThemeSystem => 'System';

  @override
  String get settingsThemeLight => 'Light';

  @override
  String get settingsThemeDark => 'Dark';

  @override
  String get settingsTerminal => 'Terminal';

  @override
  String get settingsTerminalFontSize => 'Font size';

  @override
  String settingsTerminalFontValue(int size) {
    return '$size pt';
  }

  @override
  String get settingsTerminalFontSmaller => 'Smaller text';

  @override
  String get settingsTerminalFontLarger => 'Larger text';

  @override
  String get settingsTerminalPreview => '~/code/smelt \$ cargo test';

  @override
  String get settingsAbout => 'About';

  @override
  String get settingsAboutTagline =>
      'Approve, answer and follow the agents on your machines.';

  @override
  String get settingsDeviceId => 'This device\'s ID';

  @override
  String get settingsLicenses => 'Open-source licenses';

  @override
  String get shellPickAgentHint =>
      'Pick an agent or automation to see its details.';

  @override
  String get terminalStateConnecting => 'Connecting…';

  @override
  String get terminalStateConnected => 'Live';

  @override
  String get terminalStateReconnecting => 'Reconnecting…';

  @override
  String get terminalStateEnded => 'Ended';

  @override
  String get terminalCopyOutput => 'Copy output';

  @override
  String get terminalCopySelection => 'Copy selection';

  @override
  String get terminalTextSize => 'Text size';

  @override
  String terminalTextSizePoints(int size) {
    return '$size pt';
  }

  @override
  String get terminalShowKeyboard => 'Show keyboard';

  @override
  String get terminalHideKeyboard => 'Hide keyboard';

  @override
  String get terminalKeyboardUnavailable => 'Can\'t type right now';

  @override
  String get terminalNothingToCopy => 'Nothing to copy';

  @override
  String get terminalCopyFailed => 'Couldn\'t copy';

  @override
  String get terminalNothingToPaste => 'The clipboard is empty';

  @override
  String get terminalPasteFailed => 'Couldn\'t read the clipboard';

  @override
  String get terminalOpenLinkFailed => 'Couldn\'t open the link';

  @override
  String get terminalSelectAll => 'Select all';

  @override
  String get terminalPaste => 'Paste';

  @override
  String get terminalOpenLink => 'Open link';

  @override
  String get terminalJumpToLatest => 'Latest';

  @override
  String get terminalJumpToLatestHint => 'Jump to the newest output';

  @override
  String get terminalLoadingHistory => 'Loading earlier output';

  @override
  String get terminalSessionEnded => 'This terminal session has ended.';

  @override
  String get terminalConnectionLost =>
      'Lost this terminal. It may have been closed on the machine.';

  @override
  String get terminalConnectionDropped => 'Connection dropped. Reconnecting…';

  @override
  String get terminalKeyEscape => 'Escape';

  @override
  String get terminalKeyCtrl => 'Ctrl, applies to the next key';

  @override
  String get terminalKeyTab => 'Tab';

  @override
  String get terminalKeyShiftTab => 'Shift+Tab';

  @override
  String get terminalKeyInterrupt => 'Interrupt (Ctrl+C)';

  @override
  String get terminalKeyLeft => 'Left';

  @override
  String get terminalKeyDown => 'Down';

  @override
  String get terminalKeyUp => 'Up';

  @override
  String get terminalKeyRight => 'Right';

  @override
  String get terminalKeyNewLine => 'Shift+Enter (new line)';

  @override
  String get terminalKeyPageUp => 'Page up';

  @override
  String get terminalKeyPageDown => 'Page down';
}
