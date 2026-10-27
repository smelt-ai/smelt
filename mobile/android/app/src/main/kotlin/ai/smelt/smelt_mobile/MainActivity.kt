package ai.smelt.smelt_mobile

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine

class MainActivity : FlutterActivity() {
    private var appUpdate: AppUpdateChannel? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        appUpdate = AppUpdateChannel(this, flutterEngine.dartExecutor.binaryMessenger).also { it.attach() }
    }

    override fun cleanUpFlutterEngine(flutterEngine: FlutterEngine) {
        appUpdate?.detach()
        appUpdate = null
        super.cleanUpFlutterEngine(flutterEngine)
    }

    // 应用内更新的系统确认框只能在前台弹，见 AppUpdateChannel。
    override fun onResume() {
        super.onResume()
        appUpdate?.resumed()
    }

    override fun onPause() {
        appUpdate?.paused()
        super.onPause()
    }
}
