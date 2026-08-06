// ACP snapshot models mirroring smeltd's JSON representation.

/// ACP 会话快照
class AcpSnapshot {
  final int entriesOffset;
  final int entriesTotal;
  final int snapshotRevision;
  final List<AcpEntry> entries;
  final AcpPhase phase;
  final List<PendingPermission> pendingPermissions;
  final PendingElicitation? pendingElicitation;
  final String? statusLine;
  final String? acpSessionId;
  final String? historySessionId;
  final bool supportsImage;
  final List<List<String>> availableCommands;
  final AcpUsage? usage;
  final AcpPlan? plan;
  final AcpModel? model;
  final List<AcpSessionConfig> configOptions;
  final int? turnStartedAtMs;
  final int? lastTurnDurationMs;
  final bool completedUnread;
  final bool shouldPersist;

  AcpSnapshot({
    this.entriesOffset = 0,
    int? entriesTotal,
    this.snapshotRevision = 0,
    required this.entries,
    required this.phase,
    this.pendingPermissions = const [],
    this.pendingElicitation,
    this.statusLine,
    this.acpSessionId,
    this.historySessionId,
    this.supportsImage = true,
    this.availableCommands = const [],
    this.usage,
    this.plan,
    this.model,
    this.configOptions = const [],
    this.turnStartedAtMs,
    this.lastTurnDurationMs,
    this.completedUnread = false,
    this.shouldPersist = false,
  }) : entriesTotal = entriesTotal ?? entriesOffset + entries.length;

  int get entriesEnd => entriesOffset + entries.length;
  bool get hasMoreBefore => entriesOffset > 0;
  String? get stableHistoryId => historySessionId ?? acpSessionId;

  factory AcpSnapshot.fromJson(Map<String, dynamic> json) {
    // smeltd 返回格式: {"snapshot": {...}}
    final data = json['snapshot'] as Map<String, dynamic>? ?? json;

    return AcpSnapshot(
      entriesOffset: data['entries_offset'] as int? ?? 0,
      entriesTotal:
          data['entries_total'] as int? ??
          (data['entries_offset'] as int? ?? 0) +
              ((data['entries'] as List<dynamic>?)?.length ?? 0),
      snapshotRevision: (data['snapshot_revision'] as num?)?.toInt() ?? 0,
      entries:
          (data['entries'] as List<dynamic>?)
              ?.map((e) => AcpEntry.fromJson(e))
              .toList() ??
          [],
      phase: AcpPhase.fromJson(data['phase']),
      pendingPermissions:
          (data['pending_permissions'] as List<dynamic>?)
              ?.whereType<Map<String, dynamic>>()
              .map(PendingPermission.fromJson)
              .toList() ??
          [
            if (data['pending_permission']
                case final Map<String, dynamic> value)
              PendingPermission.fromJson(value),
          ],
      pendingElicitation: data['pending_elicitation'] != null
          ? PendingElicitation.fromJson(data['pending_elicitation'])
          : null,
      statusLine: data['status_line'] as String?,
      acpSessionId: data['acp_session_id'] as String?,
      historySessionId: data['history_session_id'] as String?,
      supportsImage: data['supports_image'] as bool? ?? true,
      availableCommands:
          (data['available_commands'] as List<dynamic>?)
              ?.map(
                (cmd) =>
                    (cmd as List<dynamic>).map((e) => e.toString()).toList(),
              )
              .toList() ??
          [],
      usage: data['usage'] != null ? AcpUsage.fromJson(data['usage']) : null,
      plan: data['plan'] != null ? AcpPlan.fromJson(data['plan']) : null,
      model: data['model'] != null ? AcpModel.fromJson(data['model']) : null,
      configOptions:
          (data['config_options'] as List<dynamic>?)
              ?.whereType<Map<String, dynamic>>()
              .map(AcpSessionConfig.fromJson)
              .toList() ??
          [],
      turnStartedAtMs: (data['turn_started_at_ms'] as num?)?.toInt(),
      lastTurnDurationMs: (data['last_turn_duration_ms'] as num?)?.toInt(),
      completedUnread: data['completed_unread'] as bool? ?? false,
      shouldPersist: data['should_persist'] as bool? ?? false,
    );
  }

