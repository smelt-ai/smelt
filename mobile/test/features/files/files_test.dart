import 'package:flutter/material.dart' hide ConnectionState;
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/features/conversation/conversation_markdown.dart';
import 'package:smelt_mobile/features/files/file_links.dart';
import 'package:smelt_mobile/features/files/file_preview_page.dart';
import 'package:smelt_mobile/features/files/project_files_page.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/theme/theme.dart';

const _root = '/Users/demo/code/smelt';

/// 演示后端，记下文件相关的调用；打开的文本记下是否被释放。
class FilesBackend extends DemoBackend {
  FilesBackend() : super(now: DateTime(2025, 6, 1, 14));

  final listed = <String>[];
  final searched = <String>[];
  final opened = <String>[];
  final texts = <_TrackedText>[];

  @override
  Future<List<FileEntry>> listProjectDir(
    String runtimeId, {
    required String root,
    required String dir,
  }) {
    listed.add(dir);
    return super.listProjectDir(runtimeId, root: root, dir: dir);
  }

  @override
  Future<FileSearch> searchProject(
    String runtimeId, {
    required String root,
    required String query,
  }) {
    searched.add(query);
    return super.searchProject(runtimeId, root: root, query: query);
  }

  @override
  Future<OpenedFile> openFile(String runtimeId, String path) async {
    opened.add(path);
    final file = await super.openFile(runtimeId, path);
    if (file case OpenedText(:final text)) {
      final tracked = _TrackedText(text);
      texts.add(tracked);
      return OpenedText(tracked);
    }
    return file;
  }
}

class _TrackedText implements TextSource {
  _TrackedText(this._inner);
  final TextSource _inner;
  bool disposed = false;

  @override
  int get lineCount => _inner.lineCount;
  @override
  int get maxLineChars => _inner.maxLineChars;
  @override
  String? get language => _inner.language;
  @override
  bool get highlighted => _inner.highlighted;
  @override
  Future<List<HighlightedLine>> lines(int start, int end) =>
      _inner.lines(start, end);
  @override
  Future<String> fullText() => _inner.fullText();
  @override
  void dispose() => disposed = true;
}

Future<void> _pump(
  WidgetTester tester,
  FilesBackend backend,
  Widget child,
) async {
  tester.view.physicalSize = const Size(800, 2400);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final model = AppModel(backend, PreferencesStore.memory())..start();
  await tester.pumpWidget(
    AppScope(
      model: model,
      child: MaterialApp(
        locale: const Locale('en'),
        theme: buildGalleyTheme(Brightness.light),
        localizationsDelegates: L10n.localizationsDelegates,
        supportedLocales: L10n.supportedLocales,
        home: child,
      ),
    ),
  );
  await tester.pumpAndSettle();
  addTearDown(model.dispose);
}

/// 可选中的 Markdown 里链接点不到字上，直接走它的点击回调（就是界面点链接时调的那个）。
void _tapLink(WidgetTester tester, String href) {
  final body = tester.widget<MarkdownBody>(find.byType(MarkdownBody));
  body.onTapLink!(href, href, '');
}

/// 对话页的替身：一个按钮进文件浏览。
class _Host extends StatelessWidget {
  const _Host();

  @override
  Widget build(BuildContext context) => Scaffold(
    body: Center(
      child: TextButton(
        onPressed: () => openProjectFiles(
          context,
          runtimeId: 'r1',
          root: _root,
          title: 'smelt',
        ),
        child: const Text('host'),
      ),
    ),
  );
}

