/// 新建或编辑一条自动化：名称、做什么（智能体 + 指令，或一行命令）、什么时候跑、
/// Webhook 和通知。事件触发手机上只读，保存时原样带回。
///
/// 自动化在那台机器上，保存是整条覆盖：内核先取完整的那条，只换这里有的字段（见
/// `crates/smelt-mobile/src/catalog/automation_edit.rs`）。
library;

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../../ui/shelf.dart';
import 'agents_board.dart';
import 'agents_widgets.dart';
import 'automation_text.dart';

/// 编辑页关掉时的结果：删掉了的话，详情页跟着退。
enum AutomationEditResult { saved, deleted }

class AutomationEditPage extends StatefulWidget {
  const AutomationEditPage({
    super.key,
    required this.board,
    required this.runtimeId,
    this.automationId,
  });

  final AgentsBoard board;
  final String runtimeId;

  /// 为空是新建。
  final String? automationId;

  @override
  State<AutomationEditPage> createState() => _AutomationEditPageState();
}

class _AutomationEditPageState extends State<AutomationEditPage> {
  final _name = TextEditingController();
  final _prompt = TextEditingController();
  final _command = TextEditingController();

  AutomationEditor? _editor;
  String? _loadError;

  AutomationActionKind _action = AutomationActionKind.agent;
  String _agentId = '';
  List<AutomationSchedule> _schedules = const [];
  bool _webhook = false;
  bool _notify = true;

  bool _saving = false;
  bool _deleting = false;

  bool get _creating => widget.automationId == null;

  @override
  void initState() {
    super.initState();
    for (final c in [_name, _prompt, _command]) {
      c.addListener(_changed);
    }
    unawaited(_load());
  }

  @override
  void dispose() {
    _name.dispose();
    _prompt.dispose();
    _command.dispose();
    super.dispose();
  }

  void _changed() => setState(() {});

  void _unfocus(PointerDownEvent _) => FocusScope.of(context).unfocus();

  Future<void> _load() async {
    if (_loadError != null) setState(() => _loadError = null);
    try {
      final editor = await AppScope.of(context).backend.automationEditor(
        widget.runtimeId,
        automationId: widget.automationId,
      );
      if (!mounted) return;
      final draft = editor.draft;
      _name.text = draft.name;
      _prompt.text = draft.prompt;
      _command.text = draft.command;
      setState(() {
        _editor = editor;
        _action = draft.action;
        _agentId = draft.agentDefinitionId;
        _schedules = draft.schedules;
        _webhook = draft.webhook;
        _notify = draft.notify;
      });
    } catch (error) {
      if (mounted) setState(() => _loadError = describeError(error));
    }
  }

  AutomationDraft get _draft => AutomationDraft(
    name: _name.text,
    action: _action,
    agentDefinitionId: _agentId,
    prompt: _prompt.text,
    command: _command.text,
    schedules: _schedules,
    webhook: _webhook,
    notify: _notify,
  );

  /// 按内容比：生成的 `==` 对列表只比引用。
  bool get _dirty {
    final editor = _editor;
    if (editor == null) return false;
    final a = _draft;
    final b = editor.draft;
    return a.name != b.name ||
        a.action != b.action ||
        a.agentDefinitionId != b.agentDefinitionId ||
        a.prompt != b.prompt ||
        a.command != b.command ||
        !listEquals(a.schedules, b.schedules) ||
        a.webhook != b.webhook ||
        a.notify != b.notify;
  }

  /// 还差什么才能保存。规则和运行时的校验一致（`Automation::validate_for_save`），这里
  /// 先说出来，免得点了保存才被拒。
  List<String> _missing(L10n l10n) {
    final editor = _editor;
    final events = editor?.eventTopics.isNotEmpty ?? false;
    return [
      if (_name.text.trim().isEmpty) l10n.autoEditMissingName,
      if (_schedules.isEmpty && !_webhook && !events)
        l10n.autoEditMissingTrigger,
      if (_action == AutomationActionKind.agent && _agentId.trim().isEmpty)
        l10n.autoEditMissingAgent,
      if (_action == AutomationActionKind.agent &&
          _schedules.isNotEmpty &&
          _prompt.text.trim().isEmpty)
        l10n.autoEditMissingPrompt,
      if (_action == AutomationActionKind.shell && _command.text.trim().isEmpty)
        l10n.autoEditMissingCommand,
    ];
  }

