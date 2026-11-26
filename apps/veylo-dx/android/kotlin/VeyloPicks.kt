package dev.veylo.dx

import android.app.Activity
import java.io.File

/** Unpack shipped model assets into private storage; no networking or user data. */
object VeyloPicks {
    @JvmStatic
    fun modelDir(activity: Activity): String {
        // The earlier classifier's copy is no longer used.
        File(activity.filesDir, "picks-ai/21edefaf-v1").deleteRecursively()
        val root = File(activity.filesDir, "picks-ai/bge-ea104dac-v1")
        root.mkdirs()
        val files = arrayOf("model.onnx", "tokenizer.json", "LICENSE", "NOTICE")
        val ready = File(root, "ready")
        val needsUnpack = !ready.isFile || files.any { val f = File(root, it); !f.isFile || f.length() == 0L }
        if (needsUnpack) {
            ready.delete()
            for (name in files) {
                val dest = File(root, name)
                val tmp = File(root, "$name.tmp")
                tmp.delete()
                activity.assets.open("picks-ai/$name").use { input ->
                    tmp.outputStream().use { output -> input.copyTo(output) }
                }
                if (tmp.length() == 0L) {
                    tmp.delete()
                    throw java.io.IOException("Empty unpacked asset: $name")
                }
                if (dest.exists()) dest.delete()
                if (!tmp.renameTo(dest)) {
                    tmp.delete()
                    throw java.io.IOException("Failed to place asset: $name")
                }
            }
            ready.writeText("bge-ea104dac-v1")
        }
        return root.absolutePath
    }

    @JvmStatic
    fun runtimePath(activity: Activity): String =
        File(activity.applicationInfo.nativeLibraryDir, "libonnxruntime.so").absolutePath
}
