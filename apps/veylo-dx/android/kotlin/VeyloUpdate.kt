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
    const val ERROR_ACTION = "dev.veylo.dx.UPDATE_ERROR"
    const val EXTRA_MESSAGE = "dev.veylo.dx.UPDATE_ERROR_MESSAGE"
    private val rng = SecureRandom()
    private val lock = Any()
    private var receiver: BroadcastReceiver? = null
    private var receiverContext: Context? = null
    private var activeSessionId: Int = -1
    private var activeNonce: String? = null
    /** Why the last install attempt stopped before Android's confirmation (null: nothing went wrong). */
    @Volatile private var lastError: String? = null

    /** The reason the installer could not continue, once; null when there is none. Never holds a path or URL. */
    @JvmStatic
    fun takeError(): String? {
        val e = lastError
        lastError = null
        return e
    }

    /** Says so (log, [ERROR_ACTION] broadcast for the app, [takeError]) instead of leaving the update silently unfinished. */
    private fun fail(context: Context, sessionId: Int, message: String) {
        lastError = message
        runCatching { android.util.Log.w("veylo", "update: $message") }
        runCatching {
            context.sendBroadcast(Intent(ERROR_ACTION).setPackage(context.packageName).putExtra(EXTRA_MESSAGE, message))
        }
        stopListening(sessionId)
    }

    /** "installing", or "permission" when Veylo first needs "Install unknown apps" (Settings opened). */
    @JvmStatic
    fun install(activity: Activity, path: String): String {
        if (!activity.packageManager.canRequestPackageInstalls()) {
            activity.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:" + activity.packageName)))
            return "permission"
        }
        lastError = null
        val file = File(path)
        val installer = activity.packageManager.packageInstaller
        val prevId = synchronized(lock) { activeSessionId }
        if (prevId != -1) {
            runCatching { installer.abandonSession(prevId) }
            stopListening(prevId)
        }
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        params.setAppPackageName(activity.packageName)
        val id = installer.createSession(params)
        val nonceBytes = ByteArray(18)
        rng.nextBytes(nonceBytes)
        val nonce = Base64.getUrlEncoder().withoutPadding().encodeToString(nonceBytes)
        synchronized(lock) {
            activeSessionId = id
            activeNonce = nonce
            listenLocked(activity.applicationContext)
        }
        try {
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
        } catch (t: Throwable) {
            runCatching { installer.abandonSession(id) }
            stopListening(id)
            throw t
        }
        return "installing"
    }

    private fun stopListening(expectedSessionId: Int) {
        val pair: Pair<BroadcastReceiver, Context>? = synchronized(lock) {
            if (activeSessionId != expectedSessionId) return
            val r = receiver
            val ctx = receiverContext
            receiver = null
            receiverContext = null
            activeSessionId = -1
            activeNonce = null
            if (r != null && ctx != null) Pair(r, ctx) else null
        }
        if (pair != null) {
            try { pair.second.unregisterReceiver(pair.first) } catch (_: Exception) {}
        }
    }

    /** The installer reports back here; "pending user action" carries the confirmation screen. */
    private fun listenLocked(appContext: Context) {
        if (receiver != null) return
        val r = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if (intent.action != ACTION) return
                val sessionId = intent.getIntExtra(PackageInstaller.EXTRA_SESSION_ID, -1)
                val matched = synchronized(lock) {
                    val expectedNonce = activeNonce ?: return@synchronized false
                    sessionId == activeSessionId && intent.getStringExtra(EXTRA_NONCE) == expectedNonce
                }
                if (!matched) return
                val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
                if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
                    val confirm: Intent? = if (Build.VERSION.SDK_INT >= 33) {
                        intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                    } else {
                        @Suppress("DEPRECATION")
                        intent.getParcelableExtra(Intent.EXTRA_INTENT)
                    }
                    if (confirm == null) {
                        fail(context, sessionId, "Android's installer sent no confirmation screen.")
                        return
                    }
                    val resolved = confirm.resolveActivityInfo(context.packageManager, 0)
                    if (resolved == null) {
                        fail(context, sessionId, "The installer's confirmation screen can't be opened on this device.")
                        return
                    }
                    val isSystem = (resolved.applicationInfo.flags and ApplicationInfo.FLAG_SYSTEM) != 0
                    if (!isSystem) {
                        fail(context, sessionId, "The update confirmation did not come from Android's own installer, so it was not opened.")
                        return
                    }
                    confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    try {
                        context.startActivity(confirm)
                    } catch (t: Throwable) {
                        fail(context, sessionId, "The installer's confirmation screen couldn't be shown.")
                    }
                } else {
                    // Success ends quietly; any other status is told once, with the installer's own words.
                    if (status != PackageInstaller.STATUS_SUCCESS) {
                        val why = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)?.takeIf { it.isNotBlank() }
                        fail(context, sessionId, "The update wasn't installed" + (why?.let { ": $it" } ?: "."))
                    } else {
                        stopListening(sessionId)
                    }
                }
            }
        }
        if (Build.VERSION.SDK_INT >= 33) appContext.registerReceiver(r, IntentFilter(ACTION), Context.RECEIVER_NOT_EXPORTED)
        else appContext.registerReceiver(r, IntentFilter(ACTION))
        receiver = r
        receiverContext = appContext
    }
}
