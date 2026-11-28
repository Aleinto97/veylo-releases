package app.veylo.veylo_flutter

import android.content.pm.PackageManager
import android.content.res.Configuration
import android.os.CancellationSignal
import androidx.credentials.CredentialManager
import androidx.credentials.CredentialManagerCallback
import androidx.credentials.CustomCredential
import androidx.credentials.GetCredentialRequest
import androidx.credentials.GetCredentialResponse
import androidx.credentials.exceptions.GetCredentialCancellationException
import androidx.credentials.exceptions.GetCredentialException
import androidx.credentials.exceptions.GetCredentialProviderConfigurationException
import androidx.credentials.exceptions.GetCredentialUnsupportedException
import androidx.credentials.exceptions.NoCredentialException
import com.google.android.libraries.identity.googleid.GetGoogleIdOption
import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential
import com.google.mlkit.common.MlKitException
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File
import java.io.IOException
import java.util.concurrent.Executor

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "veylo/account").setMethodCallHandler { call, result ->
            when (call.method) {
                "googleIdToken" -> {
                    val webClientId = call.argument<String>("webClientId")
                    if (webClientId.isNullOrBlank()) {
                        result.error("error", "Google sign-in isn't set up in this build.", null)
                    } else {
                        googleIdToken(webClientId, result)
                    }
                }
                "scanCode" -> scanCode(result)
                else -> result.notImplemented()
            }
        }
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

    private fun googleIdToken(webClientId: String, reply: MethodChannel.Result) {
        val unavailable = "Google sign-in isn't available on this device — use a sign-in code from another device instead."
        val uiMode = resources.configuration.uiMode and Configuration.UI_MODE_TYPE_MASK
        if (uiMode == Configuration.UI_MODE_TYPE_TELEVISION ||
            packageManager.hasSystemFeature(PackageManager.FEATURE_LEANBACK)
        ) {
            reply.error("unavailable", unavailable, null)
            return
        }
        val retry = "Google sign-in couldn't open. Try again."
        try {
            val option = GetGoogleIdOption.Builder()
                .setServerClientId(webClientId)
                .setFilterByAuthorizedAccounts(false)
                .setAutoSelectEnabled(false)
                .build()
            val request = GetCredentialRequest.Builder().addCredentialOption(option).build()
            CredentialManager.create(this).getCredentialAsync(
                this, request, CancellationSignal(), Executor { runOnUiThread(it) },
                object : CredentialManagerCallback<GetCredentialResponse, GetCredentialException> {
                    override fun onResult(result: GetCredentialResponse) {
                        val credential = result.credential
                        val token = if (credential is CustomCredential &&
                            credential.type == GoogleIdTokenCredential.TYPE_GOOGLE_ID_TOKEN_CREDENTIAL
                        ) {
                            runCatching { GoogleIdTokenCredential.createFrom(credential.data).idToken }.getOrNull()
                        } else {
                            null
                        }
                        if (token.isNullOrBlank()) {
                            reply.error("error", "Google didn't return an account. Try signing in again.", null)
                        } else {
                            reply.success(token)
                        }
                    }

                    override fun onError(e: GetCredentialException) {
                        when (e) {
                            is GetCredentialCancellationException -> reply.error("cancel", null, null)
                            is GetCredentialUnsupportedException,
                            is GetCredentialProviderConfigurationException -> reply.error("unavailable", unavailable, null)
                            is NoCredentialException -> reply.error(
                                "error", "No Google account was offered. Try again or use a sign-in code.", null,
                            )
                            else -> reply.error("error", retry, null)
                        }
                    }
                },
            )
        } catch (_: Exception) {
            reply.error("error", retry, null)
        }
    }

    private fun scanCode(result: MethodChannel.Result) {
        val unavailable = "QR scanning isn't available on this device. Enter the sign-in code instead."
        try {
            val options = GmsBarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build()
            GmsBarcodeScanning.getClient(this, options).startScan()
                .addOnSuccessListener { barcode ->
                    val code = barcode.rawValue
                    if (code.isNullOrBlank()) {
                        result.error("error", "The QR code was empty. Try scanning it again.", null)
                    } else {
                        result.success(code)
                    }
                }
                .addOnCanceledListener { result.error("cancel", null, null) }
                .addOnFailureListener { error ->
                    when ((error as? MlKitException)?.errorCode) {
                        MlKitException.CANCELLED,
                        MlKitException.CODE_SCANNER_CANCELLED -> result.error("cancel", null, null)
                        MlKitException.NETWORK_ISSUE -> result.error("error", "Couldn't open the QR scanner. Check your connection and try again.", null)
                        else -> result.error("error", unavailable, null)
                    }
                }
        } catch (_: Exception) {
            result.error("error", unavailable, null)
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
