package ai.smelt.smelt_mobile

import android.app.Activity
import android.app.PendingIntent
import android.content.Intent
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import java.io.File
import java.security.MessageDigest
import java.util.concurrent.Executors

/**
 * 应用内更新的安卓一侧（`lib/data/app_update.dart` 的 `MethodChannelUpdatePlatform`）：
 * 版本号、算 SHA-256、安装权限、用 PackageInstaller 会话把下好的 APK 交给系统。
 *
 * 安装结果由系统经 PendingIntent 送回 [MainActivity]（`singleTop`，走 `onNewIntent`）：
 * 要用户确认就弹系统的确认框；没装上告诉 Dart。装上了进程会被换掉，不用回报。
 */
class AppUpdateChannel(private val activity: Activity, messenger: BinaryMessenger) :
    MethodChannel.MethodCallHandler {
    private val channel = MethodChannel(messenger, CHANNEL).also { it.setMethodCallHandler(this) }

    /** 算校验和、写安装会话都是几十 MB 的 IO，不放主线程。 */
    private val io = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())

    override fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "currentVersion" -> result.success(versionName())
            "canInstall" -> result.success(canInstall())
            "openInstallSettings" -> {
                openInstallSettings()
                result.success(null)
            }
            "sha256" -> background(result) { sha256(File(call.path())) }
            "install" -> background(result) {
                install(File(call.path()))
                null
            }
            else -> result.notImplemented()
        }
    }

    /** 系统送回的安装结果。不是我们的就不管。 */
    fun onNewIntent(intent: Intent) {
        if (intent.action != ACTION_INSTALL_STATUS) return
        when (val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                val confirm =
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                        intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                    } else {
                        @Suppress("DEPRECATION")
                        intent.getParcelableExtra(Intent.EXTRA_INTENT)
                    }
                if (confirm != null) {
                    activity.startActivity(confirm)
                } else {
                    reportFailure(status, "系统没给确认界面")
                }
            }
            // 装上了：进程马上会被新版换掉。
            PackageInstaller.STATUS_SUCCESS -> {}
            else -> reportFailure(status, intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE))
        }
    }

    private fun reportFailure(status: Int, message: String?) {
        channel.invokeMethod(
            "installFailed",
            mapOf(
                "status" to status,
                "message" to message,
                "cancelled" to (status == PackageInstaller.STATUS_FAILURE_ABORTED),
            ),
        )
    }

    private fun MethodCall.path(): String =
        argument<String>("path") ?: throw IllegalArgumentException("缺少 path")

    private fun background(result: MethodChannel.Result, work: () -> Any?) {
        io.execute {
            try {
                val value = work()
                main.post { result.success(value) }
            } catch (error: Exception) {
                main.post { result.error("APP_UPDATE", error.message, null) }
            }
        }
    }

    private fun versionName(): String? {
        val info =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                activity.packageManager.getPackageInfo(activity.packageName, PackageManager.PackageInfoFlags.of(0))
            } else {
                @Suppress("DEPRECATION")
                activity.packageManager.getPackageInfo(activity.packageName, 0)
            }
        return info.versionName
    }

    /** Android 8 起「安装未知应用」按应用单独授权；更早是系统级开关，交给系统安装界面去拦。 */
    private fun canInstall(): Boolean =
        Build.VERSION.SDK_INT < Build.VERSION_CODES.O || activity.packageManager.canRequestPackageInstalls()

    private fun openInstallSettings() {
        val intent =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}"))
            } else {
                Intent(Settings.ACTION_SECURITY_SETTINGS)
            }
        activity.startActivity(intent)
    }

    private fun sha256(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { input ->
            val buffer = ByteArray(1 shl 20)
            while (true) {
                val read = input.read(buffer)
                if (read < 0) break
                digest.update(buffer, 0, read)
            }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    /** 写进一个安装会话再提交。结果走 [onNewIntent]。 */
    private fun install(apk: File) {
        val installer = activity.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        params.setAppPackageName(activity.packageName)
        // Android 12 起，应用更新自己时系统可以不弹确认；条件不满足系统照样会弹。
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            params.setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
        }
        val sessionId = installer.createSession(params)
        try {
            installer.openSession(sessionId).use { session ->
                apk.inputStream().use { input ->
                    session.openWrite("base.apk", 0, apk.length()).use { output ->
                        input.copyTo(output)
                        session.fsync(output)
                    }
                }
                val intent = Intent(activity, activity.javaClass).setAction(ACTION_INSTALL_STATUS)
                // 系统要往里填结果，Android 12 起必须是可变的。
                val mutable = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0
                val pending =
                    PendingIntent.getActivity(activity, sessionId, intent, PendingIntent.FLAG_UPDATE_CURRENT or mutable)
                session.commit(pending.intentSender)
            }
        } catch (error: Exception) {
            installer.abandonSession(sessionId)
            throw error
        }
    }

    companion object {
        private const val CHANNEL = "smelt/app_update"
        private const val ACTION_INSTALL_STATUS = "ai.smelt.mobile.INSTALL_STATUS"
    }
}
