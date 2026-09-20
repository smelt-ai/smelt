import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

import '../models/acp_snapshot.dart';
import '../theme/smelt_theme.dart';

/// Agent 主动发问时的作答卡（elicitation）。
///
/// 从会话页里抽出来，一是为了让「Submit 按钮会被滚出视野」这个缺陷可以被测到
/// ——会话页依赖全局 `gatewayService` 单例，测试里替换不掉；二是这张卡除了几个
/// 回调之外并不需要知道网关的存在。
///
/// 文本框的值由调用方持有（[textValues]）：快照刷新时页面会用远端值补齐本地
/// 未编辑的字段，那份合并逻辑留在页面里。
class ElicitationCard extends StatelessWidget {
  const ElicitationCard({
    super.key,
    required this.elicitation,
    required this.textValues,
    required this.onTextChanged,
    required this.onChoose,
    required this.onSubmit,
    required this.onDismiss,
  });

  final PendingElicitation elicitation;
  final Map<int, String> textValues;
  final void Function(int fieldIndex, String value) onTextChanged;
  final void Function(int fieldIndex, int optionIndex) onChoose;
  final VoidCallback onSubmit;
  final VoidCallback onDismiss;

  /// 单个单选字段时不显示按钮行：点中选项本身就是提交。
  bool get _singleSelect =>
      elicitation.fields.length == 1 &&
      elicitation.fields.first.kind is ElicitationSelect;

  @override
  Widget build(BuildContext context) {
    final ready = elicitation.isReady(localTextValues: textValues);
    final accent = context.smeltColors.needsAttention;

    // 高度上限按屏幕比例给，不再写死 360px：横屏或小屏上 360 可能已经吃掉整个
    // 可用高度，把下面的对话列表挤没。
    final maxHeight = (MediaQuery.sizeOf(context).height * 0.45).clamp(
      180.0,
      360.0,
    );

    return Container(
      width: double.infinity,
      constraints: BoxConstraints(maxHeight: maxHeight),
      margin: const EdgeInsets.fromLTRB(12, 8, 12, 4),
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: accent.withAlpha(20),
        border: Border.all(color: accent),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          // 只有问题和字段滚，按钮钉在卡片底部。原来整卡一起滚，字段一多
          // Submit 就被滚出视野，而且用户看不出下面还有个必须点的东西。
          Flexible(
            child: SingleChildScrollView(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Icon(Icons.help_outline, color: accent, size: 20),
                      const SizedBox(width: 8),
                      const Text(
                        'Your input is needed',
                        style: TextStyle(fontWeight: FontWeight.bold),
                      ),
                    ],
                  ),
                  if (elicitation.message.isNotEmpty) ...[
                    const SizedBox(height: 6),
                    Text(elicitation.message),
                  ],
                  const SizedBox(height: 10),
                  for (final entry in elicitation.fields.asMap().entries)
                    _ElicitationField(
                      elicitation: elicitation,
                      fieldIndex: entry.key,
                      field: entry.value,
                      textValues: textValues,
                      onTextChanged: onTextChanged,
                      onChoose: onChoose,
                    ),
                ],
              ),
            ),
          ),
          if (!_singleSelect)
            // Wrap 而不是 Row：窄屏上这两个按钮的文字加起来会横向溢出。
            Wrap(
              spacing: 8,
              runSpacing: 4,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                FilledButton(
                  onPressed: ready ? onSubmit : null,
                  child: const Text('Submit'),
                ),
                TextButton(
                  onPressed: onDismiss,
                  child: const Text('Answer in text instead'),
                ),
              ],
            ),
        ],
      ),
    );
  }
}

class _ElicitationField extends StatelessWidget {
  const _ElicitationField({
    required this.elicitation,
    required this.fieldIndex,
    required this.field,
    required this.textValues,
    required this.onTextChanged,
    required this.onChoose,
  });

  final PendingElicitation elicitation;
  final int fieldIndex;
  final ElicitationField field;
  final Map<int, String> textValues;
  final void Function(int fieldIndex, String value) onTextChanged;
  final void Function(int fieldIndex, int optionIndex) onChoose;

  @override
  Widget build(BuildContext context) {
    final input = switch (field.kind) {
      ElicitationSelect(options: final options) ||
      ElicitationMultiSelect(options: final options) => Wrap(
        spacing: 8,
        runSpacing: 6,
        children: options.asMap().entries.map((entry) {
          final selected =
              elicitation.chosen[fieldIndex]?.contains(entry.key) == true;
          return ChoiceChip(
            label: Text(entry.value.label),
            selected: selected,
            onSelected: (_) => onChoose(fieldIndex, entry.key),
          );
        }).toList(),
      ),
      ElicitationText(secret: final secret) => TextFormField(
        initialValue:
            textValues[fieldIndex] ?? elicitation.textValues[fieldIndex] ?? '',
        obscureText: secret,
        decoration: const InputDecoration(border: OutlineInputBorder()),
        onChanged: (value) => onTextChanged(fieldIndex, value),
      ),
      ElicitationExternalUrl(url: final url) => Row(
        children: [
          Expanded(child: SelectableText(url)),
          IconButton(
            tooltip: 'Open link',
            onPressed: () {
              final uri = Uri.tryParse(url);
              if (uri != null) {
                launchUrl(uri, mode: LaunchMode.externalApplication);
              }
            },
            icon: const Icon(Icons.open_in_new),
          ),
        ],
      ),
    };

    return Padding(
      padding: const EdgeInsets.only(bottom: 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(bottom: 6),
            child: Text(field.title, style: const TextStyle(fontSize: 13)),
          ),
          input,
        ],
      ),
    );
  }
}