  Map<String, dynamic> toJson() => {
    'entries_offset': entriesOffset,
    'entries_total': entriesTotal,
    'snapshot_revision': snapshotRevision,
    'entries': entries.map(_entryToJson).toList(),
    'phase': _phaseToJson(phase),
    'pending_permissions': pendingPermissions
        .map((permission) => permission.toJson())
        .toList(),
    if (pendingElicitation != null)
      'pending_elicitation': pendingElicitation!.toJson(),
    if (statusLine != null) 'status_line': statusLine,
    if (acpSessionId != null) 'acp_session_id': acpSessionId,
    if (historySessionId != null) 'history_session_id': historySessionId,
    'supports_image': supportsImage,
    'available_commands': availableCommands,
    if (usage != null) 'usage': usage!.toJson(),
    if (plan != null) 'plan': plan!.toJson(),
    if (model != null) 'model': model!.toJson(),
    'config_options': configOptions.map((option) => option.toJson()).toList(),
    if (turnStartedAtMs != null) 'turn_started_at_ms': turnStartedAtMs,
    if (lastTurnDurationMs != null) 'last_turn_duration_ms': lastTurnDurationMs,
    'completed_unread': completedUnread,
    'should_persist': shouldPersist,
  };

  AcpSnapshot cacheTail({int limit = 100}) {
    if (entries.length <= limit) return this;
    final skipped = entries.length - limit;
    return _withEntries(
      entriesOffset + skipped,
      entries.skip(skipped).toList(growable: false),
    );
  }

  /// Apply smeltd's tail snapshot to the previously rendered projection.
  AcpSnapshot merge(AcpSnapshot next) {
    final currentHistory = stableHistoryId;
    final nextHistory = next.stableHistoryId;
    if (currentHistory != null &&
        nextHistory != null &&
        currentHistory != nextHistory) {
      return next;
    }

    final currentStart = entriesOffset;
    final currentEnd = entriesEnd;
    final nextStart = next.entriesOffset;
    final nextEnd = next.entriesEnd;
    final metadata = next.snapshotRevision >= snapshotRevision ? next : this;

    // Windows must overlap or touch. A disjoint latest tail is authoritative;
    // an isolated older page is ignored because rendering it would create a gap.
    if (nextEnd < currentStart || nextStart > currentEnd) {
      return nextEnd == next.entriesTotal ? next : _withWindowFrom(metadata);
    }

    late final int mergedStart;
    late final List<AcpEntry> mergedEntries;
    if (nextStart <= currentStart) {
      mergedStart = nextStart;
      final oldSuffix = (nextEnd - currentStart).clamp(0, entries.length);
      mergedEntries = [...next.entries, ...entries.skip(oldSuffix)];
    } else {
      mergedStart = currentStart;
      final prefixLength = (nextStart - currentStart).clamp(0, entries.length);
      final suffixOffset = (nextEnd - currentStart).clamp(0, entries.length);
      mergedEntries = [
        ...entries.take(prefixLength),
        ...next.entries,
        ...entries.skip(suffixOffset),
      ];
    }
    final allowedLength = (metadata.entriesTotal - mergedStart).clamp(
      0,
      mergedEntries.length,
    );
    return metadata._withEntries(
      mergedStart,
      mergedEntries.take(allowedLength).toList(growable: false),
    );
  }

  AcpSnapshot _withWindowFrom(AcpSnapshot metadata) => metadata._withEntries(
    entriesOffset,
    entries,
    entriesTotalOverride: metadata.entriesTotal,
  );

  AcpSnapshot _withEntries(
    int offset,
    List<AcpEntry> value, {
    int? entriesTotalOverride,
  }) {
    return AcpSnapshot(
      entriesOffset: offset,
      entriesTotal: entriesTotalOverride ?? entriesTotal,
      snapshotRevision: snapshotRevision,
      entries: value,
      phase: phase,
      pendingPermissions: pendingPermissions,
      pendingElicitation: pendingElicitation,
      statusLine: statusLine,
      acpSessionId: acpSessionId,
      historySessionId: historySessionId,
      supportsImage: supportsImage,
      availableCommands: availableCommands,
      usage: usage,
      plan: plan,
      model: model,
      configOptions: configOptions,
      turnStartedAtMs: turnStartedAtMs,
      lastTurnDurationMs: lastTurnDurationMs,
      completedUnread: completedUnread,
      shouldPersist: shouldPersist,
    );
  }
}

