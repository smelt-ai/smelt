// 把 lib/l10n/src/*_<locale>.arb 合并成 lib/l10n/app_<locale>.arb。
//
// 每个功能一组片段文件，键名以功能名开头（pass…、session…），合并时撞键直接报错。
// 英文是模板：中文片段缺了英文里的键、或多出英文没有的键，也报错。
//
//   dart run tool/merge_arb.dart          合并
//   dart run tool/merge_arb.dart --check  只检查生成物是否最新（CI 用）
import 'dart:convert';
import 'dart:io';

const locales = ['en', 'zh'];

void main(List<String> args) {
  final check = args.contains('--check');
  final src = Directory('lib/l10n/src');
  final merged = <String, Map<String, dynamic>>{};
  for (final locale in locales) {
    final out = <String, dynamic>{'@@locale': locale};
    final files =
        src
            .listSync()
            .whereType<File>()
            .where((f) => f.path.endsWith('_$locale.arb'))
            .toList()
          ..sort((a, b) => a.path.compareTo(b.path));
    for (final file in files) {
      final data = jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
      for (final entry in data.entries) {
        if (entry.key.startsWith('@@')) continue;
        if (out.containsKey(entry.key)) {
          stderr.writeln('重复的键 ${entry.key}（${file.path}）');
          exit(1);
        }
        out[entry.key] = entry.value;
      }
    }
    merged[locale] = out;
  }
  final template = merged['en']!.keys.where((k) => !k.startsWith('@')).toSet();
  for (final locale in locales.where((l) => l != 'en')) {
    final keys = merged[locale]!.keys.where((k) => !k.startsWith('@')).toSet();
    final missing = template.difference(keys);
    final extra = keys.difference(template);
    if (missing.isNotEmpty || extra.isNotEmpty) {
      stderr.writeln('$locale 与 en 不一致：缺 $missing，多 $extra');
      exit(1);
    }
  }
  var stale = false;
  for (final locale in locales) {
    final file = File('lib/l10n/app_$locale.arb');
    final text = '${const JsonEncoder.withIndent('  ').convert(merged[locale])}\n';
    if (file.existsSync() && file.readAsStringSync() == text) continue;
    if (check) {
      stderr.writeln('${file.path} 不是最新，先跑 dart run tool/merge_arb.dart');
      stale = true;
    } else {
      file.writeAsStringSync(text);
    }
  }
  if (stale) exit(1);
}
