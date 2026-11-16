package dev.veylo.dx

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageInstaller
import android.net.Uri
import android.os.Build
import android.provider.Settings
import java.io.File

/**
 * Installs a downloaded Veylo update (src/updater.rs) through PackageInstaller, which shows
 * Android's own "Update this app?" confirmation. The APK must be signed with the same key as
 * the installed app, so the library and settings are kept.
 */
object VeyloUpdate {
    private const val ACTION = "dev.veylo.dx.UPDATE_STATUS"
    private var receiver: BroadcastReceiver? = null

    /** "installing", or "permission" when Veylo first needs "Install unknown apps" (Settings opened). */
    @JvmStatic
    fun install(activity: Activity, path: String): String {
        if (!activity.packageManager.canRequestPackageInstalls()) {
            activity.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:" + activity.packageName)))
            return "permission"
        }
        listen(activity)
        val file = File(path)
        val installer = activity.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        params.setAppPackageName(activity.packageName)
        val id = installer.createSession(params)
        installer.openSession(id).use { session ->
            file.inputStream().use { input ->
                session.openWrite("veylo.apk", 0, file.length()).use { out ->
                    input.copyTo(out)
                    session.fsync(out)
                }
            }
            val flags = PendingIntent.FLAG_UPDATE_CURRENT or
                (if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0)
            val status = PendingIntent.getBroadcast(activity, id, Intent(ACTION).setPackage(activity.packageName), flags)
            session.commit(status.intentSender)
        }
        return "installing"
    }

    /** The installer reports back here; "pending user action" carries the confirmation screen. */
    private fun listen(activity: Activity) {
        if (receiver != null) return
        val r = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
                if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
                    @Suppress("DEPRECATION")
                    val confirm = intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT) ?: return
                    confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    context.startActivity(confirm)
                }
            }
        }
        if (Build.VERSION.SDK_INT >= 33) activity.registerReceiver(r, IntentFilter(ACTION), Context.RECEIVER_NOT_EXPORTED)
        else activity.registerReceiver(r, IntentFilter(ACTION))
        receiver = r
    }
}