/// ACP 会话阶段
sealed class AcpPhase {
  const AcpPhase();

  factory AcpPhase.fromJson(dynamic json) {
    if (json == null) return const AcpPhaseIdle();
    if (json is String) {
      return switch (json) {
        'Starting' => const AcpPhaseStarting(),
        'Idle' => const AcpPhaseIdle(),
        'Running' => const AcpPhaseRunning(),
        'AwaitingApproval' => const AcpPhaseAwaitingApproval(),
        'AwaitingChoice' => const AcpPhaseAwaitingChoice(),
        _ => const AcpPhaseIdle(),
      };
    }
    if (json is Map<String, dynamic>) {
      if (json.containsKey('Ended')) {
        return AcpPhaseEnded(reason: json['Ended'] as String? ?? '');
      }
    }
    return const AcpPhaseIdle();
  }

  bool get isActive =>
      this is AcpPhaseRunning ||
      this is AcpPhaseAwaitingApproval ||
      this is AcpPhaseAwaitingChoice;

  bool get acceptsPrompt => this is AcpPhaseIdle || this is AcpPhaseRunning;
}

class AcpPhaseStarting extends AcpPhase {
  const AcpPhaseStarting();
}

class AcpPhaseIdle extends AcpPhase {
  const AcpPhaseIdle();
}

class AcpPhaseRunning extends AcpPhase {
  const AcpPhaseRunning();
}

class AcpPhaseAwaitingApproval extends AcpPhase {
  const AcpPhaseAwaitingApproval();
}

class AcpPhaseAwaitingChoice extends AcpPhase {
  const AcpPhaseAwaitingChoice();
}

class AcpPhaseEnded extends AcpPhase {
  final String reason;
  const AcpPhaseEnded({required this.reason});
}

/// ACP 条目（消息/工具调用）
sealed class AcpEntry {
  const AcpEntry();

  factory AcpEntry.fromJson(dynamic json) {
    if (json is! Map<String, dynamic>) return const AcpEntryUnknown();

    // User 消息: {"User": "text"} 或 {"User": {"content": [...]}}
    if (json.containsKey('User')) {
      final user = json['User'];
      if (user is String) {
        return AcpEntryUser(text: user);
      }
      if (user is Map<String, dynamic>) {
        final content = user['content'];
        if (content is List) {
          final text = content
              .whereType<Map<String, dynamic>>()
              .where((c) => c['type'] == 'text')
              .map((c) => c['text'] as String?)
              .whereType<String>()
              .join('\n');
          return AcpEntryUser(text: text);
        }
      }
      return AcpEntryUser(text: user.toString());
    }

    if (json['UserWithImages'] case final Map<String, dynamic> user) {
      return AcpEntryUserWithImages(
        text: user['text'] as String? ?? '',
        images:
            (user['images'] as List<dynamic>?)
                ?.whereType<Map<String, dynamic>>()
                .map(AcpImageData.fromJson)
                .toList() ??
            [],
      );
    }

    // Assistant 消息
    if (json.containsKey('Assistant')) {
      final assistant = json['Assistant'];
      if (assistant is Map<String, dynamic>) {
        return AcpEntryAssistant(
          text: assistant['text'] as String? ?? '',
          thought: assistant['thought'] as bool? ?? false,
        );
      }
      return AcpEntryAssistant(text: assistant.toString(), thought: false);
    }

    // 工具调用
    if (json.containsKey('ToolCall')) {
      final tool = json['ToolCall'] as Map<String, dynamic>;
      return AcpEntryToolCall(
        id: tool['id'] as String? ?? '',
        title: tool['title'] as String? ?? tool['name'] as String? ?? '',
        kind: ToolKind.fromJson(tool['kind']),
        status: ToolCallStatus.fromJson(tool['status']),
        output:
            (tool['output'] as List<dynamic>?)
                ?.map((o) => ToolOutputPart.fromJson(o))
                .toList() ??
            [],
      );
    }

    // 分隔线
    if (json.containsKey('Divider')) {
      final divider = json['Divider'];
      return AcpEntryDivider(
        label: divider is String
            ? divider
            : (divider?['label'] as String? ?? ''),
      );
    }

    return const AcpEntryUnknown();
  }
}

