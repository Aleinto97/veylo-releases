package dev.veylo.dx

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ApplicationInfo
import android.content.pm.PackageInstaller
import android.net.Uri
import android.os.Build
import android.provider.Settings
import java.io.File
import java.security.SecureRandom
import java.util.Base64

/**
 * Installs a downloaded Veylo update (src/updater.rs) through PackageInstaller, which shows
 * Android's own "Update this app?" confirmation. The APK must be signed with the same key as
 * the installed app, so the library and settings are kept.
 */
object VeyloUpdate {
    private const val ACTION = "dev.veylo.dx.UPDATE_STATUS"
    private const val EXTRA_NONCE = "dev.veylo.dx.UPDATE_NONCE"
    private val rng = SecureRandom()
    private var receiver: BroadcastReceiver? = null
    private var receiverContext: Context? = null
    private var activeSessionId: Int = -1
    private var activeNonce: String? = null

    /** "installing", or "permission" when Veylo first needs "Install unknown apps" (Settings opened). */
    @JvmStatic
    fun install(activity: Activity, path: String): String {
        if (!activity.packageManager.canRequestPackageInstalls()) {
            activity.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:" + activity.packageName)))
            return "permission"
        }
        val file = File(path)
        val installer = activity.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        params.setAppPackageName(activity.packageName)
        val id = installer.createSession(params)
        val nonceBytes = ByteArray(18)
        rng.nextBytes(nonceBytes)
        val nonce = Base64.getUrlEncoder().withoutPadding().encodeToString(nonceBytes)
        activeSessionId = id
        activeNonce = nonce
        listen(activity.applicationContext)
        installer.openSession(id).use { session ->
            file.inputStream().use { input ->
                session.openWrite("veylo.apk", 0, file.length()).use { out ->
                    input.copyTo(out)
                    session.fsync(out)
                }
            }
            val flags = PendingIntent.FLAG_UPDATE_CURRENT or
                (if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0)
            val callback = Intent(ACTION)
                .setPackage(activity.packageName)
                .putExtra(EXTRA_NONCE, nonce)
            val status = PendingIntent.getBroadcast(activity, id, callback, flags)
            session.commit(status.intentSender)
        }
        return "installing"
    }

    private fun stopListening() {
        val r = receiver
        val ctx = receiverContext
        receiver = null
        receiverContext = null
        activeSessionId = -1
        activeNonce = null
        if (r != null && ctx != null) {
            try { ctx.unregisterReceiver(r) } catch (_: Exception) {}
        }
    }

    /** The installer reports back here; "pending user action" carries the confirmation screen. */
    private fun listen(appContext: Context) {
        if (receiver != null) return
        val r = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if (intent.action != ACTION) return
                val expectedNonce = activeNonce ?: return
                val sessionId = intent.getIntExtra(PackageInstaller.EXTRA_SESSION_ID, -1)
                if (sessionId != activeSessionId || intent.getStringExtra(EXTRA_NONCE) != expectedNonce) return
                val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
                if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
                    val confirm: Intent = if (Build.VERSION.SDK_INT >= 33) {
                        intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                    } else {
                        @Suppress("DEPRECATION")
                        intent.getParcelableExtra(Intent.EXTRA_INTENT)
                    } ?: return
                    val resolved = confirm.resolveActivityInfo(context.packageManager, 0) ?: return
                    val isSystem = (resolved.applicationInfo.flags and ApplicationInfo.FLAG_SYSTEM) != 0
                    if (!isSystem) return
                    confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    context.startActivity(confirm)
                } else {
                    stopListening()
                }
            }
        }
        if (Build.VERSION.SDK_INT >= 33) appContext.registerReceiver(r, IntentFilter(ACTION), Context.RECEIVER_NOT_EXPORTED)
        else appContext.registerReceiver(r, IntentFilter(ACTION))
        receiver = r
        receiverContext = appContext
    }
}
