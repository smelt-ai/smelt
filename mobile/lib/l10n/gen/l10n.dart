import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:intl/intl.dart' as intl;

import 'l10n_en.dart';
import 'l10n_zh.dart';

// ignore_for_file: type=lint

/// Callers can lookup localized strings with an instance of L10n
/// returned by `L10n.of(context)`.
///
/// Applications need to include `L10n.delegate()` in their app's
/// `localizationDelegates` list, and the locales they support in the app's
/// `supportedLocales` list. For example:
///
/// ```dart
/// import 'gen/l10n.dart';
///
/// return MaterialApp(
///   localizationsDelegates: L10n.localizationsDelegates,
///   supportedLocales: L10n.supportedLocales,
///   home: MyApplicationHome(),
/// );
/// ```
///
/// ## Update pubspec.yaml
///
/// Please make sure to update your pubspec.yaml to include the following
/// packages:
///
/// ```yaml
/// dependencies:
///   # Internationalization support.
///   flutter_localizations:
///     sdk: flutter
///   intl: any # Use the pinned version from flutter_localizations
///
///   # Rest of dependencies
/// ```
///
/// ## iOS Applications
///
/// iOS applications define key application metadata, including supported
/// locales, in an Info.plist file that is built into the application bundle.
/// To configure the locales supported by your app, you’ll need to edit this
/// file.
///
/// First, open your project’s ios/Runner.xcworkspace Xcode workspace file.
/// Then, in the Project Navigator, open the Info.plist file under the Runner
/// project’s Runner folder.
///
/// Next, select the Information Property List item, select Add Item from the
/// Editor menu, then select Localizations from the pop-up menu.
///
/// Select and expand the newly-created Localizations item then, for each
/// locale your application supports, add a new item and select the locale
/// you wish to add from the pop-up menu in the Value field. This list should
/// be consistent with the languages listed in the L10n.supportedLocales
/// property.
abstract class L10n {
  L10n(String locale)
    : localeName = intl.Intl.canonicalizedLocale(locale.toString());

  final String localeName;

  static L10n of(BuildContext context) {
    return Localizations.of<L10n>(context, L10n)!;
  }

  static const LocalizationsDelegate<L10n> delegate = _L10nDelegate();

  /// A list of this localizations delegate along with the default localizations
  /// delegates.
  ///
  /// Returns a list of localizations delegates containing this delegate along with
  /// GlobalMaterialLocalizations.delegate, GlobalCupertinoLocalizations.delegate,
  /// and GlobalWidgetsLocalizations.delegate.
  ///
  /// Additional delegates can be added by appending to this list in
  /// MaterialApp. This list does not have to be used at all if a custom list
  /// of delegates is preferred or required.
  static const List<LocalizationsDelegate<dynamic>> localizationsDelegates =
      <LocalizationsDelegate<dynamic>>[
        delegate,
        GlobalMaterialLocalizations.delegate,
        GlobalCupertinoLocalizations.delegate,
        GlobalWidgetsLocalizations.delegate,
      ];

  /// A list of this localizations delegate's supported locales.
  static const List<Locale> supportedLocales = <Locale>[
    Locale('en'),
    Locale('zh'),
  ];

  /// No description provided for @agentsTitle.
  ///
  /// In en, this message translates to:
  /// **'Agents'**
  String get agentsTitle;

  /// No description provided for @agentsSegmentAgents.
  ///
  /// In en, this message translates to:
  /// **'Agents'**
  String get agentsSegmentAgents;

  /// No description provided for @agentsSegmentAutomations.
  ///
  /// In en, this message translates to:
  /// **'Automations'**
  String get agentsSegmentAutomations;

  /// No description provided for @agentsNoMachinesTitle.
  ///
  /// In en, this message translates to:
  /// **'No machines yet'**
  String get agentsNoMachinesTitle;

  /// No description provided for @agentsNoMachinesBody.
  ///
  /// In en, this message translates to:
  /// **'Pair a machine in Settings. Its agents and automations show up here.'**
  String get agentsNoMachinesBody;

  /// No description provided for @agentsOfflineNote.
  ///
  /// In en, this message translates to:
  /// **'Shows up once this machine is back online.'**
  String get agentsOfflineNote;

  /// No description provided for @agentsLoadFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t load: {message}'**
  String agentsLoadFailed(String message);

  /// No description provided for @agentsEmptyAgents.
  ///
  /// In en, this message translates to:
  /// **'No agents on this machine yet. Create them in Smelt on the desktop.'**
  String get agentsEmptyAgents;

  /// No description provided for @agentsEmptyAutomations.
  ///
  /// In en, this message translates to:
  /// **'No automations on this machine yet. Create them in Smelt on the desktop.'**
  String get agentsEmptyAutomations;

  /// No description provided for @agentsReadOnly.
  ///
  /// In en, this message translates to:
  /// **'Agents and automations are edited in Smelt on the desktop.'**
  String get agentsReadOnly;

  /// No description provided for @agentsGone.
  ///
  /// In en, this message translates to:
  /// **'This item is no longer on {machine}.'**
  String agentsGone(String machine);

  /// No description provided for @agentsMachineOffline.
  ///
  /// In en, this message translates to:
  /// **'{machine} is offline. Actions are available once it reconnects.'**
  String agentsMachineOffline(String machine);

  /// No description provided for @agentsEngine.
  ///
  /// In en, this message translates to:
  /// **'Runs on {engine}'**
  String agentsEngine(String engine);

  /// No description provided for @agentsPrompt.
  ///
  /// In en, this message translates to:
  /// **'Standing instructions'**
  String get agentsPrompt;

  /// No description provided for @agentsNoPrompt.
  ///
  /// In en, this message translates to:
  /// **'No standing instructions.'**
  String get agentsNoPrompt;

  /// No description provided for @agentsPlugins.
  ///
  /// In en, this message translates to:
  /// **'Plugins'**
  String get agentsPlugins;

  /// No description provided for @agentsContext.
  ///
  /// In en, this message translates to:
  /// **'Context'**
  String get agentsContext;

  /// No description provided for @agentsStartConversation.
  ///
  /// In en, this message translates to:
  /// **'Start conversation'**
  String get agentsStartConversation;

  /// No description provided for @agentsStarting.
  ///
  /// In en, this message translates to:
  /// **'Starting…'**
  String get agentsStarting;

  /// No description provided for @agentsStartFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t start the conversation: {message}'**
  String agentsStartFailed(String message);

  /// No description provided for @agentsStartPending.
  ///
  /// In en, this message translates to:
  /// **'The conversation was created but hasn\'t shown up yet. Find it in Sessions shortly.'**
  String get agentsStartPending;

  /// No description provided for @agentsCannotStart.
  ///
  /// In en, this message translates to:
  /// **'This machine can\'t start a conversation with this agent: its engine isn\'t available here.'**
  String get agentsCannotStart;

  /// No description provided for @agentsNextRun.
  ///
  /// In en, this message translates to:
  /// **'Next {when}'**
  String agentsNextRun(String when);

  /// No description provided for @agentsNextRunTitle.
  ///
  /// In en, this message translates to:
  /// **'Next run'**
  String get agentsNextRunTitle;

  /// No description provided for @agentsPaused.
  ///
  /// In en, this message translates to:
  /// **'Paused'**
  String get agentsPaused;

  /// No description provided for @agentsDue.
  ///
  /// In en, this message translates to:
  /// **'due now'**
  String get agentsDue;

  /// No description provided for @agentsInMinutes.
  ///
  /// In en, this message translates to:
  /// **'{n, plural, =1{in 1 min} other{in {n} min}}'**
  String agentsInMinutes(int n);

  /// No description provided for @agentsToday.
  ///
  /// In en, this message translates to:
  /// **'today {time}'**
  String agentsToday(String time);

  /// No description provided for @agentsTomorrow.
  ///
  /// In en, this message translates to:
  /// **'tomorrow {time}'**
  String agentsTomorrow(String time);

  /// No description provided for @agentsYesterday.
  ///
  /// In en, this message translates to:
  /// **'yesterday {time}'**
  String agentsYesterday(String time);

  /// No description provided for @agentsOnDate.
  ///
  /// In en, this message translates to:
  /// **'{date} {time}'**
  String agentsOnDate(String date, String time);

  /// No description provided for @agentsNeverRun.
  ///
  /// In en, this message translates to:
  /// **'Never run'**
  String get agentsNeverRun;

  /// No description provided for @agentsScheduleDaily.
  ///
  /// In en, this message translates to:
  /// **'Daily at {time}'**
  String agentsScheduleDaily(String time);

  /// No description provided for @agentsScheduleEveryMinutes.
  ///
  /// In en, this message translates to:
  /// **'{n, plural, =1{Every minute} other{Every {n} min}}'**
  String agentsScheduleEveryMinutes(int n);

  /// No description provided for @agentsScheduleEveryHours.
  ///
  /// In en, this message translates to:
  /// **'{n, plural, =1{Every hour} other{Every {n} h}}'**
  String agentsScheduleEveryHours(int n);

  /// No description provided for @agentsScheduleWeekly.
  ///
  /// In en, this message translates to:
  /// **'{days} at {time}'**
  String agentsScheduleWeekly(String days, String time);

  /// No description provided for @agentsScheduleDuring.
  ///
  /// In en, this message translates to:
  /// **'{days} {start}–{end}, every {n} min'**
  String agentsScheduleDuring(String days, String start, String end, int n);

  /// No description provided for @agentsDaysEveryDay.
  ///
  /// In en, this message translates to:
  /// **'Every day'**
  String get agentsDaysEveryDay;

  /// No description provided for @agentsDaysWeekdays.
  ///
  /// In en, this message translates to:
  /// **'Weekdays'**
  String get agentsDaysWeekdays;

