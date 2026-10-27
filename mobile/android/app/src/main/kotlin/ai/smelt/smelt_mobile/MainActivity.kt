package ai.smelt.smelt_mobile

import android.content.Intent
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine

class MainActivity : FlutterActivity() {
    private var appUpdate: AppUpdateChannel? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        appUpdate = AppUpdateChannel(this, flutterEngine.dartExecutor.binaryMessenger)
    }

    /** 应用内更新的安装结果由系统经 PendingIntent 送回这里（见 [AppUpdateChannel]）。 */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        appUpdate?.onNewIntent(intent)
    }
}