  bool _canSave(L10n l10n) =>
      _editor != null &&
      !_saving &&
      !_deleting &&
      _missing(l10n).isEmpty &&
      (_creating || _dirty);

  Future<void> _save() async {
    final l10n = L10n.of(context);
    if (!_canSave(l10n)) return;
    final messenger = ScaffoldMessenger.of(context);
    setState(() => _saving = true);
    try {
      await widget.board.saveAutomation(
        widget.runtimeId,
        automationId: widget.automationId,
        draft: _draft,
      );
      if (mounted) Navigator.of(context).pop(AutomationEditResult.saved);
    } catch (error) {
      messenger.showSnackBar(
        SnackBar(content: Text(l10n.autoEditSaveFailed(describeError(error)))),
      );
      if (mounted) setState(() => _saving = false);
    }
  }

  Future<void> _delete() async {
    final automationId = widget.automationId;
    if (automationId == null) return;
    final l10n = L10n.of(context);
    final model = AppScope.of(context);
    final messenger = ScaffoldMessenger.of(context);
    final machine = model.fleet.runtime(widget.runtimeId)?.name ?? '';
    final name = _editor?.draft.name ?? '';
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.autoEditDeleteTitle(name)),
        content: Text(l10n.autoEditDeleteBody(machine)),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.commonCancel),
          ),
          GalleyButton.danger(
            label: l10n.commonDelete,
            onPressed: () => Navigator.pop(context, true),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    setState(() => _deleting = true);
    try {
      await widget.board.deleteAutomation(widget.runtimeId, automationId);
      if (mounted) Navigator.of(context).pop(AutomationEditResult.deleted);
    } catch (error) {
      messenger.showSnackBar(
        SnackBar(
          content: Text(l10n.autoEditDeleteFailed(describeError(error))),
        ),
      );
      if (mounted) setState(() => _deleting = false);
    }
  }

  Future<void> _confirmLeave() async {
    final l10n = L10n.of(context);
    final navigator = Navigator.of(context);
    final discard = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.autoEditDiscardTitle),
        content: Text(l10n.autoEditDiscardBody),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.commonCancel),
          ),
          GalleyButton.danger(
            label: l10n.autoEditDiscard,
            onPressed: () => Navigator.pop(context, true),
          ),
        ],
      ),
    );
    if (discard == true) navigator.pop();
  }

  Future<void> _pickAgent(AutomationEditor editor) async {
    final picked = await showModalBottomSheet<String>(
      context: context,
      useSafeArea: true,
      builder: (context) =>
          _AgentSheet(agents: editor.agents, selected: _agentId),
    );
    if (picked != null) setState(() => _agentId = picked);
  }

  /// 编辑第 [index] 条规则；[index] 为空是新加一条。
  Future<void> _editSchedule(int? index) async {
    final initial = index == null ? _newSchedule : _schedules[index];
    final edited = await showModalBottomSheet<AutomationSchedule>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      builder: (context) => ScheduleSheet(initial: initial),
    );
    if (edited == null) return;
    setState(() {
      _schedules = index == null
          ? [..._schedules, edited]
          : [for (final (i, s) in _schedules.indexed) i == index ? edited : s];
    });
  }

  static const _newSchedule = AutomationSchedule(
    kind: ScheduleKind.daily,
    hour: 9,
    minute: 0,
    minutes: 0,
    hours: 0,
    days: 0,
    endHour: 0,
    endMinute: 0,
  );

  @override
  Widget build(BuildContext context) {
    final model = AppScope.of(context);
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final runtime = model.fleet.runtime(widget.runtimeId);
    final busy = _saving || _deleting;
    return PopScope(
      canPop: !_dirty && !busy,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop && !busy) unawaited(_confirmLeave());
      },
      child: Scaffold(
        backgroundColor: c.ground,
        appBar: AppBar(
          backgroundColor: c.ground,
          title: DetailTitle(
            title: _creating ? l10n.autoEditNewTitle : l10n.autoEditTitle,
            runtime: runtime,
          ),
          actions: [
            Padding(
              padding: const EdgeInsetsDirectional.only(end: Space.sm),
              child: TextButton(
                onPressed: _canSave(l10n) ? _save : null,
                child: Text(_saving ? l10n.autoEditSaving : l10n.commonSave),
              ),
            ),
          ],
        ),
        body: _body(context),
      ),
    );
  }

  Widget _body(BuildContext context) {
    final l10n = L10n.of(context);
    final editor = _editor;
    if (editor == null) {
      final error = _loadError;
      if (error == null) {
        return const Center(child: CircularProgressIndicator.adaptive());
      }
      return Center(
        child: EmptyNote(
          icon: LucideIcons.cloudOff,
          title: l10n.autoEditLoadFailed,
          body: error,
          action: GalleyButton.solid(label: l10n.commonRetry, onPressed: _load),
        ),
      );
    }
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final enabled = !_saving && !_deleting;
    final missing = _missing(l10n);
    final agentName = editor.agents
        .where((agent) => agent.id == _agentId)
        .map((agent) => agent.name)
        .firstOrNull;
    return ListView(
      keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
      padding: EdgeInsets.only(
        bottom: MediaQuery.paddingOf(context).bottom + Space.xxl,
      ),
      children: [
        if (missing.isNotEmpty && (_dirty || !_creating))
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Space.page + Space.xs,
              Space.sm,
              Space.page + Space.xs,
              0,
            ),
            child: Text(
              l10n.autoEditMissing(missing.join(l10n.agentsDaysSeparator)),
              style: t.note.copyWith(color: c.inkMuted),
            ),
          ),
        ShelfHeader(title: l10n.autoEditName, first: true),
        EditField(
          child: TextField(
            controller: _name,
            enabled: enabled,
            autofocus: _creating,
            textInputAction: TextInputAction.next,
            onTapOutside: _unfocus,
            style: t.body.copyWith(color: c.ink),
            decoration: InputDecoration(hintText: l10n.autoEditNameHint),
          ),
        ),
        ShelfHeader(title: l10n.autoEditAction),
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: Space.page),
          child: SegmentedButton<AutomationActionKind>(
            showSelectedIcon: false,
            segments: [
              ButtonSegment(
                value: AutomationActionKind.agent,
                label: Text(l10n.autoEditActionAgent),
              ),
              ButtonSegment(
                value: AutomationActionKind.shell,
                label: Text(l10n.autoEditActionShell),
              ),
            ],
            selected: {_action},
            onSelectionChanged: enabled
                ? (selection) => setState(() => _action = selection.first)
                : null,
          ),
        ),
        const SizedBox(height: Space.md),
        if (_action == AutomationActionKind.agent) ...[
          Shelf(
            children: [
              if (editor.agents.isEmpty)
                NoteRow(text: l10n.autoEditAgentNone)
              else
                ShelfRow(
                  leading: Icon(
                    LucideIcons.bot,
                    size: Strokes.icon,
                    color: c.inkMuted,
                  ),
                  title: Text(agentName ?? l10n.autoEditAgentPick),
                  titleStyle: agentName == null
                      ? t.body.copyWith(color: c.inkMuted)
                      : null,
                  trailing: Icon(
                    LucideIcons.chevronRight,
                    size: Strokes.iconSmall,
                    color: c.inkFaint,
                  ),
                  onTap: enabled ? () => _pickAgent(editor) : null,
                ),
            ],
          ),
          ShelfHeader(title: l10n.autoEditPrompt),
          EditField(
            child: TextField(
              controller: _prompt,
              enabled: enabled,
              minLines: 4,
              maxLines: null,
              keyboardType: TextInputType.multiline,
              onTapOutside: _unfocus,
              style: t.body.copyWith(color: c.ink),
              decoration: InputDecoration(hintText: l10n.autoEditPromptHint),
            ),
          ),
        ] else ...[
          EditField(
            child: TextField(
              controller: _command,
              enabled: enabled,
              minLines: 2,
              maxLines: null,
              keyboardType: TextInputType.multiline,
              autocorrect: false,
              enableSuggestions: false,
              onTapOutside: _unfocus,
              style: t.code.copyWith(color: c.ink),
              decoration: InputDecoration(hintText: l10n.autoEditCommandHint),
            ),
          ),
        ],
        ShelfHeader(title: l10n.autoEditSchedules),
        Shelf(
          children: [
            for (final (index, schedule) in _schedules.indexed)
              ShelfRow(
                leading: Icon(
                  LucideIcons.clock,
                  size: Strokes.icon,
                  color: c.inkMuted,
                ),
                title: Text(scheduleLabel(l10n, schedule)),
                trailing: IconButton(
                  tooltip: l10n.autoEditRemoveSchedule,
                  icon: const Icon(LucideIcons.x),
                  onPressed: enabled
                      ? () => setState(
                          () => _schedules = [
                            for (final (i, s) in _schedules.indexed)
                              if (i != index) s,
                          ],
                        )
                      : null,
                ),
                onTap: enabled ? () => _editSchedule(index) : null,
              ),
            AddRow(
              label: l10n.autoEditAddSchedule,
              onTap: enabled ? () => _editSchedule(null) : null,
            ),
          ],
        ),
        if (editor.eventTopics.isNotEmpty) ...[
          ShelfHeader(title: l10n.autoEditEvents),
          Shelf(
            children: [
              for (final topic in editor.eventTopics)
                NoteRow(
                  icon: LucideIcons.zap,
                  text: l10n.agentsTriggerEvent(topic),
                ),
            ],
          ),
        ],
        const SizedBox(height: Space.section),
        Shelf(
          children: [
            ShelfRow(
              title: Text(l10n.autoEditWebhook),
              subtitle: Text(l10n.autoEditWebhookHint, maxLines: 3),
              trailing: Switch(
                value: _webhook,
                onChanged: enabled
                    ? (value) => setState(() => _webhook = value)
                    : null,
              ),
            ),
            ShelfRow(
              title: Text(l10n.autoEditNotify),
              trailing: Switch(
                value: _notify,
                onChanged: enabled
                    ? (value) => setState(() => _notify = value)
                    : null,
              ),
            ),
          ],
        ),
        if (!_creating)
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Space.page,
              Space.section,
              Space.page,
              0,
            ),
            child: GalleyButton.danger(
              expand: true,
              icon: LucideIcons.trash2,
              label: l10n.autoEditDelete,
              onPressed: enabled ? _delete : null,
            ),
          ),
      ],
    );
  }
}