  /// No description provided for @agentsDaysWeekends.
  ///
  /// In en, this message translates to:
  /// **'Weekends'**
  String get agentsDaysWeekends;

  /// No description provided for @agentsDaysNone.
  ///
  /// In en, this message translates to:
  /// **'No days'**
  String get agentsDaysNone;

  /// No description provided for @agentsDaysSeparator.
  ///
  /// In en, this message translates to:
  /// **', '**
  String get agentsDaysSeparator;

  /// No description provided for @agentsWeekday.
  ///
  /// In en, this message translates to:
  /// **'{day, select, mon{Mon} tue{Tue} wed{Wed} thu{Thu} fri{Fri} sat{Sat} other{Sun}}'**
  String agentsWeekday(String day);

  /// No description provided for @agentsTriggerEvent.
  ///
  /// In en, this message translates to:
  /// **'When “{topic}” happens'**
  String agentsTriggerEvent(String topic);

  /// No description provided for @agentsTriggerWebhook.
  ///
  /// In en, this message translates to:
  /// **'Webhook call'**
  String get agentsTriggerWebhook;

  /// No description provided for @agentsTriggerNone.
  ///
  /// In en, this message translates to:
  /// **'No trigger'**
  String get agentsTriggerNone;

  /// No description provided for @agentsRunQueued.
  ///
  /// In en, this message translates to:
  /// **'Queued'**
  String get agentsRunQueued;

  /// No description provided for @agentsRunRunning.
  ///
  /// In en, this message translates to:
  /// **'Running'**
  String get agentsRunRunning;

  /// No description provided for @agentsRunAwaitingApproval.
  ///
  /// In en, this message translates to:
  /// **'Waiting for approval'**
  String get agentsRunAwaitingApproval;

  /// No description provided for @agentsRunWaitingForUser.
  ///
  /// In en, this message translates to:
  /// **'Waiting for you'**
  String get agentsRunWaitingForUser;

  /// No description provided for @agentsRunCompleted.
  ///
  /// In en, this message translates to:
  /// **'Succeeded'**
  String get agentsRunCompleted;

  /// No description provided for @agentsRunFailed.
  ///
  /// In en, this message translates to:
  /// **'Failed'**
  String get agentsRunFailed;

  /// No description provided for @agentsRunCancelled.
  ///
  /// In en, this message translates to:
  /// **'Cancelled'**
  String get agentsRunCancelled;

  /// No description provided for @agentsRunSkipped.
  ///
  /// In en, this message translates to:
  /// **'Skipped'**
  String get agentsRunSkipped;

  /// No description provided for @agentsSourceManual.
  ///
  /// In en, this message translates to:
  /// **'Run by hand'**
  String get agentsSourceManual;

  /// No description provided for @agentsSourceScheduled.
  ///
  /// In en, this message translates to:
  /// **'On schedule'**
  String get agentsSourceScheduled;

  /// No description provided for @agentsSourceWebhook.
  ///
  /// In en, this message translates to:
  /// **'Webhook'**
  String get agentsSourceWebhook;

  /// No description provided for @agentsSourceEvent.
  ///
  /// In en, this message translates to:
  /// **'Event'**
  String get agentsSourceEvent;

  /// No description provided for @agentsEnabled.
  ///
  /// In en, this message translates to:
  /// **'Enabled'**
  String get agentsEnabled;

  /// No description provided for @agentsEnableFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t turn on “{name}”: {message}'**
  String agentsEnableFailed(String name, String message);

  /// No description provided for @agentsDisableFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t pause “{name}”: {message}'**
  String agentsDisableFailed(String name, String message);

  /// No description provided for @agentsTrigger.
  ///
  /// In en, this message translates to:
  /// **'Trigger'**
  String get agentsTrigger;

  /// No description provided for @agentsAction.
  ///
  /// In en, this message translates to:
  /// **'Action'**
  String get agentsAction;

  /// No description provided for @agentsActionAgent.
  ///
  /// In en, this message translates to:
  /// **'Agent “{name}” does the work'**
  String agentsActionAgent(String name);

  /// No description provided for @agentsActionAgentMissing.
  ///
  /// In en, this message translates to:
  /// **'The agent that ran this was deleted'**
  String get agentsActionAgentMissing;

  /// No description provided for @agentsActionShell.
  ///
  /// In en, this message translates to:
  /// **'Runs a shell command'**
  String get agentsActionShell;

  /// No description provided for @agentsLastRun.
  ///
  /// In en, this message translates to:
  /// **'Last run'**
  String get agentsLastRun;

  /// No description provided for @agentsRunStartedAt.
  ///
  /// In en, this message translates to:
  /// **'Started'**
  String get agentsRunStartedAt;

  /// No description provided for @agentsRunFinishedAt.
  ///
  /// In en, this message translates to:
  /// **'Finished'**
  String get agentsRunFinishedAt;

  /// No description provided for @agentsRunDuration.
  ///
  /// In en, this message translates to:
  /// **'Took'**
  String get agentsRunDuration;

  /// No description provided for @agentsRunSource.
  ///
  /// In en, this message translates to:
  /// **'Triggered by'**
  String get agentsRunSource;

  /// No description provided for @agentsOpenRun.
  ///
  /// In en, this message translates to:
  /// **'Open this run\'s session'**
  String get agentsOpenRun;

  /// No description provided for @agentsRunNow.
  ///
  /// In en, this message translates to:
  /// **'Run once now'**
  String get agentsRunNow;

  /// No description provided for @agentsRunNowStarted.
  ///
  /// In en, this message translates to:
  /// **'“{name}” started'**
  String agentsRunNowStarted(String name);

  /// No description provided for @agentsRunNowFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t run “{name}”: {message}'**
  String agentsRunNowFailed(String name, String message);

  /// No description provided for @appName.
  ///
  /// In en, this message translates to:
  /// **'Smelt'**
  String get appName;

  /// Tab: the cross-machine command post (things waiting for you, what is running).
  ///
  /// In en, this message translates to:
  /// **'Now'**
  String get navPass;

  /// No description provided for @navSessions.
  ///
  /// In en, this message translates to:
  /// **'Sessions'**
  String get navSessions;

  /// No description provided for @navAgents.
  ///
  /// In en, this message translates to:
  /// **'Agents'**
  String get navAgents;

  /// No description provided for @navSettings.
  ///
  /// In en, this message translates to:
  /// **'Settings'**
  String get navSettings;

  /// No description provided for @commonCancel.
  ///
  /// In en, this message translates to:
  /// **'Cancel'**
  String get commonCancel;

  /// No description provided for @commonDone.
  ///
  /// In en, this message translates to:
  /// **'Done'**
  String get commonDone;

  /// No description provided for @commonSave.
  ///
  /// In en, this message translates to:
  /// **'Save'**
  String get commonSave;

  /// No description provided for @commonRetry.
  ///
  /// In en, this message translates to:
  /// **'Retry'**
  String get commonRetry;

  /// No description provided for @commonOpen.
  ///
  /// In en, this message translates to:
  /// **'Open'**
  String get commonOpen;

  /// No description provided for @commonClose.
  ///
  /// In en, this message translates to:
  /// **'Close'**
  String get commonClose;

  /// No description provided for @commonDelete.
  ///
  /// In en, this message translates to:
  /// **'Delete'**
  String get commonDelete;

  /// No description provided for @commonRename.
  ///
  /// In en, this message translates to:
  /// **'Rename'**
  String get commonRename;

  /// No description provided for @commonCopy.
  ///
  /// In en, this message translates to:
  /// **'Copy'**
  String get commonCopy;

  /// No description provided for @commonCopied.
  ///
  /// In en, this message translates to:
  /// **'Copied'**
  String get commonCopied;

  /// No description provided for @commonMore.
  ///
  /// In en, this message translates to:
  /// **'More'**
  String get commonMore;

  /// No description provided for @commonNew.
  ///
  /// In en, this message translates to:
  /// **'New'**
  String get commonNew;

  /// No description provided for @commonAllow.
  ///
  /// In en, this message translates to:
  /// **'Allow'**
  String get commonAllow;

  /// No description provided for @commonDeny.
  ///
  /// In en, this message translates to:
  /// **'Deny'**
  String get commonDeny;

  /// No description provided for @commonAnswer.
  ///
  /// In en, this message translates to:
  /// **'Answer'**
  String get commonAnswer;

  /// No description provided for @commonView.
  ///
  /// In en, this message translates to:
  /// **'View'**
  String get commonView;

  /// No description provided for @commonLoading.
  ///
  /// In en, this message translates to:
  /// **'Loading…'**
  String get commonLoading;

  /// No description provided for @commonName.
  ///
  /// In en, this message translates to:
  /// **'Name'**
  String get commonName;

  /// No description provided for @machineOnlineLan.
  ///
  /// In en, this message translates to:
  /// **'Local · {ms} ms'**
  String machineOnlineLan(int ms);

  /// No description provided for @machineOnlineDirect.
  ///
  /// In en, this message translates to:
  /// **'Direct · {ms} ms'**
  String machineOnlineDirect(int ms);

  /// No description provided for @machineOnlineRelay.
  ///
  /// In en, this message translates to:
  /// **'Relay · {ms} ms'**
  String machineOnlineRelay(int ms);

  /// No description provided for @machineOnline.
  ///
  /// In en, this message translates to:
  /// **'Online'**
  String get machineOnline;

  /// No description provided for @machineConnecting.
  ///
  /// In en, this message translates to:
  /// **'Connecting…'**
  String get machineConnecting;

  /// No description provided for @machineReconnecting.
  ///
  /// In en, this message translates to:
  /// **'Reconnecting…'**
  String get machineReconnecting;

  /// No description provided for @machineSyncing.
  ///
  /// In en, this message translates to:
  /// **'Syncing…'**
  String get machineSyncing;

