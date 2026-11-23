package dev.veylo.dx

import android.app.Activity
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback

/**
 * Android Back closes what the Rust shell has open (a title page, Search, the videos,
 * the player) exactly like Esc does on desktop: it sends Esc to the page. Rust enables
 * it only while something is open, so Back on a top-level screen still leaves the app;
 * on a TV it is always on, so Back only goes back and the remote's Home key leaves.
 */
object VeyloBack {
    private var callback: OnBackPressedCallback? = null

    @JvmStatic
    fun setEnabled(activity: Activity, enabled: Boolean) {
        activity.runOnUiThread {
            val owner = activity as? ComponentActivity ?: return@runOnUiThread
            val cb = callback ?: object : OnBackPressedCallback(false) {
                override fun handleOnBackPressed() {
                    findWebView(activity.window.decorView)?.evaluateJavascript(
                        "document.querySelector('.app')?.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',code:'Escape',bubbles:true}))",
                        null,
                    )
                }
            }.also { owner.onBackPressedDispatcher.addCallback(it); callback = it }
            cb.isEnabled = enabled
        }
    }

    private fun findWebView(view: View): WebView? {
        if (view is WebView) return view
        if (view is ViewGroup) for (i in 0 until view.childCount) findWebView(view.getChildAt(i))?.let { return it }
        return null
    }
}