Future<void> _enterFiles(WidgetTester tester, FilesBackend backend) async {
  await _pump(tester, backend, const _Host());
  await tester.tap(find.text('host'));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('每层目录一页：返回（含右滑）退回上一层', (tester) async {
    final backend = FilesBackend();
    await _enterFiles(tester, backend);
    expect(find.text('smelt'), findsOneWidget);
    // 根目录不放「关闭」，返回键就是离开。
    expect(find.byTooltip('Close'), findsNothing);
    // 目录在前。
    expect(
      tester.getTopLeft(find.text('src')).dy,
      lessThan(tester.getTopLeft(find.text('README.md')).dy),
    );

    await tester.tap(find.text('src'));
    await tester.pumpAndSettle();
    expect(backend.listed, [_root, '$_root/src']);
    expect(find.text('main.rs'), findsOneWidget);

    // iOS 从屏幕左缘右滑退回上一层。
    await tester.dragFrom(const Offset(4, 600), const Offset(600, 0));
    await tester.pumpAndSettle();
    expect(find.text('README.md'), findsOneWidget);
    expect(find.byType(ProjectFilesPage), findsOneWidget);

    // 返回键同样只退一层；在根目录再退就离开文件浏览。
    await tester.tap(find.text('src'));
    await tester.pumpAndSettle();
    await tester.pageBack();
    await tester.pumpAndSettle();
    expect(find.byType(ProjectFilesPage), findsOneWidget);
    await tester.pageBack();
    await tester.pumpAndSettle();
    expect(find.text('host'), findsOneWidget);
  }, variant: TargetPlatformVariant.only(TargetPlatform.iOS));

  testWidgets('深层目录和预览里「关闭」一次退出整个文件浏览', (tester) async {
    final backend = FilesBackend();
    await _enterFiles(tester, backend);
    await tester.tap(find.text('src'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('main.rs'));
    await tester.pumpAndSettle();
    expect(find.byType(FilePreviewPage), findsOneWidget);

    await tester.tap(find.byTooltip('Close'));
    await tester.pumpAndSettle();
    expect(find.byType(ProjectFilesPage), findsNothing);
    expect(find.byType(FilePreviewPage), findsNothing);
    expect(find.text('host'), findsOneWidget);
  });

  testWidgets('搜索：停手后才发请求，点内容命中带行号打开', (tester) async {
    final backend = FilesBackend();
    await _pump(
      tester,
      backend,
      const ProjectFilesPage(runtimeId: 'r1', root: _root, title: 'smelt'),
    );
    await tester.enterText(find.byType(TextField), 'prin');
    await tester.pump(const Duration(milliseconds: 100));
    await tester.enterText(find.byType(TextField), 'println');
    await tester.pump(const Duration(milliseconds: 400));
    await tester.pumpAndSettle();
    expect(backend.searched, ['println']);

    await tester.tap(find.textContaining('Line 7'));
    await tester.pumpAndSettle();
    expect(backend.opened, ['$_root/src/main.rs']);
    final preview = tester.widget<FilePreviewPage>(
      find.byType(FilePreviewPage),
    );
    expect(preview.line, 7);
    expect(find.text('main.rs'), findsOneWidget);
  });

  testWidgets('文本预览带行号；离开时释放内核里的正文', (tester) async {
    final backend = FilesBackend();
    await _pump(
      tester,
      backend,
      const ProjectFilesPage(runtimeId: 'r1', root: _root, title: 'smelt'),
    );
    await tester.tap(find.text('Cargo.toml'));
    await tester.pumpAndSettle();
    expect(find.text('[package]', findRichText: true), findsOneWidget);
    expect(find.text('4 lines'), findsOneWidget);

    await tester.pageBack();
    await tester.pumpAndSettle();
    expect(backend.texts.single.disposed, isTrue);
  });

  testWidgets('Markdown 默认渲染，能切到源码；里面的相对链接按文件所在目录打开', (tester) async {
    final backend = FilesBackend();
    await _pump(
      tester,
      backend,
      const FilePreviewPage(runtimeId: 'r1', path: '$_root/README.md'),
    );
    expect(find.byType(ConversationMarkdown), findsOneWidget);

    await tester.tap(find.byTooltip('Show source'));
    await tester.pumpAndSettle();
    expect(find.byType(ConversationMarkdown), findsNothing);
    expect(find.text('# Demo project', findRichText: true), findsOneWidget);

    await tester.tap(find.byTooltip('Show preview'));
    await tester.pumpAndSettle();
    _tapLink(tester, 'docs/guide.md');
    await tester.pumpAndSettle();
    expect(backend.opened.last, '$_root/docs/guide.md');
  });

  testWidgets('二进制只给说明', (tester) async {
    await _pump(
      tester,
      FilesBackend(),
      const FilePreviewPage(runtimeId: 'r1', path: '$_root/assets/icon.bin'),
    );
    expect(find.text('Not a text file'), findsOneWidget);
  });

  testWidgets('过大的文件说明大小和上限', (tester) async {
    final backend = FilesBackend();
    await _pump(
      tester,
      backend,
      const FilePreviewPage(runtimeId: 'r1', path: '$_root/logs/build.log'),
    );
    expect(find.text('Too large to preview'), findsOneWidget);
    expect(find.textContaining('12.0 MB'), findsOneWidget);
  });

  testWidgets('对话里的文件链接带行号在 App 里预览', (tester) async {
    final backend = FilesBackend();
    await _pump(
      tester,
      backend,
      const Scaffold(
        body: FileLinkScope(
          runtimeId: 'r1',
          cwd: _root,
          child: ConversationMarkdown(
            data: 'See [main](src/main.rs:5) and [site](https://example.com).',
          ),
        ),
      ),
    );
    _tapLink(tester, 'src/main.rs:5');
    await tester.pumpAndSettle();
    expect(backend.opened, ['$_root/src/main.rs']);
    expect(
      tester.widget<FilePreviewPage>(find.byType(FilePreviewPage)).line,
      5,
    );
  });

  test('文件大小给人看的写法', () {
    expect(formatBytes(512), '512 B');
    expect(formatBytes(12 * 1024 * 1024), '12.0 MB');
  });
}