  /// No description provided for @machineOffline.
  ///
  /// In en, this message translates to:
  /// **'Offline'**
  String get machineOffline;

  /// No description provided for @machineStatusCached.
  ///
  /// In en, this message translates to:
  /// **'{status} · showing data from {time}'**
  String machineStatusCached(String status, String time);

  /// No description provided for @machineRevoked.
  ///
  /// In en, this message translates to:
  /// **'This phone was removed from this machine'**
  String get machineRevoked;

  /// No description provided for @machineIncompatible.
  ///
  /// In en, this message translates to:
  /// **'Machine needs an update'**
  String get machineIncompatible;

  /// No description provided for @timeJustNow.
  ///
  /// In en, this message translates to:
  /// **'just now'**
  String get timeJustNow;

  /// No description provided for @timeMinutesAgo.
  ///
  /// In en, this message translates to:
  /// **'{n} min ago'**
  String timeMinutesAgo(int n);

  /// No description provided for @timeHoursAgo.
  ///
  /// In en, this message translates to:
  /// **'{n} h ago'**
  String timeHoursAgo(int n);

  /// No description provided for @timeDaysAgo.
  ///
  /// In en, this message translates to:
  /// **'{n} d ago'**
  String timeDaysAgo(int n);

  /// No description provided for @phaseConnecting.
  ///
  /// In en, this message translates to:
  /// **'Starting'**
  String get phaseConnecting;

  /// No description provided for @phaseThinking.
  ///
  /// In en, this message translates to:
  /// **'Thinking'**
  String get phaseThinking;

  /// No description provided for @phaseExecutingTool.
  ///
  /// In en, this message translates to:
  /// **'Running a tool'**
  String get phaseExecutingTool;

  /// No description provided for @phaseAwaitingApproval.
  ///
  /// In en, this message translates to:
  /// **'Waiting for approval'**
  String get phaseAwaitingApproval;

  /// No description provided for @phaseWaitingForUser.
  ///
  /// In en, this message translates to:
  /// **'Waiting for you'**
  String get phaseWaitingForUser;

  /// No description provided for @phaseSucceeded.
  ///
  /// In en, this message translates to:
  /// **'Done'**
  String get phaseSucceeded;

  /// No description provided for @phaseFailed.
  ///
  /// In en, this message translates to:
  /// **'Failed'**
  String get phaseFailed;

  /// No description provided for @phaseIdle.
  ///
  /// In en, this message translates to:
  /// **'Idle'**
  String get phaseIdle;

  /// No description provided for @phaseDead.
  ///
  /// In en, this message translates to:
  /// **'Ended'**
  String get phaseDead;

  /// No description provided for @kindConversation.
  ///
  /// In en, this message translates to:
  /// **'Conversation'**
  String get kindConversation;

  /// No description provided for @kindTerminal.
  ///
  /// In en, this message translates to:
  /// **'Terminal'**
  String get kindTerminal;

  /// No description provided for @conversationUntitled.
  ///
  /// In en, this message translates to:
  /// **'Untitled conversation'**
  String get conversationUntitled;

  /// No description provided for @conversationModelAndSettings.
  ///
  /// In en, this message translates to:
  /// **'Model & settings'**
  String get conversationModelAndSettings;

  /// No description provided for @conversationSkills.
  ///
  /// In en, this message translates to:
  /// **'Skills'**
  String get conversationSkills;

  /// No description provided for @conversationCopyInfo.
  ///
  /// In en, this message translates to:
  /// **'Copy session info'**
  String get conversationCopyInfo;

  /// No description provided for @conversationInfoSession.
  ///
  /// In en, this message translates to:
  /// **'Session {id}'**
  String conversationInfoSession(String id);

  /// No description provided for @conversationLoading.
  ///
  /// In en, this message translates to:
  /// **'Opening conversation…'**
  String get conversationLoading;

  /// No description provided for @conversationUnavailableTitle.
  ///
  /// In en, this message translates to:
  /// **'Can\'t open this conversation'**
  String get conversationUnavailableTitle;

  /// No description provided for @conversationUnavailableDefault.
  ///
  /// In en, this message translates to:
  /// **'This conversation isn\'t running on the machine.'**
  String get conversationUnavailableDefault;

  /// No description provided for @conversationErrorDefault.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t load this conversation.'**
  String get conversationErrorDefault;

  /// No description provided for @conversationCachedSaved.
  ///
  /// In en, this message translates to:
  /// **'Showing the last saved data'**
  String get conversationCachedSaved;

  /// No description provided for @conversationEmptyTitle.
  ///
  /// In en, this message translates to:
  /// **'No messages yet'**
  String get conversationEmptyTitle;

  /// No description provided for @conversationEmptyBody.
  ///
  /// In en, this message translates to:
  /// **'Send a message to get started.'**
  String get conversationEmptyBody;

  /// No description provided for @conversationLoadEarlier.
  ///
  /// In en, this message translates to:
  /// **'Load earlier messages'**
  String get conversationLoadEarlier;

  /// No description provided for @conversationJumpToLatest.
  ///
  /// In en, this message translates to:
  /// **'Back to latest'**
  String get conversationJumpToLatest;

  /// No description provided for @conversationInterrupted.
  ///
  /// In en, this message translates to:
  /// **'Interrupted'**
  String get conversationInterrupted;

  /// No description provided for @conversationThought.
  ///
  /// In en, this message translates to:
  /// **'Thought'**
  String get conversationThought;

  /// No description provided for @conversationThinkingPreview.
  ///
  /// In en, this message translates to:
  /// **'Thinking…'**
  String get conversationThinkingPreview;

  /// No description provided for @conversationShowThought.
  ///
  /// In en, this message translates to:
  /// **'Show thought'**
  String get conversationShowThought;

  /// No description provided for @conversationHideThought.
  ///
  /// In en, this message translates to:
  /// **'Hide thought'**
  String get conversationHideThought;

  /// No description provided for @conversationCopyAnswer.
  ///
  /// In en, this message translates to:
  /// **'Copy answer'**
  String get conversationCopyAnswer;

  /// No description provided for @conversationTook.
  ///
  /// In en, this message translates to:
  /// **'Took {duration}'**
  String conversationTook(String duration);

  /// No description provided for @conversationCompleted.
  ///
  /// In en, this message translates to:
  /// **'Completed'**
  String get conversationCompleted;

  /// No description provided for @conversationCompletionFailed.
  ///
  /// In en, this message translates to:
  /// **'Completion failed'**
  String get conversationCompletionFailed;

  /// No description provided for @conversationTaskCompleted.
  ///
  /// In en, this message translates to:
  /// **'Task completed.'**
  String get conversationTaskCompleted;

  /// No description provided for @conversationToolCalls.
  ///
  /// In en, this message translates to:
  /// **'{count, plural, =1{1 tool call} other{{count} tool calls}}'**
  String conversationToolCalls(int count);

  /// No description provided for @conversationToolFailedCount.
  ///
  /// In en, this message translates to:
  /// **'{count} failed'**
  String conversationToolFailedCount(int count);

  /// No description provided for @conversationToolRead.
  ///
  /// In en, this message translates to:
  /// **'Read'**
  String get conversationToolRead;

  /// No description provided for @conversationToolEdit.
  ///
  /// In en, this message translates to:
  /// **'Edit'**
  String get conversationToolEdit;

  /// No description provided for @conversationToolDelete.
  ///
  /// In en, this message translates to:
  /// **'Delete'**
  String get conversationToolDelete;

  /// No description provided for @conversationToolMove.
  ///
  /// In en, this message translates to:
  /// **'Move'**
  String get conversationToolMove;

  /// No description provided for @conversationToolSearch.
  ///
  /// In en, this message translates to:
  /// **'Search'**
  String get conversationToolSearch;

  /// No description provided for @conversationToolExecute.
  ///
  /// In en, this message translates to:
  /// **'Run'**
  String get conversationToolExecute;

  /// No description provided for @conversationToolThink.
  ///
  /// In en, this message translates to:
  /// **'Think'**
  String get conversationToolThink;

  /// No description provided for @conversationToolFetch.
  ///
  /// In en, this message translates to:
  /// **'Fetch'**
  String get conversationToolFetch;

  /// No description provided for @conversationToolSwitchMode.
  ///
  /// In en, this message translates to:
  /// **'Mode'**
  String get conversationToolSwitchMode;

  /// No description provided for @conversationToolCollaborate.
  ///
  /// In en, this message translates to:
  /// **'Delegate'**
  String get conversationToolCollaborate;

  /// No description provided for @conversationToolReview.
  ///
  /// In en, this message translates to:
  /// **'Review'**
  String get conversationToolReview;

  /// No description provided for @conversationToolImage.
  ///
  /// In en, this message translates to:
  /// **'Image'**
  String get conversationToolImage;

  /// No description provided for @conversationToolCompact.
  ///
  /// In en, this message translates to:
  /// **'Compact'**
  String get conversationToolCompact;

  /// No description provided for @conversationToolWait.
  ///
  /// In en, this message translates to:
  /// **'Wait'**
  String get conversationToolWait;

  /// No description provided for @conversationToolOther.
  ///
  /// In en, this message translates to:
  /// **'Tool'**
  String get conversationToolOther;

  /// No description provided for @conversationToolPending.
  ///
  /// In en, this message translates to:
  /// **'Not started'**
  String get conversationToolPending;

  /// No description provided for @conversationToolInProgress.
  ///
  /// In en, this message translates to:
  /// **'Running'**
  String get conversationToolInProgress;

  /// No description provided for @conversationToolDone.
  ///
  /// In en, this message translates to:
  /// **'Done'**
  String get conversationToolDone;