class AcpEntryUser extends AcpEntry {
  final String text;
  const AcpEntryUser({required this.text});
}

class AcpEntryUserWithImages extends AcpEntry {
  final String text;
  final List<AcpImageData> images;

  const AcpEntryUserWithImages({required this.text, required this.images});
}

class AcpImageData {
  final String mimeType;
  final String base64;

  const AcpImageData({required this.mimeType, required this.base64});

  factory AcpImageData.fromJson(Map<String, dynamic> json) => AcpImageData(
    mimeType: json['mime'] as String? ?? 'image/png',
    base64: json['data_b64'] as String? ?? '',
  );

  Map<String, String> toJson() => {'mime': mimeType, 'data_b64': base64};
}

class AcpEntryAssistant extends AcpEntry {
  final String text;
  final bool thought;
  const AcpEntryAssistant({required this.text, this.thought = false});
}

class AcpEntryToolCall extends AcpEntry {
  final String id;
  final String title;
  final ToolKind kind;
  final ToolCallStatus status;
  final List<ToolOutputPart> output;

  const AcpEntryToolCall({
    required this.id,
    required this.title,
    required this.kind,
    required this.status,
    this.output = const [],
  });
}

class AcpEntryDivider extends AcpEntry {
  final String label;
  const AcpEntryDivider({required this.label});
}

class AcpEntryUnknown extends AcpEntry {
  const AcpEntryUnknown();
}

bool isTaskCompletionToolTitle(String title) =>
    title.trim().toLowerCase() == 'task_complete';

String completionSummaryText(List<ToolOutputPart> output) => output
    .whereType<ToolOutputText>()
    .map((part) => part.text.trim())
    .where((text) => text.isNotEmpty)
    .join('\n\n');

dynamic _phaseToJson(AcpPhase phase) => switch (phase) {
  AcpPhaseStarting() => 'Starting',
  AcpPhaseIdle() => 'Idle',
  AcpPhaseRunning() => 'Running',
  AcpPhaseAwaitingApproval() => 'AwaitingApproval',
  AcpPhaseAwaitingChoice() => 'AwaitingChoice',
  AcpPhaseEnded(reason: final reason) => {'Ended': reason},
};

dynamic _entryToJson(AcpEntry entry) => switch (entry) {
  AcpEntryUser(text: final text) => {'User': text},
  AcpEntryUserWithImages(text: final text, images: final images) => {
    'UserWithImages': {
      'text': text,
      'images': images.map((image) => image.toJson()).toList(),
    },
  },
  AcpEntryAssistant(text: final text, thought: final thought) => {
    'Assistant': {'text': text, 'thought': thought},
  },
  AcpEntryToolCall(
    id: final id,
    title: final title,
    kind: final kind,
    status: final status,
    output: final output,
  ) =>
    {
      'ToolCall': {
        'id': id,
        'title': title,
        'kind': _toolKindToJson(kind),
        'status': _toolStatusToJson(status),
        'output': output.map(_toolOutputToJson).toList(),
      },
    },
  AcpEntryDivider(label: final label) => {
    'Divider': {'label': label},
  },
  AcpEntryUnknown() => const <String, dynamic>{},
};

String _toolKindToJson(ToolKind kind) => switch (kind) {
  ToolKind.switchMode => 'switch_mode',
  _ => kind.name,
};

String _toolStatusToJson(ToolCallStatus status) => switch (status) {
  ToolCallStatus.inProgress => 'in_progress',
  _ => status.name,
};

