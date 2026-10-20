import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:path_provider/path_provider.dart';

import 'app.dart';
import 'data/backend.dart';
import 'data/demo_backend.dart';
import 'data/preferences.dart';
import 'data/stores.dart';
import 'migration/legacy_import.dart';
import 'rust_lib.dart';
import 'src/rust/api/identity.dart' show initClient;
import 'ui/agent_glyph.dart';

/// `--dart-define=SMELT_DEMO=true`：用合成数据启动，不加载 Rust、不联网。截图审查用。
const _demo = bool.fromEnvironment('SMELT_DEMO');

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  LicenseRegistry.addLicense(() async* {
    final text = await rootBundle.loadString(
      'assets/fonts/jetbrains-mono/OFL.txt',
    );
    yield LicenseEntryWithLineBreaks(['JetBrains Mono'], text);
  });
  // 图标名单来自资产清单，读一次缓存住；没读完之前所有 agent 都是兜底图标。
  await loadAgentIcons();

  final Backend backend;
  final PreferencesStore prefs;
  String? deviceId;
  if (_demo) {
    deviceId = 'demo0device0id0synthetic';
    backend = DemoBackend();
    prefs = PreferencesStore.memory();
  } else {
    // 偏好在 runApp 之前读好：第一帧就是对的明暗。
    prefs = await PreferencesStore.load();
    await initRustLib();
    // 设备身份（iroh 密钥）、机器登记、草稿、离线缓存都在 App Support 目录。
    // initClient 读出登记的机器，并在后台把它们一台台连上。
    final dataDir = await getApplicationSupportDirectory();
    deviceId = (await initClient(dataDir: dataDir.path)).endpointId;
    backend = const FrbBackend();
    await importLegacyData(backend: backend, dataDir: dataDir, prefs: prefs);
  }

  final model = AppModel(backend, prefs, deviceId: deviceId)..start();
  final demoScreen = _demo ? await _demoScreen() : null;
  runApp(
    _Lifecycle(
      model: model,
      child: SmeltApp(model: model, demoScreen: demoScreen),
    ),
  );
  if (kDebugMode && _demo) {
    debugPrint('Smelt 演示模式：数据为合成数据，直达 $demoScreen');
  }
}

/// 演示模式直达哪一屏：截图脚本启动前把屏幕名写进 App 数据目录的 `demo-screen`
/// （iOS 上 Dart 读不到 simctl 传的环境变量）。读完即删，下次正常启动不受影响。
Future<String?> _demoScreen() async {
  try {
    final file = File(
      '${(await getApplicationSupportDirectory()).path}/demo-screen',
    );
    if (!await file.exists()) return null;
    final screen = (await file.readAsString()).trim();
    await file.delete();
    return screen.isEmpty ? null : screen;
  } catch (_) {
    return null;
  }
}

/// 回到前台立刻确认每台机器还连着：系统在后台冻结心跳，这段时间网络往往已经换过。
class _Lifecycle extends StatefulWidget {
  const _Lifecycle({required this.model, required this.child});

  final AppModel model;
  final Widget child;

  @override
  State<_Lifecycle> createState() => _LifecycleState();
}

class _LifecycleState extends State<_Lifecycle> with WidgetsBindingObserver {
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      widget.model.backend.verifyConnections();
    }
  }

  @override
  Widget build(BuildContext context) => widget.child;
}
