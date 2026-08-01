import 'dart:convert';
import 'dart:io';

import 'package:path_provider/path_provider.dart';

import '../models/acp_snapshot.dart';

class MessageDraft {
  const MessageDraft({
    required this.content,
    this.images = const [],
    this.requestId,
  });

  final String content;
  final List<AcpImageData> images;
  final String? requestId;

  bool get isEmpty => content.trim().isEmpty && images.isEmpty;

  MessageDraft copyWith({String? requestId, bool clearRequestId = false}) {
    return MessageDraft(
      content: content,
      images: images,
      requestId: clearRequestId ? null : requestId ?? this.requestId,
    );
  }

  factory MessageDraft.fromJson(Map<String, dynamic> json) => MessageDraft(
    content: json['content'] as String? ?? '',
    images: (json['images'] as List<dynamic>? ?? const [])
        .whereType<Map<String, dynamic>>()
        .map(AcpImageData.fromJson)
        .toList(),
    requestId: json['requestId'] as String?,
  );

  Map<String, dynamic> toJson() => {
    'content': content,
    'images': images.map((image) => image.toJson()).toList(),
    if (requestId != null) 'requestId': requestId,
  };
}

abstract interface class MessageDraftStore {
  Future<MessageDraft?> load(String sessionId);
  Future<void> save(String sessionId, MessageDraft draft);
  Future<void> delete(String sessionId);

  /// Applies an acknowledgement only while the persisted draft still belongs
  /// to that request. Returns false when a newer draft has replaced it.
  Future<bool> resolveRequest(
    String sessionId,
    String requestId, {
    required bool succeeded,
  });
}

class FileMessageDraftStore implements MessageDraftStore {
  FileMessageDraftStore({Future<Directory> Function()? directoryProvider})
    : _directoryProvider = directoryProvider ?? getApplicationSupportDirectory;

  final Future<Directory> Function() _directoryProvider;
  final Map<String, Future<void>> _operations = {};

  String _fileName(String sessionId) =>
      '${base64UrlEncode(utf8.encode(sessionId)).replaceAll('=', '')}.json';

  Future<File> _file(String sessionId) async {
    final root = await _directoryProvider();
    final directory = Directory('${root.path}/message-drafts');
    await directory.create(recursive: true);
    return File('${directory.path}/${_fileName(sessionId)}');
  }

  @override
  Future<MessageDraft?> load(String sessionId) async {
    await (_operations[sessionId] ?? Future<void>.value()).catchError((_) {});
    return _readDraft(await _file(sessionId));
  }

  Future<MessageDraft?> _readDraft(File file) async {
    if (!await file.exists()) return null;
    try {
      final value = jsonDecode(await file.readAsString());
      if (value is! Map<String, dynamic>) return null;
      final draft = MessageDraft.fromJson(value);
      return draft.isEmpty ? null : draft;
    } on FormatException {
      return null;
    }
  }

  @override
  Future<void> save(String sessionId, MessageDraft draft) {
    return _enqueue(sessionId, () async {
      final file = await _file(sessionId);
      if (draft.isEmpty) {
        await _deleteFiles(file);
        return;
      }
      await _writeDraft(file, draft);
    });
  }

  @override
  Future<void> delete(String sessionId) => _enqueue(sessionId, () async {
    await _deleteFiles(await _file(sessionId));
  });

  @override
  Future<bool> resolveRequest(
    String sessionId,
    String requestId, {
    required bool succeeded,
  }) async {
    var resolved = false;
    await _enqueue(sessionId, () async {
      final file = await _file(sessionId);
      final draft = await _readDraft(file);
      if (draft?.requestId != requestId) return;
      resolved = true;
      if (succeeded) {
        await _deleteFiles(file);
      } else {
        await _writeDraft(file, draft!.copyWith(clearRequestId: true));
      }
    });
    return resolved;
  }

  Future<void> _writeDraft(File file, MessageDraft draft) async {
    final temporary = File('${file.path}.tmp');
    await temporary.writeAsString(jsonEncode(draft.toJson()), flush: true);
    await temporary.rename(file.path);
  }

  Future<void> _deleteFiles(File file) async {
    if (await file.exists()) await file.delete();
    final temporary = File('${file.path}.tmp');
    if (await temporary.exists()) await temporary.delete();
  }

  Future<void> _enqueue(String sessionId, Future<void> Function() operation) {
    final previous = _operations[sessionId] ?? Future<void>.value();
    final next = previous.catchError((_) {}).then((_) => operation());
    _operations[sessionId] = next;
    return next.whenComplete(() {
      if (identical(_operations[sessionId], next)) {
        _operations.remove(sessionId);
      }
    });
  }
}