dynamic _toolOutputToJson(ToolOutputPart output) => switch (output) {
  ToolOutputText(text: final text) => {'Text': text},
  ToolOutputDiff(
    path: final path,
    oldText: final oldText,
    newText: final newText,
  ) =>
    {
      'Diff': {'path': path, 'old_text': oldText, 'new_text': newText},
    },
  ToolOutputImage(base64: final base64, mimeType: final mimeType) => {
    'Image': {'base64': base64, 'mime_type': mimeType},
  },
};

/// 工具类型
enum ToolKind {
  read,
  edit,
  delete,
  move,
  search,
  execute,
  think,
  fetch,
  switchMode,
  collaborate,
  review,
  image,
  compact,
  wait,
  other;

  factory ToolKind.fromJson(dynamic json) {
    if (json is String) {
      return switch (json.toLowerCase()) {
        'read' => ToolKind.read,
        'edit' || 'write' => ToolKind.edit,
        'delete' => ToolKind.delete,
        'move' => ToolKind.move,
        'search' => ToolKind.search,
        'execute' || 'bash' => ToolKind.execute,
        'think' => ToolKind.think,
        'fetch' => ToolKind.fetch,
        'switch_mode' => ToolKind.switchMode,
        'collaborate' => ToolKind.collaborate,
        'review' => ToolKind.review,
        'image' => ToolKind.image,
        'compact' => ToolKind.compact,
        'wait' => ToolKind.wait,
        _ => ToolKind.other,
      };
    }
    return ToolKind.other;
  }
}

/// 工具调用状态
enum ToolCallStatus {
  pending,
  inProgress,
  completed,
  failed;

  factory ToolCallStatus.fromJson(dynamic json) {
    if (json is String) {
      return switch (json) {
        'Pending' || 'pending' => ToolCallStatus.pending,
        'InProgress' || 'in_progress' => ToolCallStatus.inProgress,
        'Completed' || 'completed' => ToolCallStatus.completed,
        'Failed' || 'failed' => ToolCallStatus.failed,
        _ => ToolCallStatus.pending,
      };
    }
    return ToolCallStatus.pending;
  }

  bool get isRunning => this == pending || this == inProgress;
}

/// 工具输出部分
sealed class ToolOutputPart {
  const ToolOutputPart();

  factory ToolOutputPart.fromJson(dynamic json) {
    if (json is String) return ToolOutputText(text: json);
    if (json is! Map<String, dynamic>) return const ToolOutputText(text: '');

    if (json.containsKey('Text')) {
      return ToolOutputText(text: json['Text'] as String? ?? '');
    }
    if (json.containsKey('Diff')) {
      final diff = json['Diff'] as Map<String, dynamic>;
      return ToolOutputDiff(
        path: diff['path'] as String? ?? '',
        oldText: diff['old_text'] as String?,
        newText: diff['new_text'] as String? ?? '',
      );
    }
    if (json.containsKey('Image')) {
      final img = json['Image'] as Map<String, dynamic>;
      return ToolOutputImage(
        base64: img['base64'] as String? ?? '',
        mimeType: img['mime_type'] as String? ?? 'image/png',
      );
    }

    return ToolOutputText(text: json.toString());
  }
}

class ToolOutputText extends ToolOutputPart {
  final String text;
  const ToolOutputText({required this.text});
}

class ToolOutputDiff extends ToolOutputPart {
  final String path;
  final String? oldText;
  final String newText;

  const ToolOutputDiff({
    required this.path,
    this.oldText,
    required this.newText,
  });
}

class ToolOutputImage extends ToolOutputPart {
  final String base64;
  final String mimeType;
  const ToolOutputImage({required this.base64, this.mimeType = 'image/png'});
}

/// 待处理权限请求
class PendingPermission {
  final String toolCallId;
  final String question;
  final List<PermissionOption> options;
  final ApprovalDetails details;

  const PendingPermission({
    required this.toolCallId,
    required this.question,
    required this.options,
    this.details = const ApprovalDetailsGeneric(),
  });

  factory PendingPermission.fromJson(Map<String, dynamic> json) {
    return PendingPermission(
      toolCallId: json['tool_call_id'] as String? ?? '',
      question: json['question'] as String? ?? '',
      options:
          (json['options'] as List<dynamic>?)
              ?.map((o) => PermissionOption.fromJson(o))
              .toList() ??
          [],
      details: ApprovalDetails.fromJson(json['details']),
    );
  }

