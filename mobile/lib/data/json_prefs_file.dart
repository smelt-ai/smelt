import 'dart:convert';
import 'dart:io';

import 'package:path_provider/path_provider.dart';

/// 本地显示偏好的落盘细节：宽容读、串行写。
///
/// 抽出来是因为「终端字号」和「主题」是两类互不相干的偏好，各自有各自的文件和
/// 默认值，但读写规则完全一样。再来第三类偏好时只要给一组 `decode/encode/
/// fallback`，不必第三次抄同一段容错逻辑。
///
/// 注意这里承载的都是**显示偏好**，不是凭据：换一台桌面、换一次配对码都不该把
/// 用户调好的设置清掉，所以走普通文件而不是 `flutter_secure_storage`。
class JsonPrefsFile<T> {
  JsonPrefsFile({
    required this.fileName,
    required this.decode,
    required this.encode,
    required this.fallback,
    Future<Directory> Function()? directoryProvider,
  }) : _directoryProvider = directoryProvider ?? getApplicationSupportDirectory;

  final String fileName;
  final T Function(Map<String, dynamic> json) decode;
  final Map<String, dynamic> Function(T value) encode;

  /// 读不出来时退回的值。整个类的容错策略就一句话：**永远给得出一个值**。
  final T fallback;

  final Future<Directory> Function() _directoryProvider;
  Future<void> _pending = Future<void>.value();

  Future<File> _file() async {
    final root = await _directoryProvider();
    return File('${root.path}/$fileName');
  }

  Future<T> load() async {
    // 等排队中的写落盘，否则刚存的值可能读不到。
    await _pending.catchError((_) {});
    try {
      final file = await _file();
      if (!await file.exists()) return fallback;
      final value = jsonDecode(await file.readAsString());
      if (value is! Map<String, dynamic>) return fallback;
      return decode(value);
    } catch (_) {
      // 刻意宽 catch：损坏的 json、拿不到目录（测试环境没有 path_provider 的
      // 平台通道）都不该让功能打不开——显示偏好读不到就用默认值。
      return fallback;
    }
  }

  Future<void> save(T value) {
    // 串行化：连点几下不该产生交错的半截写入。
    final next = _pending.catchError((_) {}).then((_) async {
      final file = await _file();
      await file.parent.create(recursive: true);
      await file.writeAsString(jsonEncode(encode(value)), flush: true);
    });
    _pending = next.catchError((_) {});
    return next;
  }
}
