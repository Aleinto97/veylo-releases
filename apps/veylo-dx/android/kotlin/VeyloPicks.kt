package dev.veylo.dx

import android.app.Activity
import java.io.File

/** Unpack shipped model assets into private storage; no networking or user data. */
object VeyloPicks {
    @JvmStatic
    fun modelDir(activity: Activity): String {
        val root = File(activity.filesDir, "picks-ai/21edefaf-v1")
        root.mkdirs()
        for (name in arrayOf("model.onnx", "tokenizer.json", "LICENSE", "NOTICE")) {
            val dest = File(root, name)
            activity.assets.open("picks-ai/$name").use { input ->
                // A marker is written only after both large files finished copying.
                if (!File(root, "ready").exists() || !dest.isFile) {
                    dest.outputStream().use { output -> input.copyTo(output) }
                }
            }
        }
        File(root, "ready").writeText("21edefaf-v1")
        return root.absolutePath
    }

    @JvmStatic
    fun runtimePath(activity: Activity): String =
        File(activity.applicationInfo.nativeLibraryDir, "libonnxruntime.so").absolutePath
}