  Map<String, dynamic> toJson() => {
    'tool_call_id': toolCallId,
    'question': question,
    'options': options.map((option) => option.toJson()).toList(),
    'details': _approvalDetailsToJson(details),
  };
}

Map<String, dynamic> _approvalDetailsToJson(ApprovalDetails details) =>
    switch (details) {
      ApprovalDetailsCommand(
        command: final command,
        cwd: final cwd,
        reason: final reason,
      ) =>
        {'kind': 'command', 'command': command, 'cwd': cwd, 'reason': reason},
      ApprovalDetailsFileChange(reason: final reason, grantRoot: final root) =>
        {'kind': 'file_change', 'reason': reason, 'grant_root': root},
      ApprovalDetailsPermissions(summary: final summary) => {
        'kind': 'permissions',
        'summary': summary,
      },
      ApprovalDetailsGeneric() => {'kind': 'generic'},
    };

sealed class ApprovalDetails {
  const ApprovalDetails();

  factory ApprovalDetails.fromJson(dynamic json) {
    if (json is! Map<String, dynamic>) {
      return const ApprovalDetailsGeneric();
    }
    return switch (json['kind']) {
      'command' => ApprovalDetailsCommand(
        command: json['command'] as String? ?? '',
        cwd: json['cwd'] as String?,
        reason: json['reason'] as String?,
      ),
      'file_change' => ApprovalDetailsFileChange(
        reason: json['reason'] as String?,
        grantRoot: json['grant_root'] as String?,
      ),
      'permissions' => ApprovalDetailsPermissions(
        summary: json['summary'] as String? ?? '',
      ),
      _ => const ApprovalDetailsGeneric(),
    };
  }
}

class ApprovalDetailsCommand extends ApprovalDetails {
  final String command;
  final String? cwd;
  final String? reason;
  const ApprovalDetailsCommand({required this.command, this.cwd, this.reason});
}

class ApprovalDetailsFileChange extends ApprovalDetails {
  final String? reason;
  final String? grantRoot;
  const ApprovalDetailsFileChange({this.reason, this.grantRoot});
}

class ApprovalDetailsPermissions extends ApprovalDetails {
  final String summary;
  const ApprovalDetailsPermissions({required this.summary});
}

class ApprovalDetailsGeneric extends ApprovalDetails {
  const ApprovalDetailsGeneric();
}

/// 权限选项
class PermissionOption {
  final String optionId;
  final String name;
  final String kind;

  const PermissionOption({
    required this.optionId,
    required this.name,
    required this.kind,
  });

  factory PermissionOption.fromJson(Map<String, dynamic> json) {
    return PermissionOption(
      optionId: json['option_id'] as String? ?? '',
      name: json['name'] as String? ?? '',
      kind: json['kind'] as String? ?? 'AllowOnce',
    );
  }

  Map<String, dynamic> toJson() => {
    'option_id': optionId,
    'name': name,
    'kind': kind,
  };

  bool get isAllow => kind.startsWith('Allow');
  bool get isReject => kind.startsWith('Reject');
  bool get isAlways => kind.endsWith('Always');
}

/// 待处理的询问
class PendingElicitation {
  final String message;
  final List<ElicitationField> fields;
  final Map<int, List<int>> chosen;
  final Map<int, String> textValues;

  const PendingElicitation({
    required this.message,
    this.fields = const [],
    this.chosen = const {},
    this.textValues = const {},
  });

  factory PendingElicitation.fromJson(Map<String, dynamic> json) {
    return PendingElicitation(
      message: json['message'] as String? ?? '',
      fields:
          (json['fields'] as List<dynamic>?)
              ?.whereType<Map<String, dynamic>>()
              .map(ElicitationField.fromJson)
              .toList() ??
          [],
      chosen: _parseIndexMap<List<int>>(
        json['chosen'],
        (value) => (value as List<dynamic>)
            .map((index) => (index as num).toInt())
            .toList(),
      ),
      textValues: _parseIndexMap<String>(
        json['text_values'],
        (value) => value.toString(),
      ),
    );
  }