  /// No description provided for @conversationToolFailed.
  ///
  /// In en, this message translates to:
  /// **'Failed'**
  String get conversationToolFailed;

  /// No description provided for @conversationShowOutput.
  ///
  /// In en, this message translates to:
  /// **'Show output'**
  String get conversationShowOutput;

  /// No description provided for @conversationHideOutput.
  ///
  /// In en, this message translates to:
  /// **'Hide output'**
  String get conversationHideOutput;

  /// No description provided for @conversationExitCode.
  ///
  /// In en, this message translates to:
  /// **'Exit code {code}'**
  String conversationExitCode(int code);

  /// No description provided for @conversationOutputTruncated.
  ///
  /// In en, this message translates to:
  /// **'Output truncated'**
  String get conversationOutputTruncated;

  /// No description provided for @conversationShowAllLines.
  ///
  /// In en, this message translates to:
  /// **'Show all {count} lines'**
  String conversationShowAllLines(int count);

  /// No description provided for @conversationCollapse.
  ///
  /// In en, this message translates to:
  /// **'Collapse'**
  String get conversationCollapse;

  /// No description provided for @conversationUnchangedLines.
  ///
  /// In en, this message translates to:
  /// **'{count, plural, =1{1 unchanged line} other{{count} unchanged lines}}'**
  String conversationUnchangedLines(int count);

  /// No description provided for @conversationNewFile.
  ///
  /// In en, this message translates to:
  /// **'New file'**
  String get conversationNewFile;

  /// No description provided for @conversationPlan.
  ///
  /// In en, this message translates to:
  /// **'Plan · {done}/{total}'**
  String conversationPlan(int done, int total);

  /// No description provided for @conversationPlanPending.
  ///
  /// In en, this message translates to:
  /// **'To do'**
  String get conversationPlanPending;

  /// No description provided for @conversationPlanInProgress.
  ///
  /// In en, this message translates to:
  /// **'In progress'**
  String get conversationPlanInProgress;

  /// No description provided for @conversationPlanCompleted.
  ///
  /// In en, this message translates to:
  /// **'Done'**
  String get conversationPlanCompleted;

  /// No description provided for @conversationTaskBackground.
  ///
  /// In en, this message translates to:
  /// **'Background task'**
  String get conversationTaskBackground;

  /// No description provided for @conversationTaskPeerMessage.
  ///
  /// In en, this message translates to:
  /// **'Message'**
  String get conversationTaskPeerMessage;

  /// No description provided for @conversationTaskSubagent.
  ///
  /// In en, this message translates to:
  /// **'Subagent'**
  String get conversationTaskSubagent;

  /// No description provided for @conversationTaskRunning.
  ///
  /// In en, this message translates to:
  /// **'Running'**
  String get conversationTaskRunning;

  /// No description provided for @conversationTaskCompletedStatus.
  ///
  /// In en, this message translates to:
  /// **'Finished'**
  String get conversationTaskCompletedStatus;

  /// No description provided for @conversationTaskFailed.
  ///
  /// In en, this message translates to:
  /// **'Failed'**
  String get conversationTaskFailed;

  /// No description provided for @conversationTaskTimedOut.
  ///
  /// In en, this message translates to:
  /// **'Timed out'**
  String get conversationTaskTimedOut;

  /// No description provided for @conversationTaskStopped.
  ///
  /// In en, this message translates to:
  /// **'Stopped'**
  String get conversationTaskStopped;

  /// No description provided for @conversationImageOpen.
  ///
  /// In en, this message translates to:
  /// **'Open image'**
  String get conversationImageOpen;

  /// No description provided for @conversationImageClose.
  ///
  /// In en, this message translates to:
  /// **'Close image'**
  String get conversationImageClose;

  /// No description provided for @conversationImageUnavailable.
  ///
  /// In en, this message translates to:
  /// **'Image not available'**
  String get conversationImageUnavailable;

  /// No description provided for @conversationImageUndecodable.
  ///
  /// In en, this message translates to:
  /// **'This image can\'t be displayed.'**
  String get conversationImageUndecodable;

  /// No description provided for @conversationImageLoadFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t load the original: {message}'**
  String conversationImageLoadFailed(String message);

  /// No description provided for @conversationStatusEnded.
  ///
  /// In en, this message translates to:
  /// **'Session ended'**
  String get conversationStatusEnded;

  /// No description provided for @conversationHint.
  ///
  /// In en, this message translates to:
  /// **'Message {agent}'**
  String conversationHint(String agent);

  /// No description provided for @conversationHintOffline.
  ///
  /// In en, this message translates to:
  /// **'Machine is offline. Your draft is kept.'**
  String get conversationHintOffline;

  /// No description provided for @conversationHintTicket.
  ///
  /// In en, this message translates to:
  /// **'Handle the ticket above first'**
  String get conversationHintTicket;

  /// No description provided for @conversationHintEnded.
  ///
  /// In en, this message translates to:
  /// **'This session has ended'**
  String get conversationHintEnded;

  /// No description provided for @conversationHintSending.
  ///
  /// In en, this message translates to:
  /// **'Waiting for the machine to confirm…'**
  String get conversationHintSending;

  /// No description provided for @conversationSend.
  ///
  /// In en, this message translates to:
  /// **'Send'**
  String get conversationSend;

  /// No description provided for @conversationStop.
  ///
  /// In en, this message translates to:
  /// **'Stop this turn'**
  String get conversationStop;

  /// No description provided for @conversationAttachImage.
  ///
  /// In en, this message translates to:
  /// **'Add image'**
  String get conversationAttachImage;

  /// No description provided for @conversationNoImageSupport.
  ///
  /// In en, this message translates to:
  /// **'This agent can\'t take images'**
  String get conversationNoImageSupport;

  /// No description provided for @conversationCommands.
  ///
  /// In en, this message translates to:
  /// **'Commands and skills'**
  String get conversationCommands;

  /// No description provided for @conversationRemoveImage.
  ///
  /// In en, this message translates to:
  /// **'Remove image'**
  String get conversationRemoveImage;

  /// No description provided for @conversationImagesSkipped.
  ///
  /// In en, this message translates to:
  /// **'{count, plural, =1{1 image was over 5 MB and was skipped} other{{count} images were over 5 MB and were skipped}}'**
  String conversationImagesSkipped(int count);

  /// No description provided for @conversationAttachFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t add the image: {message}'**
  String conversationAttachFailed(String message);

  /// No description provided for @conversationUnconfirmed.
  ///
  /// In en, this message translates to:
  /// **'Delivery wasn\'t confirmed. Check the conversation, then send again.'**
  String get conversationUnconfirmed;

  /// No description provided for @conversationRejected.
  ///
  /// In en, this message translates to:
  /// **'Not sent: {message}'**
  String conversationRejected(String message);

  /// No description provided for @conversationDraftSaveFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t save the draft: {message}'**
  String conversationDraftSaveFailed(String message);

  /// No description provided for @conversationResendTitle.
  ///
  /// In en, this message translates to:
  /// **'Send again?'**
  String get conversationResendTitle;

  /// No description provided for @conversationResendBody.
  ///
  /// In en, this message translates to:
  /// **'The last attempt may already have reached the agent. Check the conversation before sending it again.'**
  String get conversationResendBody;

  /// No description provided for @conversationResendConfirm.
  ///
  /// In en, this message translates to:
  /// **'Send again'**
  String get conversationResendConfirm;

  /// No description provided for @conversationContextUsage.
  ///
  /// In en, this message translates to:
  /// **'Context {percent}% · {used} of {total} tokens'**
  String conversationContextUsage(int percent, String used, String total);

  /// No description provided for @conversationModel.
  ///
  /// In en, this message translates to:
  /// **'Model'**
  String get conversationModel;

  /// No description provided for @conversationProvider.
  ///
  /// In en, this message translates to:
  /// **'Provider'**
  String get conversationProvider;

  /// No description provided for @conversationOn.
  ///
  /// In en, this message translates to:
  /// **'On'**
  String get conversationOn;

  /// No description provided for @conversationOff.
  ///
  /// In en, this message translates to:
  /// **'Off'**
  String get conversationOff;

  /// No description provided for @conversationSkillsEmpty.
  ///
  /// In en, this message translates to:
  /// **'No skills are loaded in this conversation.'**
  String get conversationSkillsEmpty;

  /// No description provided for @conversationSkillsNote.
  ///
  /// In en, this message translates to:
  /// **'Skills are managed on the computer.'**
  String get conversationSkillsNote;

  /// No description provided for @conversationCommandsTitle.
  ///
  /// In en, this message translates to:
  /// **'Commands'**
  String get conversationCommandsTitle;

  /// No description provided for @conversationNoSettings.
  ///
  /// In en, this message translates to:
  /// **'This agent has no settings to change here.'**
  String get conversationNoSettings;

  /// No description provided for @conversationTicketApproval.
  ///
  /// In en, this message translates to:
  /// **'Approval'**
  String get conversationTicketApproval;

  /// No description provided for @conversationTicketQuestion.
  ///
  /// In en, this message translates to:
  /// **'Question'**
  String get conversationTicketQuestion;

  /// No description provided for @conversationTicketCount.
  ///
  /// In en, this message translates to:
  /// **'{index} of {count}'**
  String conversationTicketCount(int index, int count);

  /// No description provided for @conversationTicketCollapse.
  ///
  /// In en, this message translates to:
  /// **'Fold the ticket down'**
  String get conversationTicketCollapse;

  /// No description provided for @conversationTicketExpand.
  ///
  /// In en, this message translates to:
  /// **'Open the ticket'**
  String get conversationTicketExpand;

  /// No description provided for @conversationActionFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t send: {message}'**
  String conversationActionFailed(String message);

  /// No description provided for @launchTitle.
  ///
  /// In en, this message translates to:
  /// **'New session'**
  String get launchTitle;