class _AgentSheet extends StatelessWidget {
  const _AgentSheet({required this.agents, required this.selected});

  final List<AutomationAgentChoice> agents;
  final String selected;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    return ListView(
      shrinkWrap: true,
      padding: EdgeInsets.only(
        bottom: MediaQuery.paddingOf(context).bottom + Space.lg,
      ),
      children: [
        ShelfHeader(title: l10n.autoEditAgent, first: true),
        HairlineRows(
          children: [
            for (final agent in agents)
              ShelfRow(
                leading: Icon(
                  LucideIcons.bot,
                  size: Strokes.icon,
                  color: c.ink,
                ),
                title: Text(agent.name),
                trailing: agent.id == selected
                    ? Icon(LucideIcons.check, size: Strokes.icon, color: c.ink)
                    : null,
                onTap: () => Navigator.pop(context, agent.id),
              ),
          ],
        ),
      ],
    );
  }
}

/// 一条定时规则的编辑面板。四种：每天几点、每周哪几天几点、每隔 N 分钟 / 小时、
/// 哪几天的某个时段内每隔 N 分钟。和桌面的触发器表单一一对应。
class ScheduleSheet extends StatefulWidget {
  const ScheduleSheet({super.key, required this.initial});

  final AutomationSchedule initial;

  @override
  State<ScheduleSheet> createState() => _ScheduleSheetState();
}

