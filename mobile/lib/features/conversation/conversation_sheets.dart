import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/shelf.dart';

/// 底部弹层里的一个候选。
class SheetChoice {
  const SheetChoice({
    required this.value,
    required this.name,
    required this.selected,
    this.enabled = true,
    this.description,
  });

  final String value;
  final String name;
  final bool selected;
  final bool enabled;
  final String? description;
}

/// 底部弹层外框：标题 + 可滚动的行，行间发丝线。
class _SheetFrame extends StatelessWidget {
  const _SheetFrame({required this.title, required this.children, this.note});

  final String title;
  final String? note;
  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return SafeArea(
      top: false,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(Space.xl, 0, Space.xl, Space.sm),
            child: Semantics(
              header: true,
              child: Text(title, style: t.title.copyWith(color: c.ink)),
            ),
          ),
          if (note != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.xl,
                0,
                Space.xl,
                Space.sm,
              ),
              child: Text(note!, style: t.meta.copyWith(color: c.inkMuted)),
            ),
          Flexible(
            child: ListView.separated(
              shrinkWrap: true,
              padding: const EdgeInsets.only(bottom: Space.lg),
              itemCount: children.length,
              separatorBuilder: (_, _) => Padding(
                padding: const EdgeInsetsDirectional.only(start: Space.lg),
                child: Divider(height: Strokes.hairline, color: c.hairline),
              ),
              itemBuilder: (_, i) => children[i],
            ),
          ),
        ],
      ),
    );
  }
}

Future<T?> _showSheet<T>(BuildContext context, WidgetBuilder builder) =>
    showModalBottomSheet<T>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      constraints: BoxConstraints(
        maxHeight: MediaQuery.sizeOf(context).height * 0.85,
        maxWidth: Space.readingMax,
      ),
      builder: builder,
    );

/// 在一组候选里选一个，返回选中的值；关掉弹层返回 null。
Future<String?> showChoiceSheet(
  BuildContext context, {
  required String title,
  required List<SheetChoice> choices,
}) => _showSheet<String>(context, (context) {
  final c = GalleyColors.of(context);
  return _SheetFrame(
    title: title,
    children: [
      for (final choice in choices)
        Semantics(
          selected: choice.selected,
          enabled: choice.enabled,
          child: ShelfRow(
            title: Text(choice.name),
            titleStyle: GalleyText.of(
              context,
            ).body.copyWith(color: choice.enabled ? c.ink : c.inkFaint),
            subtitle: choice.description == null
                ? null
                : Text(choice.description!),
            trailing: choice.selected
                ? Icon(LucideIcons.check, size: Strokes.icon, color: c.ink)
                : null,
            onTap: choice.enabled && !choice.selected
                ? () => Navigator.pop(context, choice.value)
                : null,
          ),
        ),
    ],
  );
});

/// 模型与配置的写回动作，由页面给（失败提示也在页面）。
typedef SetConfig =
    void Function(String configId, String valueId, {bool? boolean});

/// 换模型：协议按 provider 分组时只列当前 provider 下的模型。
Future<void> pickModel(
  BuildContext context,
  ModelSelector model,
  SetConfig set,
) async {
  final l10n = L10n.of(context);
  final value = await showChoiceSheet(
    context,
    title: l10n.conversationModel,
    choices: [
      for (final o in model.options)
        SheetChoice(value: o.value, name: o.name, selected: o.selected),
    ],
  );
  if (value != null) set(model.configId, value);
}

/// 换 provider。协议只认模型值，没有「只换 provider」的写法：切过去写的是这一组的
/// `switchValue`；给不出目标模型的空组只列出来，不假装能点。
Future<void> pickProvider(
  BuildContext context,
  ModelSelector model,
  SetConfig set,
) async {
  final l10n = L10n.of(context);
  final value = await showChoiceSheet(
    context,
    title: l10n.conversationProvider,
    choices: [
      for (final p in model.providers)
        SheetChoice(
          value: p.switchValue ?? '',
          name: p.name,
          selected: p.selected,
          enabled: p.switchValue != null,
        ),
    ],
  );
  if (value != null && value.isNotEmpty) set(model.configId, value);
}

Future<void> pickConfig(
  BuildContext context,
  ConfigOption config,
  SetConfig set,
) async {
  final value = await showChoiceSheet(
    context,
    title: config.name,
    choices: [
      for (final o in config.options)
        SheetChoice(value: o.value, name: o.name, selected: o.selected),
    ],
  );
  if (value != null) set(config.configId, value);
}

ModelProvider? currentProvider(ModelSelector model) {
  final i = model.currentProvider;
  return i != null && i >= 0 && i < model.providers.length
      ? model.providers[i]
      : null;
}

