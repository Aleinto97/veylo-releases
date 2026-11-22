package dev.veylo.dx

import android.app.Activity
import android.os.CancellationSignal
import androidx.credentials.CredentialManager
import androidx.credentials.CredentialManagerCallback
import androidx.credentials.CustomCredential
import androidx.credentials.GetCredentialRequest
import androidx.credentials.GetCredentialResponse
import androidx.credentials.exceptions.GetCredentialCancellationException
import androidx.credentials.exceptions.GetCredentialException
import com.google.android.libraries.identity.googleid.GetGoogleIdOption
import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
import java.util.concurrent.Executor

/**
 * Account sign-in on Android, all inside the app (src/backend/account.rs):
 * - Google: Android's own account sheet (Credential Manager), which hands back a Google ID token;
 * - a computer's sign-in QR code: Google's code scanner, which needs no camera permission.
 * Each starts at once and leaves its answer for Rust to take (`take`): "token:…", "code:…",
 * "cancel:" or "error:…".
 */
object VeyloAccount {
    @Volatile private var result: String? = null

    /** The last answer, once. */
    @JvmStatic
    fun take(): String? { val r = result; result = null; return r }

    @JvmStatic
    fun signInWithGoogle(activity: Activity, webClientId: String) {
        result = null
        val option = GetGoogleIdOption.Builder()
            .setServerClientId(webClientId)
            .setFilterByAuthorizedAccounts(false)
            .setAutoSelectEnabled(false)
            .build()
        val request = GetCredentialRequest.Builder().addCredentialOption(option).build()
        val onUi = Executor { activity.runOnUiThread(it) }
        activity.runOnUiThread {
            CredentialManager.create(activity).getCredentialAsync(activity, request, CancellationSignal(), onUi,
                object : CredentialManagerCallback<GetCredentialResponse, GetCredentialException> {
                    override fun onResult(response: GetCredentialResponse) {
                        val c = response.credential
                        result = if (c is CustomCredential && c.type == GoogleIdTokenCredential.TYPE_GOOGLE_ID_TOKEN_CREDENTIAL) {
                            "token:" + GoogleIdTokenCredential.createFrom(c.data).idToken
                        } else "error:Google gave no account"
                    }
                    override fun onError(e: GetCredentialException) {
                        result = if (e is GetCredentialCancellationException) "cancel:" else "error:" + (e.message ?: e.type)
                    }
                })
        }
    }

    @JvmStatic
    fun scanCode(activity: Activity) {
        result = null
        val options = GmsBarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build()
        activity.runOnUiThread {
            GmsBarcodeScanning.getClient(activity, options).startScan()
                .addOnSuccessListener { result = "code:" + (it.rawValue ?: "") }
                .addOnCanceledListener { result = "cancel:" }
                .addOnFailureListener { result = "error:" + (it.message ?: "scanner unavailable") }
        }
    }
}