enum _Kind { daily, weekly, every, during }

/// 间隔的上限，和运行时的校验一致（`AutomationSchedule::validate`）。
const _maxMinutes = 1440;
const _maxHours = 168;
const _weekdayMask = 0x1f;

class _ScheduleSheetState extends State<ScheduleSheet> {
  late _Kind _kind = switch (widget.initial.kind) {
    ScheduleKind.daily => _Kind.daily,
    ScheduleKind.weekly => _Kind.weekly,
    ScheduleKind.everyMinutes || ScheduleKind.everyHours => _Kind.every,
    ScheduleKind.during => _Kind.during,
  };
  late TimeOfDay _time = TimeOfDay(
    hour: widget.initial.hour,
    minute: widget.initial.minute,
  );
  late TimeOfDay _end = widget.initial.kind == ScheduleKind.during
      ? TimeOfDay(
          hour: widget.initial.endHour,
          minute: widget.initial.endMinute,
        )
      : const TimeOfDay(hour: 18, minute: 0);
  late int _days = widget.initial.days == 0
      ? _weekdayMask
      : widget.initial.days;
  late bool _hoursUnit = widget.initial.kind == ScheduleKind.everyHours;
  late final _interval = TextEditingController(
    text: switch (widget.initial.kind) {
      ScheduleKind.everyHours => '${widget.initial.hours}',
      ScheduleKind.everyMinutes ||
      ScheduleKind.during => '${widget.initial.minutes}',
      _ => '30',
    },
  );