  Map<String, dynamic> toJson() => {
    'message': message,
    'fields': fields.map((field) => field.toJson()).toList(),
    'chosen': chosen.map((key, value) => MapEntry('$key', value)),
    'text_values': textValues.map((key, value) => MapEntry('$key', value)),
  };

  bool isReady({Map<int, String> localTextValues = const {}}) {
    return fields.asMap().entries.every((entry) {
      final field = entry.value;
      if (!field.required) return true;
      return switch (field.kind) {
        ElicitationSelect() ||
        ElicitationMultiSelect() => chosen[entry.key]?.isNotEmpty == true,
        ElicitationText() =>
          (localTextValues[entry.key] ?? textValues[entry.key])
                  ?.trim()
                  .isNotEmpty ==
              true,
        ElicitationExternalUrl() => true,
      };
    });
  }
}

Map<int, T> _parseIndexMap<T>(
  dynamic json,
  T Function(dynamic value) parseValue,
) {
  if (json is! Map<String, dynamic>) return {};
  final parsed = <int, T>{};
  for (final entry in json.entries) {
    final index = int.tryParse(entry.key);
    if (index != null) parsed[index] = parseValue(entry.value);
  }
  return parsed;
}

class ElicitationField {
  final String key;
  final String title;
  final bool required;
  final ElicitationFieldKind kind;

  const ElicitationField({
    required this.key,
    required this.title,
    this.required = false,
    required this.kind,
  });

  factory ElicitationField.fromJson(Map<String, dynamic> json) {
    return ElicitationField(
      key: json['key'] as String? ?? '',
      title: json['title'] as String? ?? '',
      // Legacy snapshots predate optional elicitation fields. Missing metadata
      // therefore means the old all-fields-required behavior, while new
      // snapshots can explicitly send false.
      required: json['required'] as bool? ?? true,
      kind: ElicitationFieldKind.fromJson(json['kind']),
    );
  }

  Map<String, dynamic> toJson() => {
    'key': key,
    'title': title,
    'required': required,
    'kind': _elicitationKindToJson(kind),
  };
}

Map<String, dynamic> _elicitationKindToJson(ElicitationFieldKind kind) =>
    switch (kind) {
      ElicitationSelect(options: final options) => {
        'Select': options.map((option) => {'label': option.label}).toList(),
      },
      ElicitationMultiSelect(options: final options) => {
        'MultiSelect': options
            .map((option) => {'label': option.label})
            .toList(),
      },
      ElicitationText(secret: final secret) => {
        'Text': {'secret': secret},
      },
      ElicitationExternalUrl(url: final url) => {'ExternalUrl': url},
    };

sealed class ElicitationFieldKind {
  const ElicitationFieldKind();

  factory ElicitationFieldKind.fromJson(dynamic json) {
    if (json is! Map<String, dynamic>) {
      return const ElicitationText(secret: false);
    }
    if (json['Select'] case final List<dynamic> options) {
      return ElicitationSelect(_parseElicitationOptions(options));
    }
    if (json['MultiSelect'] case final List<dynamic> options) {
      return ElicitationMultiSelect(_parseElicitationOptions(options));
    }
    if (json['Text'] case final Map<String, dynamic> text) {
      return ElicitationText(secret: text['secret'] as bool? ?? false);
    }
    if (json['ExternalUrl'] case final String url) {
      return ElicitationExternalUrl(url);
    }
    return const ElicitationText(secret: false);
  }
}

List<ElicitationOption> _parseElicitationOptions(List<dynamic> options) {
  return options
      .whereType<Map<String, dynamic>>()
      .map((option) => ElicitationOption(option['label'] as String? ?? ''))
      .toList();
}

class ElicitationSelect extends ElicitationFieldKind {
  final List<ElicitationOption> options;
  const ElicitationSelect(this.options);
}

class ElicitationMultiSelect extends ElicitationFieldKind {
  final List<ElicitationOption> options;
  const ElicitationMultiSelect(this.options);
}