  /// No description provided for @launchBack.
  ///
  /// In en, this message translates to:
  /// **'Back'**
  String get launchBack;

  /// No description provided for @launchStepMachine.
  ///
  /// In en, this message translates to:
  /// **'Choose a machine'**
  String get launchStepMachine;

  /// No description provided for @launchStepProject.
  ///
  /// In en, this message translates to:
  /// **'Choose a project'**
  String get launchStepProject;

  /// No description provided for @launchNoMachineOnline.
  ///
  /// In en, this message translates to:
  /// **'No machine is online. A session starts on the machine itself, so it has to be connected.'**
  String get launchNoMachineOnline;

  /// No description provided for @launchMachineUnavailable.
  ///
  /// In en, this message translates to:
  /// **'{status} · available once it reconnects'**
  String launchMachineUnavailable(String status);

  /// No description provided for @launchLoadFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t load this machine\'s projects'**
  String get launchLoadFailed;

  /// No description provided for @launchNoProjects.
  ///
  /// In en, this message translates to:
  /// **'No projects open on this machine. Open one in Smelt on the computer first.'**
  String get launchNoProjects;

  /// No description provided for @launchNoProject.
  ///
  /// In en, this message translates to:
  /// **'No project'**
  String get launchNoProject;

  /// No description provided for @launchNoProjectHint.
  ///
  /// In en, this message translates to:
  /// **'Talk to one of your agents in its own space'**
  String get launchNoProjectHint;

  /// No description provided for @launchSectionPinned.
  ///
  /// In en, this message translates to:
  /// **'Pinned'**
  String get launchSectionPinned;

  /// No description provided for @launchSectionConversation.
  ///
  /// In en, this message translates to:
  /// **'Conversations'**
  String get launchSectionConversation;

  /// No description provided for @launchSectionTerminal.
  ///
  /// In en, this message translates to:
  /// **'Terminals'**
  String get launchSectionTerminal;

  /// No description provided for @launchSectionAgents.
  ///
  /// In en, this message translates to:
  /// **'Agents'**
  String get launchSectionAgents;

  /// No description provided for @launchAgentSubtitle.
  ///
  /// In en, this message translates to:
  /// **'Agent · {engine}'**
  String launchAgentSubtitle(String engine);

  /// No description provided for @launchNoActions.
  ///
  /// In en, this message translates to:
  /// **'Nothing to start here yet. Update Smelt on the computer, then try again.'**
  String get launchNoActions;

  /// No description provided for @launchStarting.
  ///
  /// In en, this message translates to:
  /// **'Starting {label}…'**
  String launchStarting(String label);

  /// No description provided for @launchWaiting.
  ///
  /// In en, this message translates to:
  /// **'Started. Opening it…'**
  String get launchWaiting;

  /// No description provided for @launchFailedTitle.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t start {label}'**
  String launchFailedTitle(String label);

  /// No description provided for @launchFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t start the session: {message}'**
  String launchFailed(String message);

  /// No description provided for @launchNotListed.
  ///
  /// In en, this message translates to:
  /// **'It started on {machine} but hasn\'t shown up yet. Look for it in Sessions in a moment.'**
  String launchNotListed(String machine);

  /// No description provided for @machinesOsMacos.
  ///
  /// In en, this message translates to:
  /// **'macOS'**
  String get machinesOsMacos;

  /// No description provided for @machinesOsLinux.
  ///
  /// In en, this message translates to:
  /// **'Linux'**
  String get machinesOsLinux;

  /// No description provided for @machinesOsWindows.
  ///
  /// In en, this message translates to:
  /// **'Windows'**
  String get machinesOsWindows;

  /// No description provided for @machinesOsUnknown.
  ///
  /// In en, this message translates to:
  /// **'Unknown system'**
  String get machinesOsUnknown;

  /// No description provided for @machinesName.
  ///
  /// In en, this message translates to:
  /// **'Name'**
  String get machinesName;

  /// No description provided for @machinesHostname.
  ///
  /// In en, this message translates to:
  /// **'Host name: {hostname}'**
  String machinesHostname(String hostname);

  /// No description provided for @machinesRenameTitle.
  ///
  /// In en, this message translates to:
  /// **'Rename machine'**
  String get machinesRenameTitle;

  /// No description provided for @machinesRenameEmptyHint.
  ///
  /// In en, this message translates to:
  /// **'Leave empty to use the host name ({hostname})'**
  String machinesRenameEmptyHint(String hostname);

  /// No description provided for @machinesRenameFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t rename: {error}'**
  String machinesRenameFailed(String error);

  /// No description provided for @machinesAccent.
  ///
  /// In en, this message translates to:
  /// **'Label color'**
  String get machinesAccent;

  /// No description provided for @machinesAccentFooter.
  ///
  /// In en, this message translates to:
  /// **'Every session from this machine carries this color.'**
  String get machinesAccentFooter;

  /// No description provided for @machinesAccentBlue.
  ///
  /// In en, this message translates to:
  /// **'Blue'**
  String get machinesAccentBlue;

  /// No description provided for @machinesAccentTeal.
  ///
  /// In en, this message translates to:
  /// **'Teal'**
  String get machinesAccentTeal;

  /// No description provided for @machinesAccentViolet.
  ///
  /// In en, this message translates to:
  /// **'Violet'**
  String get machinesAccentViolet;

  /// No description provided for @machinesAccentPink.
  ///
  /// In en, this message translates to:
  /// **'Pink'**
  String get machinesAccentPink;

  /// No description provided for @machinesAccentOlive.
  ///
  /// In en, this message translates to:
  /// **'Olive'**
  String get machinesAccentOlive;

  /// No description provided for @machinesAccentIndigo.
  ///
  /// In en, this message translates to:
  /// **'Indigo'**
  String get machinesAccentIndigo;

  /// No description provided for @machinesAccentSky.
  ///
  /// In en, this message translates to:
  /// **'Sky'**
  String get machinesAccentSky;

  /// No description provided for @machinesAccentBrown.
  ///
  /// In en, this message translates to:
  /// **'Brown'**
  String get machinesAccentBrown;

  /// No description provided for @machinesAccentOther.
  ///
  /// In en, this message translates to:
  /// **'Color {n}'**
  String machinesAccentOther(int n);

  /// No description provided for @machinesAccentFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t change the color: {error}'**
  String machinesAccentFailed(String error);

  /// No description provided for @machinesLink.
  ///
  /// In en, this message translates to:
  /// **'Connection'**
  String get machinesLink;

  /// No description provided for @machinesStatus.
  ///
  /// In en, this message translates to:
  /// **'Status'**
  String get machinesStatus;

  /// No description provided for @machinesPath.
  ///
  /// In en, this message translates to:
  /// **'Path'**
  String get machinesPath;

  /// No description provided for @machinesPathLan.
  ///
  /// In en, this message translates to:
  /// **'Local network'**
  String get machinesPathLan;

  /// No description provided for @machinesPathDirect.
  ///
  /// In en, this message translates to:
  /// **'Direct'**
  String get machinesPathDirect;

  /// No description provided for @machinesPathRelay.
  ///
  /// In en, this message translates to:
  /// **'Relay'**
  String get machinesPathRelay;

  /// No description provided for @machinesPathUnknown.
  ///
  /// In en, this message translates to:
  /// **'Not chosen yet'**
  String get machinesPathUnknown;

  /// No description provided for @machinesRtt.
  ///
  /// In en, this message translates to:
  /// **'Round trip'**
  String get machinesRtt;

  /// No description provided for @machinesRttValue.
  ///
  /// In en, this message translates to:
  /// **'{ms} ms'**
  String machinesRttValue(int ms);

  /// No description provided for @machinesEndpoint.
  ///
  /// In en, this message translates to:
  /// **'Endpoint ID'**
  String get machinesEndpoint;

  /// No description provided for @machinesEndpointCopy.
  ///
  /// In en, this message translates to:
  /// **'Copy endpoint ID'**
  String get machinesEndpointCopy;

  /// No description provided for @machinesRelay.
  ///
  /// In en, this message translates to:
  /// **'Relay server'**
  String get machinesRelay;

  /// No description provided for @machinesRelayDefault.
  ///
  /// In en, this message translates to:
  /// **'Default public relays'**
  String get machinesRelayDefault;

  /// No description provided for @machinesVersion.
  ///
  /// In en, this message translates to:
  /// **'Runtime version'**
  String get machinesVersion;

  /// No description provided for @machinesVersionUnknown.
  ///
  /// In en, this message translates to:
  /// **'Not connected yet'**
  String get machinesVersionUnknown;

  /// No description provided for @machinesSystem.
  ///
  /// In en, this message translates to:
  /// **'System'**
  String get machinesSystem;

  /// No description provided for @machinesLastError.
  ///
  /// In en, this message translates to:
  /// **'Last error'**
  String get machinesLastError;

  /// No description provided for @machinesRelayMixedNote.
  ///
  /// In en, this message translates to:
  /// **'Your machines use different relay settings, so on iOS a self-hosted relay may be unreachable while a system proxy or VPN is on.'**
  String get machinesRelayMixedNote;

  /// No description provided for @machinesReconnect.
  ///
  /// In en, this message translates to:
  /// **'Reconnect now'**
  String get machinesReconnect;

  /// No description provided for @machinesReconnectBusy.
  ///
  /// In en, this message translates to:
  /// **'Reconnecting…'**
  String get machinesReconnectBusy;

  /// No description provided for @machinesReconnected.
  ///
  /// In en, this message translates to:
  /// **'Connected to {name}'**
  String machinesReconnected(String name);

  /// No description provided for @machinesReconnectOffline.
  ///
  /// In en, this message translates to:
  /// **'Still can\'t reach {name}. It keeps retrying in the background.'**
  String machinesReconnectOffline(String name);

