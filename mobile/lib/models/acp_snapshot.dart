/// ACP 快照数据模型
/// 
/// 直接解析 smeltd 返回的原始 JSON 格式，与 PC GUI 保持一致。

/// ACP 会话快照
class AcpSnapshot {
  final List<AcpEntry> entries;
  final AcpPhase phase;
  final PendingPermission? pendingPermission;
  final PendingElicitation? pendingElicitation;
  final String? statusLine;
  final String? acpSessionId;
  final bool supportsImage;
  final List<List<String>> availableCommands;
  final AcpUsage? usage;
  final AcpPlan? plan;
  final AcpModel? model;
  final bool completedUnread;
  final bool shouldPersist;

  AcpSnapshot({
    required this.entries,
    required this.phase,
    this.pendingPermission,
    this.pendingElicitation,
    this.statusLine,
    this.acpSessionId,
    this.supportsImage = true,
    this.availableCommands = const [],
    this.usage,
    this.plan,
    this.model,
    this.completedUnread = false,
    this.shouldPersist = false,
  });

  factory AcpSnapshot.fromJson(Map<String, dynamic> json) {
    // smeltd 返回格式: {"snapshot": {...}}
    final data = json['snapshot'] as Map<String, dynamic>? ?? json;
    
    return AcpSnapshot(
      entries: (data['entries'] as List<dynamic>?)
          ?.map((e) => AcpEntry.fromJson(e))
          .toList() ?? [],
      phase: AcpPhase.fromJson(data['phase']),
      pendingPermission: data['pending_permission'] != null
          ? PendingPermission.fromJson(data['pending_permission'])
          : null,
      pendingElicitation: data['pending_elicitation'] != null
          ? PendingElicitation.fromJson(data['pending_elicitation'])
          : null,
      statusLine: data['status_line'] as String?,
      acpSessionId: data['acp_session_id'] as String?,
      supportsImage: data['supports_image'] as bool? ?? true,
      availableCommands: (data['available_commands'] as List<dynamic>?)
          ?.map((cmd) => (cmd as List<dynamic>).map((e) => e.toString()).toList())
          .toList() ?? [],
      usage: data['usage'] != null ? AcpUsage.fromJson(data['usage']) : null,
      plan: data['plan'] != null ? AcpPlan.fromJson(data['plan']) : null,
      model: data['model'] != null ? AcpModel.fromJson(data['model']) : null,
      completedUnread: data['completed_unread'] as bool? ?? false,
      shouldPersist: data['should_persist'] as bool? ?? false,
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

  bool get isActive => this is AcpPhaseRunning || 
                        this is AcpPhaseAwaitingApproval || 
                        this is AcpPhaseAwaitingChoice;
}

class AcpPhaseStarting extends AcpPhase { const AcpPhaseStarting(); }
class AcpPhaseIdle extends AcpPhase { const AcpPhaseIdle(); }
class AcpPhaseRunning extends AcpPhase { const AcpPhaseRunning(); }
class AcpPhaseAwaitingApproval extends AcpPhase { const AcpPhaseAwaitingApproval(); }
class AcpPhaseAwaitingChoice extends AcpPhase { const AcpPhaseAwaitingChoice(); }
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
        output: (tool['output'] as List<dynamic>?)
            ?.map((o) => ToolOutputPart.fromJson(o))
            .toList() ?? [],
      );
    }
    
    // 分隔线
    if (json.containsKey('Divider')) {
      final divider = json['Divider'];
      return AcpEntryDivider(
        label: divider is String ? divider : (divider?['label'] as String? ?? ''),
      );
    }
    
    return const AcpEntryUnknown();
  }
}

class AcpEntryUser extends AcpEntry {
  final String text;
  const AcpEntryUser({required this.text});
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

/// 工具类型
enum ToolKind {
  read,
  edit,
  execute,
  other;

  factory ToolKind.fromJson(dynamic json) {
    if (json is String) {
      return switch (json.toLowerCase()) {
        'read' => ToolKind.read,
        'edit' || 'write' || 'delete' || 'move' => ToolKind.edit,
        'execute' || 'bash' => ToolKind.execute,
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
        hunks: (diff['hunks'] as List<dynamic>?)?.cast<String>() ?? [],
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
  final List<String> hunks;
  const ToolOutputDiff({required this.path, this.hunks = const []});
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

  const PendingPermission({
    required this.toolCallId,
    required this.question,
    required this.options,
  });

  factory PendingPermission.fromJson(Map<String, dynamic> json) {
    return PendingPermission(
      toolCallId: json['tool_call_id'] as String? ?? '',
      question: json['question'] as String? ?? '',
      options: (json['options'] as List<dynamic>?)
          ?.map((o) => PermissionOption.fromJson(o))
          .toList() ?? [],
    );
  }
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

  bool get isAllow => kind.startsWith('Allow');
  bool get isReject => kind.startsWith('Reject');
  bool get isAlways => kind.endsWith('Always');
}

/// 待处理的询问
class PendingElicitation {
  final String message;
  
  const PendingElicitation({required this.message});

  factory PendingElicitation.fromJson(Map<String, dynamic> json) {
    return PendingElicitation(
      message: json['message'] as String? ?? '',
    );
  }
}

/// 用量统计
class AcpUsage {
  final int inputTokens;
  final int outputTokens;
  final double? costUsd;

  const AcpUsage({
    this.inputTokens = 0,
    this.outputTokens = 0,
    this.costUsd,
  });

  factory AcpUsage.fromJson(Map<String, dynamic> json) {
    return AcpUsage(
      inputTokens: json['input_tokens'] as int? ?? 0,
      outputTokens: json['output_tokens'] as int? ?? 0,
      costUsd: (json['cost_usd'] as num?)?.toDouble(),
    );
  }
}

/// 计划
class AcpPlan {
  final List<AcpPlanStep> steps;

  const AcpPlan({this.steps = const []});

  factory AcpPlan.fromJson(Map<String, dynamic> json) {
    return AcpPlan(
      steps: (json['steps'] as List<dynamic>?)
          ?.map((s) => AcpPlanStep.fromJson(s))
          .toList() ?? [],
    );
  }
}

class AcpPlanStep {
  final String title;
  final String status;

  const AcpPlanStep({required this.title, this.status = 'pending'});

  factory AcpPlanStep.fromJson(Map<String, dynamic> json) {
    return AcpPlanStep(
      title: json['title'] as String? ?? '',
      status: json['status'] as String? ?? 'pending',
    );
  }
}

/// 模型信息
class AcpModel {
  final String currentName;
  final List<List<String>> options;

  const AcpModel({required this.currentName, this.options = const []});

  factory AcpModel.fromJson(Map<String, dynamic> json) {
    return AcpModel(
      currentName: json['current_name'] as String? ?? '',
      options: (json['options'] as List<dynamic>?)
          ?.map((o) => (o as List<dynamic>).map((e) => e.toString()).toList())
          .toList() ?? [],
    );
  }
}