  @override
  void initState() {
    super.initState();
    _interval.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _interval.dispose();
    super.dispose();
  }

  int? get _intervalValue {
    final value = int.tryParse(_interval.text.trim());
    final max = _kind == _Kind.every && _hoursUnit ? _maxHours : _maxMinutes;
    return value != null && value >= 1 && value <= max ? value : null;
  }

  bool get _valid => switch (_kind) {
    _Kind.daily => true,
    _Kind.weekly => _days != 0,
    _Kind.every => _intervalValue != null,
    _Kind.during =>
      _days != 0 &&
          _intervalValue != null &&
          (_end.hour * 60 + _end.minute) > (_time.hour * 60 + _time.minute),
  };

  AutomationSchedule get _result {
    final interval = _intervalValue ?? 0;
    return AutomationSchedule(
      kind: switch (_kind) {
        _Kind.daily => ScheduleKind.daily,
        _Kind.weekly => ScheduleKind.weekly,
        _Kind.every =>
          _hoursUnit ? ScheduleKind.everyHours : ScheduleKind.everyMinutes,
        _Kind.during => ScheduleKind.during,
      },
      hour: _kind == _Kind.every ? 0 : _time.hour,
      minute: _kind == _Kind.every ? 0 : _time.minute,
      minutes: switch (_kind) {
        _Kind.every when !_hoursUnit => interval,
        _Kind.during => interval,
        _ => 0,
      },
      hours: _kind == _Kind.every && _hoursUnit ? interval : 0,
      days: _kind == _Kind.weekly || _kind == _Kind.during ? _days : 0,
      endHour: _kind == _Kind.during ? _end.hour : 0,
      endMinute: _kind == _Kind.during ? _end.minute : 0,
    );
  }

