package dev.veylo.dx

import android.app.Activity
import android.app.PictureInPictureParams
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.graphics.Color
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.media.MediaMetadata
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Build
import android.provider.Settings
import android.util.Rational
import android.view.Display
import android.view.Gravity
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.webkit.WebView
import android.widget.FrameLayout
import java.io.File

/**
 * The picture of Veylo's Rust player (src/engine.rs) on Android: a SurfaceView under the
 * transparent Dioxus WebView, so the same Dioxus controls sit over the video as on desktop.
 * Also wires Android AudioFocus, headphone/Bluetooth disconnect pause, MediaSession transport
 * controls, display frame-rate matching, brightness/volume helpers, rotation lock, and PiP.
 */
object VeyloVideo {
    private var hostActivity: java.lang.ref.WeakReference<Activity>? = null
    private var frame: FrameLayout? = null
    private var surface: SurfaceView? = null
    private var surfaceCallback: SurfaceHolder.Callback? = null
    private var aspect = 0.0
    private var fit = 0
    private var pendingFps = 0f
    private var audioManager: AudioManager? = null
    private var focusRequest: AudioFocusRequest? = null
    @Volatile private var pausedByFocus = false
    private var noisyReceiver: BroadcastReceiver? = null
    @Volatile private var mediaSession: MediaSession? = null
    private val visibilityGen = java.util.concurrent.atomic.AtomicLong(0)

    /** Implemented in Rust (src/android.rs). A null surface means it is going away. */
    @JvmStatic external fun nativeSurface(surface: Surface?, width: Int, height: Int)

    /** Implemented in Rust (src/android.rs). Commands from AudioFocus, noisy audio, or MediaSession. */
    @JvmStatic external fun nativeCommand(cmd: String)

    /** mpv-android's CA bundle (assets/cacert.pem), copied atomically once into the app's files. */
    @JvmStatic
    fun prepare(activity: Activity): String {
        val file = File(activity.filesDir, "cacert.pem")
        if (!file.isFile || file.length() == 0L) {
            runCatching {
                val tmp = File(activity.filesDir, "cacert.pem.tmp")
                activity.assets.open("cacert.pem").use { input -> tmp.outputStream().use { input.copyTo(it) } }
                if (tmp.length() > 0L) {
                    if (file.exists()) file.delete()
                    tmp.renameTo(file)
                } else {
                    tmp.delete()
                }
            }
        }
        return file.path
    }

    /** Bit 1 HDR10, bit 2 HLG, bit 4 HDR10+, bit 8 Dolby Vision: what the panel declares. */
    @JvmStatic
    @Suppress("DEPRECATION")
    fun hdrTypes(activity: Activity): Int = runCatching {
        val display = if (Build.VERSION.SDK_INT >= 30) activity.display else activity.windowManager.defaultDisplay
        val types = display?.hdrCapabilities?.supportedHdrTypes ?: intArrayOf()
        var mask = 0
        if (Display.HdrCapabilities.HDR_TYPE_HDR10 in types) mask = mask or 1
        if (Display.HdrCapabilities.HDR_TYPE_HLG in types) mask = mask or 2
        if (Display.HdrCapabilities.HDR_TYPE_HDR10_PLUS in types) mask = mask or 4
        if (Display.HdrCapabilities.HDR_TYPE_DOLBY_VISION in types) mask = mask or 8
        mask
    }.getOrDefault(0)

