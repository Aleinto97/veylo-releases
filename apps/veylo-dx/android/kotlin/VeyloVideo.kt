package dev.veylo.dx

import android.app.Activity
import android.graphics.Color
import android.os.Build
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
 * mpv is driven from Rust; this only places the surface, sizes it to the picture and reports
 * its lifetime to Rust ([nativeSurface], called synchronously so mpv lets go before it dies).
 * With mpv's direct output MediaCodec decodes into this surface, and HDR10/HLG/HDR10+ reach
 * the panel as HDR.
 */
object VeyloVideo {
    private var frame: FrameLayout? = null
    private var surface: SurfaceView? = null
    private var aspect = 0.0
    private var fit = 0

    /** Implemented in Rust (src/android.rs). A null surface means it is going away. */
    @JvmStatic external fun nativeSurface(surface: Surface?, width: Int, height: Int)

    /** mpv-android's CA bundle (assets/cacert.pem), copied once into the app's files. */
    @JvmStatic
    fun prepare(activity: Activity): String {
        val file = File(activity.filesDir, "cacert.pem")
        if (!file.isFile) runCatching { activity.assets.open("cacert.pem").use { input -> file.outputStream().use { input.copyTo(it) } } }
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
        activity.runOnUiThread {
            if (frame != null) return@runOnUiThread
            val web = findWebView(activity.window.decorView) ?: return@runOnUiThread
            val parent = web.parent as? ViewGroup ?: return@runOnUiThread
            val view = SurfaceView(activity)
            view.holder.addCallback(object : SurfaceHolder.Callback {
                override fun surfaceCreated(holder: SurfaceHolder) {
                    val r = holder.surfaceFrame
                    nativeSurface(holder.surface, r.width(), r.height())
                }
                override fun surfaceChanged(holder: SurfaceHolder, format: Int, w: Int, h: Int) = nativeSurface(holder.surface, w, h)
                override fun surfaceDestroyed(holder: SurfaceHolder) = nativeSurface(null, 0, 0)
            })
            val box = FrameLayout(activity).apply { setBackgroundColor(Color.BLACK) }
            box.addView(view, FrameLayout.LayoutParams(-1, -1, Gravity.CENTER))
            box.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> relayout() }
            parent.addView(box, parent.indexOfChild(web), ViewGroup.LayoutParams(-1, -1))
            web.setBackgroundColor(Color.TRANSPARENT)
            frame = box; surface = view
            activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            immersive(activity, true)
        }
    }

    @JvmStatic
    fun hide(activity: Activity) {
        activity.runOnUiThread {
            val box = frame ?: return@runOnUiThread
            (box.parent as? ViewGroup)?.removeView(box)
            frame = null; surface = null; aspect = 0.0; fit = 0
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

    private fun immersive(activity: Activity, on: Boolean) {
        if (Build.VERSION.SDK_INT < 30) return
        activity.window.insetsController?.let {
            if (on) { it.hide(WindowInsets.Type.systemBars()); it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE }
            else it.show(WindowInsets.Type.systemBars())
        }
    }

    private fun findWebView(view: View): WebView? {
        if (view is WebView) return view
        if (view is ViewGroup) for (i in 0 until view.childCount) findWebView(view.getChildAt(i))?.let { return it }
        return null
    }
}