  Future<void> _pickTime(bool end) async {
    final picked = await showTimePicker(
      context: context,
      initialTime: end ? _end : _time,
    );
    if (picked == null) return;
    setState(() => end ? _end = picked : _time = picked);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final material = MaterialLocalizations.of(context);
    String clock(TimeOfDay time) =>
        material.formatTimeOfDay(time, alwaysUse24HourFormat: true);
    Widget timeRow(String label, TimeOfDay value, bool end) => ShelfRow(
      title: Text(label),
      trailing: Text(clock(value), style: t.monoTime.copyWith(color: c.ink)),
      onTap: () => _pickTime(end),
    );
    return Padding(
      padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
      child: ListView(
        shrinkWrap: true,
        padding: EdgeInsets.only(
          bottom: MediaQuery.paddingOf(context).bottom + Space.lg,
        ),
        children: [
          ShelfHeader(
            title: l10n.autoEditScheduleTitle,
            first: true,
            trailing: TextButton(
              onPressed: _valid ? () => Navigator.pop(context, _result) : null,
              child: Text(l10n.autoEditDone),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: Space.page),
            child: SegmentedButton<_Kind>(
              showSelectedIcon: false,
              segments: [
                ButtonSegment(
                  value: _Kind.daily,
                  label: Text(l10n.autoEditKindDaily),
                ),
                ButtonSegment(
                  value: _Kind.weekly,
                  label: Text(l10n.autoEditKindWeekly),
                ),
                ButtonSegment(
                  value: _Kind.every,
                  label: Text(l10n.autoEditKindEvery),
                ),
                ButtonSegment(
                  value: _Kind.during,
                  label: Text(l10n.autoEditKindDuring),
                ),
              ],
              selected: {_kind},
              onSelectionChanged: (selection) =>
                  setState(() => _kind = selection.first),
            ),
          ),
          const SizedBox(height: Space.md),
          if (_kind == _Kind.weekly || _kind == _Kind.during) ...[
            ShelfHeader(title: l10n.autoEditDays),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: Space.page),
              child: Wrap(
                spacing: Space.sm,
                runSpacing: Space.sm,
                children: [
                  for (final (bit, key) in _dayKeys.indexed)
                    FilterChip(
                      label: Text(l10n.agentsWeekday(key)),
                      selected: _days & (1 << bit) != 0,
                      onSelected: (on) => setState(
                        () => _days = on
                            ? _days | (1 << bit)
                            : _days & ~(1 << bit),
                      ),
                    ),
                ],
              ),
            ),
            const SizedBox(height: Space.md),
          ],
          Shelf(
            children: [
              if (_kind == _Kind.daily || _kind == _Kind.weekly)
                timeRow(l10n.autoEditTime, _time, false),
              if (_kind == _Kind.during) ...[
                timeRow(l10n.autoEditStart, _time, false),
                timeRow(l10n.autoEditEnd, _end, true),
              ],
              if (_kind == _Kind.every || _kind == _Kind.during)
                Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: Space.lg,
                    vertical: Space.sm,
                  ),
                  child: Row(
                    children: [
                      // 和上面「从 / 到」的行标题同一字级。
                      Text(
                        l10n.autoEditInterval,
                        style: t.headline.copyWith(color: c.ink),
                      ),
                      const SizedBox(width: Space.md),
                      SizedBox(
                        width: 88,
                        child: TextField(
                          controller: _interval,
                          keyboardType: TextInputType.number,
                          textAlign: TextAlign.end,
                          style: t.monoTime.copyWith(color: c.ink),
                          decoration: InputDecoration(
                            isDense: true,
                            errorText: _intervalValue == null
                                ? l10n.autoEditIntervalRange(
                                    1,
                                    _kind == _Kind.every && _hoursUnit
                                        ? _maxHours
                                        : _maxMinutes,
                                  )
                                : null,
                          ),
                        ),
                      ),
                      const SizedBox(width: Space.md),
                      if (_kind == _Kind.every)
                        SegmentedButton<bool>(
                          showSelectedIcon: false,
                          segments: [
                            ButtonSegment(
                              value: false,
                              label: Text(l10n.autoEditMinutes),
                            ),
                            ButtonSegment(
                              value: true,
                              label: Text(l10n.autoEditHours),
                            ),
                          ],
                          selected: {_hoursUnit},
                          onSelectionChanged: (selection) =>
                              setState(() => _hoursUnit = selection.first),
                        )
                      else
                        Text(l10n.autoEditMinutes, style: t.body),
                    ],
                  ),
                ),
            ],
          ),
        ],
      ),
    );
  }
}

const _dayKeys = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'];
