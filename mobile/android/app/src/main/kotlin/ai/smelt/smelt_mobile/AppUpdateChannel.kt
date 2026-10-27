package ai.smelt.smelt_mobile

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
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
 * 安装结果由系统经 PendingIntent 广播回来，收在这里注册的、不导出的接收器里。要用户确认
 * 时，Smelt 在前台就马上弹系统确认框，在后台就记着、回到前台再弹（后台不许起界面）。没装上
 * 告诉 Dart；装上了进程会被换掉，不用回报。这些都跟着 [MainActivity] 的生命周期走：
 * [attach] 在创建时、[detach] 在销毁时，[resumed]、[paused] 跟前后台。
 */
class AppUpdateChannel(private val activity: Activity, messenger: BinaryMessenger) :
    MethodChannel.MethodCallHandler {
    private val channel = MethodChannel(messenger, CHANNEL).also { it.setMethodCallHandler(this) }
    private val main = Handler(Looper.getMainLooper())

    /**
     * 交给系统、还没出结果的那个会话。进程没了它也没了，Dart 回到前台时据此判断。在 IO 线程
     * 上提交前写好，结果在主线程上清掉：先写后清，结果来得再快也不会被盖回去。
     */
    @Volatile private var activeSession: Int? = null

    /** 结果回来时 Smelt 在后台，确认框留到回到前台再弹。 */
    private var pendingConfirm: Intent? = null
    private var foreground = false

    private val receiver =
        object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) = onStatus(intent)
        }

    fun attach() {
        val filter = IntentFilter(ACTION_INSTALL_STATUS)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            activity.registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            activity.registerReceiver(receiver, filter)
        }
    }

    fun detach() {
        activity.unregisterReceiver(receiver)
        channel.setMethodCallHandler(null)
    }

    fun resumed() {
        foreground = true
        pendingConfirm?.let {
            pendingConfirm = null
            activity.startActivity(it)
        }
    }

    fun paused() {
        foreground = false
    }

    override fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "currentVersion" -> result.success(versionName())
            "canInstall" -> result.success(canInstall())
            "openInstallSettings" -> {
                openInstallSettings()
                result.success(null)
            }
            "installInProgress" -> result.success(activeSession != null)
            "sha256" -> background(result) { sha256(File(call.path())) }
            "install" -> background(result) {
                install(File(call.path()))
                null
            }
            else -> result.notImplemented()
        }
    }

    private fun onStatus(intent: Intent) {
        when (val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                // 类型化的 getParcelableExtra 在 Android 13 上有 bug，34 起再用。
                val confirm: Intent? =
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                        intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                    } else {
                        @Suppress("DEPRECATION")
                        intent.getParcelableExtra(Intent.EXTRA_INTENT)
                    }
                when {
                    confirm == null -> finish(status, "系统没给确认界面")
                    foreground -> activity.startActivity(confirm)
                    else -> pendingConfirm = confirm
                }
            }
            // 装上了：进程马上会被新版换掉。
            PackageInstaller.STATUS_SUCCESS -> activeSession = null
            else -> finish(status, intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE))
        }
    }

    /** 没装上：用户取消也好、装失败也好，这次会话都结束了。 */
    private fun finish(status: Int, message: String?) {
        activeSession = null
        pendingConfirm = null
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

    /** 写进一个安装会话再提交。结果走 [onStatus]。 */
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
                // 显式指向本应用，只有上面那个不导出的接收器收得到。
                val intent = Intent(ACTION_INSTALL_STATUS).setPackage(activity.packageName)
                // 系统要往里填结果，Android 12 起必须是可变的。
                val mutable = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0
                val pending =
                    PendingIntent.getBroadcast(activity, sessionId, intent, PendingIntent.FLAG_UPDATE_CURRENT or mutable)
                activeSession = sessionId
                session.commit(pending.intentSender)
            }
        } catch (error: Exception) {
            activeSession = null
            installer.abandonSession(sessionId)
            throw error
        }
    }

    companion object {
        private const val CHANNEL = "smelt/app_update"
        private const val ACTION_INSTALL_STATUS = "ai.smelt.mobile.INSTALL_STATUS"

        /** 算校验和、写安装会话都是几十 MB 的 IO，不放主线程。整个进程共用一个。 */
        private val io = Executors.newSingleThreadExecutor()
    }
}
