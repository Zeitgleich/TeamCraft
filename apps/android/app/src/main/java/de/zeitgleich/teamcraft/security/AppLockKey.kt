package de.zeitgleich.teamcraft.security

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.util.Log
import androidx.annotation.RequiresApi
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.security.ProviderException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey

private const val TAG = "AppLockKey"

/**
 * The Keystore key an unlock is proven with (e2ee.md §9.2, "App lock"; #1200): AES-256-GCM, usable
 * for one operation only after the system prompt checked strong biometrics or the screen lock.
 * The prompt authorizes the cipher handed to it; the unlock counts only once that very cipher
 * encrypts, which the Keystore refuses without a fresh authentication. A forged success callback
 * therefore opens nothing. The key guards no data: it is the proof, not a secret.
 *
 * The device invalidates the key when the screen lock is removed or a new fingerprint or face is
 * enrolled; [begin] reports it, and the gate replaces the key before the prompt.
 */
@RequiresApi(Build.VERSION_CODES.R)
class AppLockKey : UnlockKey<Cipher> {
  // Lazy: a Keystore failure surfaces in [begin], where the gate handles it.
  private val keyStore: KeyStore by lazy { KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) } }

  override fun begin(): Cipher {
    val cipher = Cipher.getInstance(TRANSFORMATION)
    try {
      cipher.init(Cipher.ENCRYPT_MODE, key())
    } catch (invalidated: KeyPermanentlyInvalidatedException) {
      throw UnlockKeyInvalidatedException(invalidated)
    }
    return cipher
  }

  override fun prove(challenge: Cipher, maybeProof: Cipher?): Proof {
    // Only the cipher handed to the prompt: any other one (a software cipher) would encrypt freely.
    if (maybeProof !== challenge) return Proof.Refused
    return try {
      challenge.doFinal(PROOF_PLAINTEXT)
      Proof.Valid
    } catch (invalidated: KeyPermanentlyInvalidatedException) {
      Proof.KeyBroken
    } catch (keystoreFailure: ProviderException) {
      // The Keystore itself failed (seen on some devices); a new key may work.
      Proof.KeyBroken
    } catch (notAuthorized: GeneralSecurityException) {
      // Without the prompt's authorization the Keystore refuses the operation
      // (IllegalBlockSizeException wrapping "key user not authenticated").
      Proof.Refused
    } catch (notInitialized: IllegalStateException) {
      Proof.Refused
    }
  }

  override fun reset() {
    try {
      if (keyStore.containsAlias(ALIAS)) keyStore.deleteEntry(ALIAS)
    } catch (failure: Exception) {
      // Left as it is: an unusable key is replaced at the next unlock, a usable one needs the prompt.
      Log.w(TAG, "app lock key not removed: ${failure.javaClass.simpleName}")
    }
  }

  private fun key(): SecretKey =
    (keyStore.getKey(ALIAS, null) as? SecretKey) ?: generate()

  // No StrongBox: the key protects no data, it only proves the prompt, and StrongBox makes
  // creating it slower and fails on many devices.
  private fun generate(): SecretKey {
    val spec = KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT)
      .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
      .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
      .setKeySize(KEY_BITS)
      .setUserAuthenticationRequired(true)
      // 0: every use needs its own prompt, bound to the operation (no time window).
      .setUserAuthenticationParameters(0, AUTHENTICATORS)
      .build()
    val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
    generator.init(spec)
    return generator.generateKey()
  }

  companion object {
    private const val ANDROID_KEYSTORE = "AndroidKeyStore"
    private const val ALIAS = "teamcraft_app_lock"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val KEY_BITS = 256
    private const val AUTHENTICATORS =
      KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL
    private val PROOF_PLAINTEXT = byteArrayOf(1)
  }
}

/** Removes the app lock key when the lock is turned off; the next unlock creates a new one. */
fun deleteAppLockKey() {
  if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) AppLockKey().reset()
}