/// 「模型与配置」总表：每项一行，点开再选；开关项直接拨。
Future<void> showSettingsSheet(
  BuildContext context, {
  required ConversationView view,
  required SetConfig set,
}) => _showSheet<void>(context, (sheetContext) {
  final l10n = L10n.of(sheetContext);
  final c = GalleyColors.of(sheetContext);
  final model = view.model;
  final provider = model == null ? null : currentProvider(model);
  final chevron = Icon(
    LucideIcons.chevronRight,
    size: Strokes.iconSmall,
    color: c.inkFaint,
  );
  final rows = <Widget>[
    if (model != null && provider != null)
      ShelfRow(
        title: Text(l10n.conversationProvider),
        subtitle: Text(provider.name),
        trailing: chevron,
        onTap: model.providers.length > 1
            ? () => pickProvider(sheetContext, model, set)
            : null,
      ),
    if (model != null && model.currentName.isNotEmpty)
      ShelfRow(
        title: Text(l10n.conversationModel),
        subtitle: Text(model.currentName),
        trailing: chevron,
        onTap: model.options.length > 1
            ? () => pickModel(sheetContext, model, set)
            : null,
      ),
    for (final config in view.configOptions)
      if (config.boolean case final on?)
        _SwitchRow(config: config, value: on, set: set)
      else if (config.options.length > 1)
        ShelfRow(
          title: Text(config.name),
          subtitle: Text(config.currentName),
          trailing: chevron,
          onTap: () => pickConfig(sheetContext, config, set),
        ),
  ];
  return _SheetFrame(
    title: l10n.conversationModelAndSettings,
    note: rows.isEmpty ? l10n.conversationNoSettings : null,
    children: rows,
  );
});

/// 开关项：拨了立刻写回，结果从视图流回来；弹层里先按拨后的样子显示。
class _SwitchRow extends StatefulWidget {
  const _SwitchRow({
    required this.config,
    required this.value,
    required this.set,
  });

  final ConfigOption config;
  final bool value;
  final SetConfig set;

  @override
  State<_SwitchRow> createState() => _SwitchRowState();
}

class _SwitchRowState extends State<_SwitchRow> {
  late bool _on = widget.value;

  @override
  Widget build(BuildContext context) {
    return MergeSemantics(
      child: ShelfRow(
        title: Text(widget.config.name),
        subtitle: widget.config.description == null
            ? null
            : Text(widget.config.description!),
        trailing: Switch.adaptive(
          value: _on,
          onChanged: (next) {
            setState(() => _on = next);
            widget.set(widget.config.configId, '', boolean: next);
          },
        ),
      ),
    );
  }
}

/// 斜杠命令与技能。选了命令返回命令名（不带斜杠）；技能只读：装什么技能是电脑上的事。
Future<String?> showCommandsSheet(
  BuildContext context, {
  required List<SlashCommand> commands,
  required List<SessionSkill>? skills,
}) => _showSheet<String>(context, (sheetContext) {
  final l10n = L10n.of(sheetContext);
  final c = GalleyColors.of(sheetContext);
  final t = GalleyText.of(sheetContext);
  return _SheetFrame(
    title: l10n.conversationCommands,
    children: [
      for (final command in commands)
        ShelfRow(
          title: Text('/${command.name}'),
          titleStyle: t.codeRow.copyWith(color: c.ink),
          subtitle: command.description.isEmpty
              ? null
              : Text(command.description),
          onTap: () => Navigator.pop(sheetContext, command.name),
        ),
      if (skills != null) ...[
        ShelfHeader(title: l10n.conversationSkills),
        ..._skillRows(sheetContext, skills),
      ],
    ],
  );
});

Future<void> showSkillsSheet(
  BuildContext context, {
  required List<SessionSkill> skills,
}) => _showSheet<void>(context, (sheetContext) {
  final l10n = L10n.of(sheetContext);
  return _SheetFrame(
    title: l10n.conversationSkills,
    note: l10n.conversationSkillsNote,
    children: _skillRows(sheetContext, skills),
  );
});

List<Widget> _skillRows(BuildContext context, List<SessionSkill> skills) {
  final l10n = L10n.of(context);
  final c = GalleyColors.of(context);
  final t = GalleyText.of(context);
  if (skills.isEmpty) {
    return [
      Padding(
        padding: const EdgeInsets.all(Space.lg),
        child: Text(
          l10n.conversationSkillsEmpty,
          style: t.meta.copyWith(color: c.inkMuted),
        ),
      ),
    ];
  }
  return [
    for (final skill in skills)
      ShelfRow(
        title: Text(skill.name),
        subtitle: skill.description.isEmpty ? null : Text(skill.description),
      ),
  ];
}