    @JvmStatic
    fun show(activity: Activity) {
        val gen = visibilityGen.incrementAndGet()
        activity.runOnUiThread {
            if (visibilityGen.get() != gen) return@runOnUiThread
            // If Android recreated the Activity in the same process, discard stale views bound to the old one.
            if (frame != null && hostActivity?.get() !== activity) {
                stopMediaControls(activity.applicationContext)
                // Always detach mpv from the old Surface before removing callbacks so Rust never keeps a dead wid.
                nativeSurface(null, 0, 0)
                surfaceCallback?.let { cb -> surface?.holder?.removeCallback(cb) }
                surfaceCallback = null
                frame = null; surface = null; hostActivity = null
            }
            if (frame != null) return@runOnUiThread
            val web = findWebView(activity.window.decorView) ?: return@runOnUiThread
            val parent = web.parent as? ViewGroup ?: return@runOnUiThread
            val view = SurfaceView(activity)
            val cb = object : SurfaceHolder.Callback {
                override fun surfaceCreated(holder: SurfaceHolder) {
                    if (surface !== view) return
                    val r = holder.surfaceFrame
                    applyFrameRate(holder.surface, pendingFps)
                    nativeSurface(holder.surface, r.width(), r.height())
                }
                override fun surfaceChanged(holder: SurfaceHolder, format: Int, w: Int, h: Int) {
                    if (surface !== view) return
                    nativeSurface(holder.surface, w, h)
                }
                override fun surfaceDestroyed(holder: SurfaceHolder) {
                    if (surface !== view) return
                    nativeSurface(null, 0, 0)
                }
            }
            view.holder.addCallback(cb)
            val box = FrameLayout(activity).apply { setBackgroundColor(Color.BLACK) }
            box.addView(view, FrameLayout.LayoutParams(-1, -1, Gravity.CENTER))
            box.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> relayout() }
            parent.addView(box, parent.indexOfChild(web), ViewGroup.LayoutParams(-1, -1))
            web.setBackgroundColor(Color.TRANSPARENT)
            hostActivity = java.lang.ref.WeakReference(activity)
            frame = box; surface = view; surfaceCallback = cb
            activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            immersive(activity, true)
            startMediaControls(activity)
        }
    }

    @JvmStatic
    fun hide(activity: Activity) {
        val gen = visibilityGen.incrementAndGet()
        activity.runOnUiThread {
            // A newer show() (source/episode switch) already took ownership: do not tear it down.
            if (visibilityGen.get() != gen) return@runOnUiThread
            stopMediaControls(activity.applicationContext)
            // Restore system brightness & orientation when leaving the player.
            runCatching {
                val lp = activity.window.attributes
                if (lp.screenBrightness >= 0f) {
                    lp.screenBrightness = WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE
                    activity.window.attributes = lp
                }
                activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
            }
            val box = frame ?: return@runOnUiThread
            // Detach mpv synchronously before removing callbacks/view so Rust's SURFACE never retains a destroyed Surface.
            nativeSurface(null, 0, 0)
            surfaceCallback?.let { cb -> surface?.holder?.removeCallback(cb) }
            surfaceCallback = null
            (box.parent as? ViewGroup)?.removeView(box)
            frame = null; surface = null; hostActivity = null; aspect = 0.0; fit = 0; pendingFps = 0f
            findWebView(activity.window.decorView)?.setBackgroundColor(Color.BLACK)
            activity.window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            immersive(activity, false)
        }
    }

    /** `fit`: 0 the whole picture, 1 fill and crop, 2 stretch. */
    @JvmStatic
    fun layout(activity: Activity, aspect: Double, fit: Int) {
        activity.runOnUiThread { this.aspect = aspect; this.fit = fit; relayout() }
    }

    /** Hint the video's frame rate (e.g. 23.976 Hz) to the display surface (Android 11+). */
    @JvmStatic
    fun setFrameRate(activity: Activity, fps: Double) {
        activity.runOnUiThread {
            pendingFps = fps.toFloat()
            surface?.holder?.surface?.let { if (it.isValid) applyFrameRate(it, pendingFps) }
        }
    }

    private fun applyFrameRate(s: Surface, fps: Float) {
        if (fps <= 0f) return
        runCatching {
            if (Build.VERSION.SDK_INT >= 31) {
                s.setFrameRate(fps, Surface.FRAME_RATE_COMPATIBILITY_FIXED_SOURCE, Surface.CHANGE_FRAME_RATE_ONLY_IF_SEAMLESS)
            } else if (Build.VERSION.SDK_INT >= 30) {
                s.setFrameRate(fps, Surface.FRAME_RATE_COMPATIBILITY_FIXED_SOURCE)
            }
        }
    }

    @JvmStatic
    fun setMediaMetadata(activity: Activity, title: String, subtitle: String) {
        activity.runOnUiThread {
            runCatching {
                val meta = MediaMetadata.Builder()
                    .putString(MediaMetadata.METADATA_KEY_TITLE, title)
                    .putString(MediaMetadata.METADATA_KEY_DISPLAY_SUBTITLE, subtitle)
                    .build()
                mediaSession?.setMetadata(meta)
            }
        }
    }

    @JvmStatic
    fun setMediaState(playing: Boolean, positionMs: Long, speed: Float) {
        runCatching {
            val actions = PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or PlaybackState.ACTION_PLAY_PAUSE or
                PlaybackState.ACTION_FAST_FORWARD or PlaybackState.ACTION_REWIND or PlaybackState.ACTION_SKIP_TO_NEXT or PlaybackState.ACTION_STOP
            val st = if (playing) PlaybackState.STATE_PLAYING else PlaybackState.STATE_PAUSED
            val pb = PlaybackState.Builder().setActions(actions).setState(st, positionMs.coerceAtLeast(0L), if (playing) speed else 0f).build()
            mediaSession?.setPlaybackState(pb)
        }
    }

    @JvmStatic
    fun getBrightness(activity: Activity): Float = runCatching {
        val current = activity.window.attributes.screenBrightness
        if (current >= 0f) current.coerceIn(0.05f, 1f)
        else (Settings.System.getInt(activity.contentResolver, Settings.System.SCREEN_BRIGHTNESS, 128) / 255f).coerceIn(0.05f, 1f)
    }.getOrDefault(0.5f)

    @JvmStatic
    fun setBrightness(activity: Activity, level: Float) {
        activity.runOnUiThread {
            runCatching {
                val lp = activity.window.attributes
                lp.screenBrightness = level.coerceIn(0.05f, 1f)
                activity.window.attributes = lp
            }
        }
    }

    @JvmStatic
    fun getVolume(activity: Activity): Float = runCatching {
        val am = activity.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val max = am.getStreamMaxVolume(AudioManager.STREAM_MUSIC).coerceAtLeast(1)
        (am.getStreamVolume(AudioManager.STREAM_MUSIC).toFloat() / max).coerceIn(0f, 1f)
    }.getOrDefault(0.5f)

    @JvmStatic
    fun setVolume(activity: Activity, level: Float) {
        activity.runOnUiThread {
            runCatching {
                val am = activity.getSystemService(Context.AUDIO_SERVICE) as AudioManager
                val max = am.getStreamMaxVolume(AudioManager.STREAM_MUSIC).coerceAtLeast(1)
                val target = (level.coerceIn(0f, 1f) * max).toInt().coerceIn(0, max)
                am.setStreamVolume(AudioManager.STREAM_MUSIC, target, 0)
            }
        }
    }

    @JvmStatic
    fun setRotationLock(activity: Activity, locked: Boolean) {
        activity.runOnUiThread {
            runCatching {
                activity.requestedOrientation = if (locked) ActivityInfo.SCREEN_ORIENTATION_LOCKED else ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
            }
        }
    }

    @JvmStatic
    fun enterPip(activity: Activity, aspectW: Int, aspectH: Int): Boolean {
        if (Build.VERSION.SDK_INT < 26) return false
        if (!activity.packageManager.hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE)) return false
        activity.runOnUiThread {
            runCatching {
                val w = if (aspectW > 0 && aspectH > 0) aspectW else 16
                val h = if (aspectW > 0 && aspectH > 0) aspectH else 9
                // Android requires PiP aspect ratio within [1/2.39, 2.39/1].
                val ratio = (w.toDouble() / h.toDouble()).coerceIn(0.45, 2.35)
                val rational = Rational((ratio * 100).toInt(), 100)
                val params = PictureInPictureParams.Builder().setAspectRatio(rational).build()
                activity.enterPictureInPictureMode(params)
            }
        }
        return true
    }

    private fun startMediaControls(activity: Activity) {
        val appCtx = activity.applicationContext
        runCatching {
            val am = appCtx.getSystemService(Context.AUDIO_SERVICE) as? AudioManager
            if (Build.VERSION.SDK_INT >= 26) {
                focusRequest?.let { prev -> audioManager?.abandonAudioFocusRequest(prev) }
                focusRequest = null
            }
            audioManager = am
            pausedByFocus = false
            val listener = AudioManager.OnAudioFocusChangeListener { change ->
                when (change) {
                    AudioManager.AUDIOFOCUS_LOSS -> {
                        // Permanent focus loss (another media app took over): pause and do not auto-resume.
                        pausedByFocus = false
                        nativeCommand("pause")
                    }
                    AudioManager.AUDIOFOCUS_LOSS_TRANSIENT,
                    AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> {
                        val playing = mediaSession?.controller?.playbackState?.state == PlaybackState.STATE_PLAYING
                        if (playing) {
                            pausedByFocus = true
                            nativeCommand("pause")
                        }
                    }
                    AudioManager.AUDIOFOCUS_GAIN -> {
                        if (pausedByFocus) {
                            pausedByFocus = false
                            nativeCommand("play")
                        }
                    }
                }
            }
            if (am != null && Build.VERSION.SDK_INT >= 26) {
                val attrs = AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
                    .build()
                val req = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                    .setAudioAttributes(attrs)
                    .setOnAudioFocusChangeListener(listener)
                    .build()
                focusRequest = req
                am.requestAudioFocus(req)
            }
        }
        runCatching {
            if (noisyReceiver == null) {
                val r = object : BroadcastReceiver() {
                    override fun onReceive(context: Context, intent: Intent) {
                        if (intent.action == AudioManager.ACTION_AUDIO_BECOMING_NOISY) {
                            nativeCommand("pause")
                        }
                    }
                }
                val filter = IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY)
                if (Build.VERSION.SDK_INT >= 33) appCtx.registerReceiver(r, filter, Context.RECEIVER_NOT_EXPORTED)
                else appCtx.registerReceiver(r, filter)
                noisyReceiver = r
            }
        }
        runCatching {
            if (mediaSession == null) {
                val session = MediaSession(appCtx, "Veylo")
                session.setCallback(object : MediaSession.Callback() {
                    override fun onPlay() { pausedByFocus = false; nativeCommand("play") }
                    override fun onPause() { pausedByFocus = false; nativeCommand("pause") }
                    override fun onSkipToNext() = nativeCommand("next")
                    override fun onFastForward() = nativeCommand("seek_fwd")
                    override fun onRewind() = nativeCommand("seek_back")
                    override fun onStop() { pausedByFocus = false; nativeCommand("stop") }
                })
                session.isActive = true
                mediaSession = session
                setMediaState(true, 0L, 1.0f)
            }
        }
    }

    private fun stopMediaControls(appCtx: Context) {
        val r = noisyReceiver
        noisyReceiver = null
        if (r != null) runCatching { appCtx.unregisterReceiver(r) }
        val req = focusRequest
        focusRequest = null
        pausedByFocus = false
        if (req != null && Build.VERSION.SDK_INT >= 26) {
            runCatching { audioManager?.abandonAudioFocusRequest(req) }
        }
        val session = mediaSession
        mediaSession = null
        if (session != null) {
            runCatching {
                session.isActive = false
                session.release()
            }
        }
    }

    private fun relayout() {
        val box = frame ?: return
        val view = surface ?: return
        val (bw, bh) = box.width to box.height
        val lp = view.layoutParams as FrameLayout.LayoutParams
        var (w, h) = -1 to -1
        if (aspect > 0 && bw > 0 && bh > 0 && fit != 2) {
            val wide = bw.toDouble() / bh > aspect
            // Fit: the limiting side fills; Fill: the other side does, and the overflow is cropped.
            if (wide == (fit == 0)) { h = bh; w = (bh * aspect).toInt() } else { w = bw; h = (bw / aspect).toInt() }
        }
        if (lp.width != w || lp.height != h) { lp.width = w; lp.height = h; lp.gravity = Gravity.CENTER; view.layoutParams = lp }
    }

    @Suppress("DEPRECATION")
    private fun immersive(activity: Activity, on: Boolean) {
        if (Build.VERSION.SDK_INT >= 30) {
            activity.window.insetsController?.let {
                if (on) { it.hide(WindowInsets.Type.systemBars()); it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE }
                else it.show(WindowInsets.Type.systemBars())
            }
        } else {
            activity.window.decorView.systemUiVisibility = if (on) {
                View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY or
                    View.SYSTEM_UI_FLAG_FULLSCREEN or
                    View.SYSTEM_UI_FLAG_HIDE_NAVIGATION or
                    View.SYSTEM_UI_FLAG_LAYOUT_STABLE or
                    View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION or
                    View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
            } else {
                View.SYSTEM_UI_FLAG_VISIBLE
            }
        }
    }

    private fun findWebView(view: View): WebView? {
        if (view is WebView) return view
        if (view is ViewGroup) for (i in 0 until view.childCount) findWebView(view.getChildAt(i))?.let { return it }
        return null
    }
}