  /// No description provided for @machinesReconnectFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t reconnect: {error}'**
  String machinesReconnectFailed(String error);

  /// No description provided for @machinesRevokedBody.
  ///
  /// In en, this message translates to:
  /// **'{name} no longer accepts this phone: it was removed on that machine, or its device list was reset. Make a new pairing code there and pair again. The name and label color stay.'**
  String machinesRevokedBody(String name);

  /// No description provided for @machinesIncompatibleBody.
  ///
  /// In en, this message translates to:
  /// **'This app and Smelt on {name} are on incompatible versions. Update the older one, then reconnect. If it still fails, pair again.'**
  String machinesIncompatibleBody(String name);

  /// No description provided for @machinesRepair.
  ///
  /// In en, this message translates to:
  /// **'Pair again'**
  String get machinesRepair;

  /// No description provided for @machinesForget.
  ///
  /// In en, this message translates to:
  /// **'Forget this machine'**
  String get machinesForget;

  /// No description provided for @machinesForgetTitle.
  ///
  /// In en, this message translates to:
  /// **'Forget {name}?'**
  String machinesForgetTitle(String name);

  /// No description provided for @machinesForgetBody.
  ///
  /// In en, this message translates to:
  /// **'Removes {name} from this phone, along with its drafts and offline cache. Whether this phone may connect is managed on {name} itself: to revoke access, remove this phone from its device list. You can pair again later.'**
  String machinesForgetBody(String name);

  /// No description provided for @machinesForgetConfirm.
  ///
  /// In en, this message translates to:
  /// **'Forget'**
  String get machinesForgetConfirm;

  /// No description provided for @machinesForgotten.
  ///
  /// In en, this message translates to:
  /// **'Forgot {name}'**
  String machinesForgotten(String name);

  /// No description provided for @machinesForgetFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t forget the machine: {error}'**
  String machinesForgetFailed(String error);

  /// No description provided for @machinesGone.
  ///
  /// In en, this message translates to:
  /// **'This machine is no longer on this phone.'**
  String get machinesGone;

  /// No description provided for @machinesPairTitle.
  ///
  /// In en, this message translates to:
  /// **'Pair a machine'**
  String get machinesPairTitle;

  /// No description provided for @machinesPairGuide.
  ///
  /// In en, this message translates to:
  /// **'Scan the code from Smelt on your computer (Settings → Mobile Remote), or the one printed by “smeltd pair” on a server.'**
  String get machinesPairGuide;

  /// No description provided for @machinesPairPaste.
  ///
  /// In en, this message translates to:
  /// **'Paste pairing code'**
  String get machinesPairPaste;

  /// No description provided for @machinesPairPasteHint.
  ///
  /// In en, this message translates to:
  /// **'smelt+iroh://…'**
  String get machinesPairPasteHint;

  /// No description provided for @machinesPairFromClipboard.
  ///
  /// In en, this message translates to:
  /// **'Paste from clipboard'**
  String get machinesPairFromClipboard;

  /// No description provided for @machinesPairSubmit.
  ///
  /// In en, this message translates to:
  /// **'Pair'**
  String get machinesPairSubmit;

  /// No description provided for @machinesPairTorchOn.
  ///
  /// In en, this message translates to:
  /// **'Turn on flashlight'**
  String get machinesPairTorchOn;

  /// No description provided for @machinesPairTorchOff.
  ///
  /// In en, this message translates to:
  /// **'Turn off flashlight'**
  String get machinesPairTorchOff;

  /// No description provided for @machinesPairNotACode.
  ///
  /// In en, this message translates to:
  /// **'That isn\'t a Smelt pairing code.'**
  String get machinesPairNotACode;

  /// No description provided for @machinesPairBusyNew.
  ///
  /// In en, this message translates to:
  /// **'Pairing with a new machine…'**
  String get machinesPairBusyNew;

  /// No description provided for @machinesPairBusyAgain.
  ///
  /// In en, this message translates to:
  /// **'Pairing with {name} again…'**
  String machinesPairBusyAgain(String name);

  /// No description provided for @machinesPairBusyBody.
  ///
  /// In en, this message translates to:
  /// **'Your other machines stay connected.'**
  String get machinesPairBusyBody;

  /// No description provided for @machinesPaired.
  ///
  /// In en, this message translates to:
  /// **'Connected to {name}'**
  String machinesPaired(String name);

  /// No description provided for @machinesPairFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t pair'**
  String get machinesPairFailed;

  /// No description provided for @machinesPairErrorExpired.
  ///
  /// In en, this message translates to:
  /// **'This pairing code has expired or was already used. Make a new one on the machine and scan it.'**
  String get machinesPairErrorExpired;

  /// No description provided for @machinesPairErrorIncompatible.
  ///
  /// In en, this message translates to:
  /// **'This app and Smelt on that machine are on incompatible versions. Update the older one, then pair with a new code.'**
  String get machinesPairErrorIncompatible;

  /// No description provided for @machinesPairErrorOffline.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t reach the machine. Check that it\'s awake and both sides are online, then try again. If the code has expired, make a new one.'**
  String get machinesPairErrorOffline;

  /// No description provided for @machinesPairErrorRejected.
  ///
  /// In en, this message translates to:
  /// **'The machine turned this phone away. Make a new pairing code on it and try again.'**
  String get machinesPairErrorRejected;

  /// No description provided for @machinesPairErrorCancelled.
  ///
  /// In en, this message translates to:
  /// **'Pairing was interrupted. Try again.'**
  String get machinesPairErrorCancelled;

  /// No description provided for @machinesPairErrorOther.
  ///
  /// In en, this message translates to:
  /// **'Pairing failed: {error}'**
  String machinesPairErrorOther(String error);

  /// No description provided for @machinesPairScanAgain.
  ///
  /// In en, this message translates to:
  /// **'Scan again'**
  String get machinesPairScanAgain;

  /// No description provided for @machinesCameraDenied.
  ///
  /// In en, this message translates to:
  /// **'Smelt can\'t use the camera. Allow camera access in the system settings, or paste the pairing code instead.'**
  String get machinesCameraDenied;

  /// No description provided for @machinesCameraUnavailable.
  ///
  /// In en, this message translates to:
  /// **'The camera isn\'t available. Paste the pairing code instead.'**
  String get machinesCameraUnavailable;

  /// No description provided for @passTitle.
  ///
  /// In en, this message translates to:
  /// **'Now'**
  String get passTitle;

  /// No description provided for @passNewSession.
  ///
  /// In en, this message translates to:
  /// **'New session'**
  String get passNewSession;

  /// No description provided for @passRailEmpty.
  ///
  /// In en, this message translates to:
  /// **'Nothing is waiting for you.'**
  String get passRailEmpty;

  /// No description provided for @passTicketsWaiting.
  ///
  /// In en, this message translates to:
  /// **'{count, plural, =1{1 waiting for you} other{{count} waiting for you}}'**
  String passTicketsWaiting(int count);

  /// No description provided for @passTicketNumber.
  ///
  /// In en, this message translates to:
  /// **'#{n}'**
  String passTicketNumber(int n);

  /// No description provided for @passWantsToRun.
  ///
  /// In en, this message translates to:
  /// **'{agent} wants to run'**
  String passWantsToRun(String agent);

  /// No description provided for @passWantsTo.
  ///
  /// In en, this message translates to:
  /// **'{agent} asks'**
  String passWantsTo(String agent);

  /// No description provided for @passHasQuestion.
  ///
  /// In en, this message translates to:
  /// **'{agent} has a question'**
  String passHasQuestion(String agent);

  /// No description provided for @passFailed.
  ///
  /// In en, this message translates to:
  /// **'{agent} stopped with an error'**
  String passFailed(String agent);

  /// No description provided for @passContextYou.
  ///
  /// In en, this message translates to:
  /// **'You'**
  String get passContextYou;

  /// No description provided for @passLoadingDetails.
  ///
  /// In en, this message translates to:
  /// **'Fetching what it needs…'**
  String get passLoadingDetails;

  /// No description provided for @passMoreOptions.
  ///
  /// In en, this message translates to:
  /// **'More options'**
  String get passMoreOptions;

  /// No description provided for @passStampAllowed.
  ///
  /// In en, this message translates to:
  /// **'Allowed · {time}'**
  String passStampAllowed(String time);

  /// No description provided for @passStampDenied.
  ///
  /// In en, this message translates to:
  /// **'Denied · {time}'**
  String passStampDenied(String time);

  /// No description provided for @passStampAnswered.
  ///
  /// In en, this message translates to:
  /// **'Answered · {time}'**
  String passStampAnswered(String time);

  /// No description provided for @passStationIdle.
  ///
  /// In en, this message translates to:
  /// **'Nothing running here.'**
  String get passStationIdle;

  /// No description provided for @passSeeAllSessions.
  ///
  /// In en, this message translates to:
  /// **'All sessions'**
  String get passSeeAllSessions;

  /// No description provided for @passRunning.
  ///
  /// In en, this message translates to:
  /// **'Running'**
  String get passRunning;

  /// No description provided for @passJustFinished.
  ///
  /// In en, this message translates to:
  /// **'Just finished'**
  String get passJustFinished;

  /// No description provided for @passRecent.
  ///
  /// In en, this message translates to:
  /// **'Recent'**
  String get passRecent;

  /// No description provided for @passRepair.
  ///
  /// In en, this message translates to:
  /// **'Pair again'**
  String get passRepair;

  /// No description provided for @passNoMachinesTitle.
  ///
  /// In en, this message translates to:
  /// **'Pair your first machine'**
  String get passNoMachinesTitle;

