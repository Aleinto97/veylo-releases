package app.veylo.veylo_flutter

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File
import java.io.IOException

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        // Picks AI: the shared Rust backend needs a real model folder and the ONNX Runtime path.
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "veylo/picks").setMethodCallHandler { call, result ->
            if (call.method != "paths") { result.notImplemented(); return@setMethodCallHandler }
            Thread {
                try {
                    val paths = mapOf("model" to modelDir(), "runtime" to runtimePath())
                    runOnUiThread { result.success(paths) }
                } catch (e: Exception) {
                    runOnUiThread { result.error("picks", e.message, null) }
                }
            }.start()
        }
    }

    /** Unpack the shipped model assets into private storage once (same layout as the DX app). */
    private fun modelDir(): String {
        val root = File(filesDir, "picks-ai/bge-ea104dac-v1")
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
                assets.open("picks-ai/$name").use { input -> tmp.outputStream().use { output -> input.copyTo(output) } }
                if (tmp.length() == 0L) { tmp.delete(); throw IOException("Empty unpacked asset: $name") }
                if (dest.exists()) dest.delete()
                if (!tmp.renameTo(dest)) { tmp.delete(); throw IOException("Failed to place asset: $name") }
            }
            ready.writeText("bge-ea104dac-v1")
        }
        return root.absolutePath
    }

    private fun runtimePath(): String = File(applicationInfo.nativeLibraryDir, "libonnxruntime.so").absolutePath
}