class ElicitationText extends ElicitationFieldKind {
  final bool secret;
  const ElicitationText({required this.secret});
}

class ElicitationExternalUrl extends ElicitationFieldKind {
  final String url;
  const ElicitationExternalUrl(this.url);
}

class ElicitationOption {
  final String label;
  const ElicitationOption(this.label);
}

/// 用量统计
class AcpUsage {
  final int usedTokens;
  final int contextWindow;

  const AcpUsage({this.usedTokens = 0, this.contextWindow = 0});

  factory AcpUsage.fromJson(dynamic json) {
    if (json is List && json.length >= 2) {
      return AcpUsage(
        usedTokens: (json[0] as num?)?.toInt() ?? 0,
        contextWindow: (json[1] as num?)?.toInt() ?? 0,
      );
    }
    if (json is! Map<String, dynamic>) return const AcpUsage();
    return AcpUsage(
      usedTokens:
          (json['used_tokens'] as num?)?.toInt() ??
          (json['input_tokens'] as num?)?.toInt() ??
          0,
      contextWindow: (json['context_window'] as num?)?.toInt() ?? 0,
    );
  }

  Map<String, dynamic> toJson() => {
    'used_tokens': usedTokens,
    'context_window': contextWindow,
  };
}

/// 计划
class AcpPlan {
  final List<AcpPlanStep> steps;

  const AcpPlan({this.steps = const []});

  factory AcpPlan.fromJson(Map<String, dynamic> json) {
    return AcpPlan(
      steps:
          ((json['entries'] ?? json['steps']) as List<dynamic>?)
              ?.map((s) => AcpPlanStep.fromJson(s))
              .toList() ??
          [],
    );
  }

  Map<String, dynamic> toJson() => {
    'entries': steps.map((step) => step.toJson()).toList(),
  };
}

class AcpPlanStep {
  final String title;
  final String status;

  const AcpPlanStep({required this.title, this.status = 'pending'});

  factory AcpPlanStep.fromJson(Map<String, dynamic> json) {
    return AcpPlanStep(
      title: json['content'] as String? ?? json['title'] as String? ?? '',
      status: json['status'] as String? ?? 'pending',
    );
  }

  Map<String, dynamic> toJson() => {'content': title, 'status': status};

  bool get isCompleted => status.toLowerCase() == 'completed';
  bool get isInProgress =>
      status.toLowerCase() == 'inprogress' ||
      status.toLowerCase() == 'in_progress';
}

/// 模型信息
class AcpModel {
  final String configId;
  final String currentName;
  final List<List<String>> options;

  const AcpModel({
    required this.configId,
    required this.currentName,
    this.options = const [],
  });

  factory AcpModel.fromJson(Map<String, dynamic> json) {
    return AcpModel(
      configId: json['config_id'] as String? ?? '',
      currentName: json['current_name'] as String? ?? '',
      options:
          (json['options'] as List<dynamic>?)
              ?.map(
                (o) => (o as List<dynamic>).map((e) => e.toString()).toList(),
              )
              .toList() ??
          [],
    );
  }

  Map<String, dynamic> toJson() => {
    'config_id': configId,
    'current_name': currentName,
    'options': options,
  };
}

class AcpSessionConfig {
  final String configId;
  final String name;
  final String? description;
  final String currentName;
  final List<List<String>> options;

  const AcpSessionConfig({
    required this.configId,
    required this.name,
    this.description,
    required this.currentName,
    this.options = const [],
  });

  factory AcpSessionConfig.fromJson(Map<String, dynamic> json) {
    return AcpSessionConfig(
      configId: json['config_id'] as String? ?? '',
      name: json['name'] as String? ?? '',
      description: json['description'] as String?,
      currentName: json['current_name'] as String? ?? '',
      options:
          (json['options'] as List<dynamic>?)
              ?.map(
                (option) => (option as List<dynamic>)
                    .map((value) => value.toString())
                    .toList(),
              )
              .toList() ??
          [],
    );
  }

  Map<String, dynamic> toJson() => {
    'config_id': configId,
    'name': name,
    if (description != null) 'description': description,
    'current_name': currentName,
    'options': options,
  };
}