  /// No description provided for @passNoMachinesBody.
  ///
  /// In en, this message translates to:
  /// **'On your Mac, open Smelt → Settings → Mobile Remote, or run `smeltd pair` on a server, then scan the code.'**
  String get passNoMachinesBody;

  /// No description provided for @passScanCode.
  ///
  /// In en, this message translates to:
  /// **'Scan pairing code'**
  String get passScanCode;

  /// No description provided for @passTriageTitle.
  ///
  /// In en, this message translates to:
  /// **'{index} of {count}'**
  String passTriageTitle(int index, int count);

  /// No description provided for @passTriageDone.
  ///
  /// In en, this message translates to:
  /// **'All caught up'**
  String get passTriageDone;

  /// No description provided for @passTriageDoneBody.
  ///
  /// In en, this message translates to:
  /// **'Every ticket has been handled.'**
  String get passTriageDoneBody;

  /// No description provided for @passOpenConversation.
  ///
  /// In en, this message translates to:
  /// **'Open conversation'**
  String get passOpenConversation;

  /// No description provided for @passWorkingDir.
  ///
  /// In en, this message translates to:
  /// **'In {dir}'**
  String passWorkingDir(String dir);

  /// No description provided for @passGrantRoot.
  ///
  /// In en, this message translates to:
  /// **'Grants access to {path}'**
  String passGrantRoot(String path);

  /// No description provided for @passActionFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t send: {message}'**
  String passActionFailed(String message);

  /// No description provided for @approvalAllowOnce.
  ///
  /// In en, this message translates to:
  /// **'Allow once'**
  String get approvalAllowOnce;

  /// No description provided for @approvalAllowAlways.
  ///
  /// In en, this message translates to:
  /// **'Always allow'**
  String get approvalAllowAlways;

  /// No description provided for @approvalRejectOnce.
  ///
  /// In en, this message translates to:
  /// **'Deny'**
  String get approvalRejectOnce;

  /// No description provided for @approvalRejectAlways.
  ///
  /// In en, this message translates to:
  /// **'Always deny'**
  String get approvalRejectAlways;

  /// No description provided for @elicitationSubmit.
  ///
  /// In en, this message translates to:
  /// **'Send answer'**
  String get elicitationSubmit;

  /// No description provided for @elicitationDismiss.
  ///
  /// In en, this message translates to:
  /// **'Skip'**
  String get elicitationDismiss;

  /// No description provided for @elicitationRequired.
  ///
  /// In en, this message translates to:
  /// **'Required'**
  String get elicitationRequired;

  /// No description provided for @elicitationOpenLink.
  ///
  /// In en, this message translates to:
  /// **'Open link'**
  String get elicitationOpenLink;

  /// No description provided for @elicitationTextHint.
  ///
  /// In en, this message translates to:
  /// **'Type your answer'**
  String get elicitationTextHint;

  /// No description provided for @sessionsTitle.
  ///
  /// In en, this message translates to:
  /// **'Sessions'**
  String get sessionsTitle;

  /// No description provided for @sessionsNewSession.
  ///
  /// In en, this message translates to:
  /// **'New session'**
  String get sessionsNewSession;

  /// No description provided for @sessionsFilterAll.
  ///
  /// In en, this message translates to:
  /// **'All'**
  String get sessionsFilterAll;

  /// Screen reader label for the row of machine filter chips.
  ///
  /// In en, this message translates to:
  /// **'Show sessions from'**
  String get sessionsFilterMachines;

  /// No description provided for @sessionsSearchHint.
  ///
  /// In en, this message translates to:
  /// **'Search titles, projects, agents'**
  String get sessionsSearchHint;

  /// No description provided for @sessionsSearchClear.
  ///
  /// In en, this message translates to:
  /// **'Clear search'**
  String get sessionsSearchClear;

  /// No description provided for @sessionsAgentChats.
  ///
  /// In en, this message translates to:
  /// **'Agent conversations'**
  String get sessionsAgentChats;

  /// No description provided for @sessionsAutomationRuns.
  ///
  /// In en, this message translates to:
  /// **'Automation runs'**
  String get sessionsAutomationRuns;

  /// No description provided for @sessionsNoProject.
  ///
  /// In en, this message translates to:
  /// **'No project'**
  String get sessionsNoProject;

  /// No description provided for @sessionsIdleProjects.
  ///
  /// In en, this message translates to:
  /// **'Projects without open sessions'**
  String get sessionsIdleProjects;

  /// Last row in a project group: opens the resumable past conversations of that project.
  ///
  /// In en, this message translates to:
  /// **'History'**
  String get sessionsHistoryRow;

  /// No description provided for @sessionsMachineEmpty.
  ///
  /// In en, this message translates to:
  /// **'No sessions on this machine.'**
  String get sessionsMachineEmpty;

  /// No description provided for @sessionsMachineWaiting.
  ///
  /// In en, this message translates to:
  /// **'Waiting for this machine\'s session list…'**
  String get sessionsMachineWaiting;

  /// No description provided for @sessionsNoMachinesTitle.
  ///
  /// In en, this message translates to:
  /// **'No machines yet'**
  String get sessionsNoMachinesTitle;

  /// No description provided for @sessionsNoMachinesBody.
  ///
  /// In en, this message translates to:
  /// **'Pair a machine from Now or Settings, and its sessions show up here.'**
  String get sessionsNoMachinesBody;

  /// No description provided for @sessionsNoMatchesTitle.
  ///
  /// In en, this message translates to:
  /// **'Nothing matches “{query}”'**
  String sessionsNoMatchesTitle(String query);

  /// No description provided for @sessionsNoMatchesBody.
  ///
  /// In en, this message translates to:
  /// **'Search looks at titles, projects and agents.'**
  String get sessionsNoMatchesBody;

  /// No description provided for @sessionsUntitledConversation.
  ///
  /// In en, this message translates to:
  /// **'Untitled conversation'**
  String get sessionsUntitledConversation;

  /// No description provided for @sessionsUntitledTerminal.
  ///
  /// In en, this message translates to:
  /// **'Terminal'**
  String get sessionsUntitledTerminal;

  /// No description provided for @sessionsDeleteTerminalTitle.
  ///
  /// In en, this message translates to:
  /// **'Delete this terminal?'**
  String get sessionsDeleteTerminalTitle;

  /// No description provided for @sessionsDeleteTerminalBody.
  ///
  /// In en, this message translates to:
  /// **'“{title}” closes on {machine} and its shell process ends.'**
  String sessionsDeleteTerminalBody(String title, String machine);

  /// No description provided for @sessionsDeleteConversationTitle.
  ///
  /// In en, this message translates to:
  /// **'Delete this conversation?'**
  String get sessionsDeleteConversationTitle;

  /// No description provided for @sessionsDeleteConversationBody.
  ///
  /// In en, this message translates to:
  /// **'“{title}” stops on {machine} and leaves the session list. The agent\'s transcript stays in History.'**
  String sessionsDeleteConversationBody(String title, String machine);

  /// No description provided for @sessionsDeleteFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t delete: {message}'**
  String sessionsDeleteFailed(String message);

  /// No description provided for @sessionsHistoryTitle.
  ///
  /// In en, this message translates to:
  /// **'History'**
  String get sessionsHistoryTitle;

  /// No description provided for @sessionsHistoryNoAgents.
  ///
  /// In en, this message translates to:
  /// **'This machine has no agents to resume conversations with.'**
  String get sessionsHistoryNoAgents;

  /// No description provided for @sessionsHistoryEmptyTitle.
  ///
  /// In en, this message translates to:
  /// **'Nothing to resume'**
  String get sessionsHistoryEmptyTitle;

  /// No description provided for @sessionsHistoryEmptyBody.
  ///
  /// In en, this message translates to:
  /// **'{agent} has no saved conversations in {project}.'**
  String sessionsHistoryEmptyBody(String agent, String project);

  /// No description provided for @sessionsHistoryLoadFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t load history'**
  String get sessionsHistoryLoadFailed;

  /// No description provided for @sessionsHistoryMessages.
  ///
  /// In en, this message translates to:
  /// **'{count, plural, =1{1 message} other{{count} messages}}'**
  String sessionsHistoryMessages(int count);

  /// No description provided for @sessionsHistoryOriginal.
  ///
  /// In en, this message translates to:
  /// **'Originally “{title}”'**
  String sessionsHistoryOriginal(String title);

  /// No description provided for @sessionsHistoryRenameTitle.
  ///
  /// In en, this message translates to:
  /// **'Rename conversation'**
  String get sessionsHistoryRenameTitle;

  /// No description provided for @sessionsHistoryRenameHelp.
  ///
  /// In en, this message translates to:
  /// **'Leave empty to go back to the agent\'s own title.'**
  String get sessionsHistoryRenameHelp;

  /// No description provided for @sessionsHistoryResetName.
  ///
  /// In en, this message translates to:
  /// **'Use original name'**
  String get sessionsHistoryResetName;

  /// No description provided for @sessionsHistoryRenameFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t rename: {message}'**
  String sessionsHistoryRenameFailed(String message);

  /// No description provided for @sessionsHistoryResumeFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t resume: {message}'**
  String sessionsHistoryResumeFailed(String message);

  /// No description provided for @sessionsHistoryResuming.
  ///
  /// In en, this message translates to:
  /// **'Resuming…'**
  String get sessionsHistoryResuming;

  /// No description provided for @settingsTitle.
  ///
  /// In en, this message translates to:
  /// **'Settings'**
  String get settingsTitle;

  /// No description provided for @settingsMachines.
  ///
  /// In en, this message translates to:
  /// **'Machines'**
  String get settingsMachines;

  /// No description provided for @settingsPairNew.
  ///
  /// In en, this message translates to:
  /// **'Pair a new machine'**
  String get settingsPairNew;

  /// No description provided for @settingsNoMachines.
  ///
  /// In en, this message translates to:
  /// **'No machines yet. Pair one and its sessions show up here.'**
  String get settingsNoMachines;

  /// No description provided for @settingsReorderHint.
  ///
  /// In en, this message translates to:
  /// **'Touch and hold a machine to drag it into a new order.'**
  String get settingsReorderHint;

  /// No description provided for @settingsOpenMachine.
  ///
  /// In en, this message translates to:
  /// **'Details for {name}'**
  String settingsOpenMachine(String name);

  /// No description provided for @settingsAppearance.
  ///
  /// In en, this message translates to:
  /// **'Appearance'**
  String get settingsAppearance;

  /// No description provided for @settingsThemeSystem.
  ///
  /// In en, this message translates to:
  /// **'System'**
  String get settingsThemeSystem;

  /// No description provided for @settingsThemeLight.
  ///
  /// In en, this message translates to:
  /// **'Light'**
  String get settingsThemeLight;

  /// No description provided for @settingsThemeDark.
  ///
  /// In en, this message translates to:
  /// **'Dark'**
  String get settingsThemeDark;

  /// No description provided for @settingsTerminal.
  ///
  /// In en, this message translates to:
  /// **'Terminal'**
  String get settingsTerminal;

  /// No description provided for @settingsTerminalFontSize.
  ///
  /// In en, this message translates to:
  /// **'Font size'**
  String get settingsTerminalFontSize;

  /// No description provided for @settingsTerminalFontValue.
  ///
  /// In en, this message translates to:
  /// **'{size} pt'**
  String settingsTerminalFontValue(int size);

  /// No description provided for @settingsTerminalFontSmaller.
  ///
  /// In en, this message translates to:
  /// **'Smaller text'**
  String get settingsTerminalFontSmaller;

  /// No description provided for @settingsTerminalFontLarger.
  ///
  /// In en, this message translates to:
  /// **'Larger text'**
  String get settingsTerminalFontLarger;

  /// No description provided for @settingsTerminalPreview.
  ///
  /// In en, this message translates to:
  /// **'~/code/smelt \$ cargo test'**
  String get settingsTerminalPreview;

  /// No description provided for @settingsAbout.
  ///
  /// In en, this message translates to:
  /// **'About'**
  String get settingsAbout;

  /// No description provided for @settingsAboutTagline.
  ///
  /// In en, this message translates to:
  /// **'Approve, answer and follow the agents on your machines.'**
  String get settingsAboutTagline;

  /// No description provided for @settingsDeviceId.
  ///
  /// In en, this message translates to:
  /// **'This device\'s ID'**
  String get settingsDeviceId;

  /// No description provided for @settingsLicenses.
  ///
  /// In en, this message translates to:
  /// **'Open-source licenses'**
  String get settingsLicenses;

  /// No description provided for @shellPickAgentHint.
  ///
  /// In en, this message translates to:
  /// **'Pick an agent or automation to see its details.'**
  String get shellPickAgentHint;

  /// Terminal page subtitle: attaching to the terminal, nothing on screen is live yet.
  ///
  /// In en, this message translates to:
  /// **'Connecting…'**
  String get terminalStateConnecting;

  /// Terminal page subtitle: attached, output is streaming.
  ///
  /// In en, this message translates to:
  /// **'Live'**
  String get terminalStateConnected;

  /// No description provided for @terminalStateReconnecting.
  ///
  /// In en, this message translates to:
  /// **'Reconnecting…'**
  String get terminalStateReconnecting;

  /// No description provided for @terminalStateEnded.
  ///
  /// In en, this message translates to:
  /// **'Ended'**
  String get terminalStateEnded;

  /// No description provided for @terminalCopyOutput.
  ///
  /// In en, this message translates to:
  /// **'Copy output'**
  String get terminalCopyOutput;

  /// No description provided for @terminalCopySelection.
  ///
  /// In en, this message translates to:
  /// **'Copy selection'**
  String get terminalCopySelection;

  /// No description provided for @terminalTextSize.
  ///
  /// In en, this message translates to:
  /// **'Text size'**
  String get terminalTextSize;

  /// No description provided for @terminalTextSizePoints.
  ///
  /// In en, this message translates to:
  /// **'{size} pt'**
  String terminalTextSizePoints(int size);

  /// No description provided for @terminalShowKeyboard.
  ///
  /// In en, this message translates to:
  /// **'Show keyboard'**
  String get terminalShowKeyboard;

  /// No description provided for @terminalHideKeyboard.
  ///
  /// In en, this message translates to:
  /// **'Hide keyboard'**
  String get terminalHideKeyboard;

  /// No description provided for @terminalKeyboardUnavailable.
  ///
  /// In en, this message translates to:
  /// **'Can\'t type right now'**
  String get terminalKeyboardUnavailable;

  /// No description provided for @terminalNothingToCopy.
  ///
  /// In en, this message translates to:
  /// **'Nothing to copy'**
  String get terminalNothingToCopy;

  /// No description provided for @terminalCopyFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t copy'**
  String get terminalCopyFailed;

  /// No description provided for @terminalNothingToPaste.
  ///
  /// In en, this message translates to:
  /// **'The clipboard is empty'**
  String get terminalNothingToPaste;

  /// No description provided for @terminalPasteFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t read the clipboard'**
  String get terminalPasteFailed;

  /// No description provided for @terminalOpenLinkFailed.
  ///
  /// In en, this message translates to:
  /// **'Couldn\'t open the link'**
  String get terminalOpenLinkFailed;

  /// No description provided for @terminalSelectAll.
  ///
  /// In en, this message translates to:
  /// **'Select all'**
  String get terminalSelectAll;

  /// No description provided for @terminalPaste.
  ///
  /// In en, this message translates to:
  /// **'Paste'**
  String get terminalPaste;

  /// No description provided for @terminalOpenLink.
  ///
  /// In en, this message translates to:
  /// **'Open link'**
  String get terminalOpenLink;

  /// Button that scrolls the terminal back to the newest output.
  ///
  /// In en, this message translates to:
  /// **'Latest'**
  String get terminalJumpToLatest;

  /// No description provided for @terminalJumpToLatestHint.
  ///
  /// In en, this message translates to:
  /// **'Jump to the newest output'**
  String get terminalJumpToLatestHint;

  /// No description provided for @terminalLoadingHistory.
  ///
  /// In en, this message translates to:
  /// **'Loading earlier output'**
  String get terminalLoadingHistory;

  /// No description provided for @terminalSessionEnded.
  ///
  /// In en, this message translates to:
  /// **'This terminal session has ended.'**
  String get terminalSessionEnded;

  /// No description provided for @terminalConnectionLost.
  ///
  /// In en, this message translates to:
  /// **'Lost this terminal. It may have been closed on the machine.'**
  String get terminalConnectionLost;

  /// No description provided for @terminalConnectionDropped.
  ///
  /// In en, this message translates to:
  /// **'Connection dropped. Reconnecting…'**
  String get terminalConnectionDropped;

  /// No description provided for @terminalKeyEscape.
  ///
  /// In en, this message translates to:
  /// **'Escape'**
  String get terminalKeyEscape;

  /// No description provided for @terminalKeyCtrl.
  ///
  /// In en, this message translates to:
  /// **'Ctrl, applies to the next key'**
  String get terminalKeyCtrl;

  /// No description provided for @terminalKeyTab.
  ///
  /// In en, this message translates to:
  /// **'Tab'**
  String get terminalKeyTab;

  /// No description provided for @terminalKeyShiftTab.
  ///
  /// In en, this message translates to:
  /// **'Shift+Tab'**
  String get terminalKeyShiftTab;

  /// No description provided for @terminalKeyInterrupt.
  ///
  /// In en, this message translates to:
  /// **'Interrupt (Ctrl+C)'**
  String get terminalKeyInterrupt;

  /// No description provided for @terminalKeyLeft.
  ///
  /// In en, this message translates to:
  /// **'Left'**
  String get terminalKeyLeft;

  /// No description provided for @terminalKeyDown.
  ///
  /// In en, this message translates to:
  /// **'Down'**
  String get terminalKeyDown;

  /// No description provided for @terminalKeyUp.
  ///
  /// In en, this message translates to:
  /// **'Up'**
  String get terminalKeyUp;

  /// No description provided for @terminalKeyRight.
  ///
  /// In en, this message translates to:
  /// **'Right'**
  String get terminalKeyRight;

  /// No description provided for @terminalKeyNewLine.
  ///
  /// In en, this message translates to:
  /// **'Shift+Enter (new line)'**
  String get terminalKeyNewLine;

  /// No description provided for @terminalKeyPageUp.
  ///
  /// In en, this message translates to:
  /// **'Page up'**
  String get terminalKeyPageUp;

  /// No description provided for @terminalKeyPageDown.
  ///
  /// In en, this message translates to:
  /// **'Page down'**
  String get terminalKeyPageDown;
}

class _L10nDelegate extends LocalizationsDelegate<L10n> {
  const _L10nDelegate();

  @override
  Future<L10n> load(Locale locale) {
    return SynchronousFuture<L10n>(lookupL10n(locale));
  }

  @override
  bool isSupported(Locale locale) =>
      <String>['en', 'zh'].contains(locale.languageCode);

  @override
  bool shouldReload(_L10nDelegate old) => false;
}

L10n lookupL10n(Locale locale) {
  // Lookup logic when only language code is specified.
  switch (locale.languageCode) {
    case 'en':
      return L10nEn();
    case 'zh':
      return L10nZh();
  }

  throw FlutterError(
    'L10n.delegate failed to load unsupported locale "$locale". This is likely '
    'an issue with the localizations generation tool. Please file an issue '
    'on GitHub with a reproducible sample app and the gen-l10n configuration '
    'that was used.',
  );
}
