package de.zeitgleich.teamcraft.security

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Seals small secrets (the database key, the OIDC session) with a non-exportable AES-256-GCM key
 * in the Android Keystore, hardware-backed (StrongBox or TEE) where the device has it
 * (e2ee.md §9.2). No user authentication is required, so push wake-ups can unseal while locked.
 *
 * Layout: `version (1) || iv (12) || ciphertext and tag`.
 */
class KeystoreBox(private val alias: String) {
  private val keyStore: KeyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }

  fun seal(plaintext: ByteArray): ByteArray {
    val cipher = Cipher.getInstance(TRANSFORMATION)
    cipher.init(Cipher.ENCRYPT_MODE, key())
    val iv = cipher.iv
    check(iv.size == IV_LENGTH) { "unexpected IV length" }
    return byteArrayOf(VERSION) + iv + cipher.doFinal(plaintext)
  }

  /** Throws if the data was sealed with another key, changed, or is malformed. */
  fun open(sealed: ByteArray): ByteArray {
    require(sealed.size > 1 + IV_LENGTH && sealed[0] == VERSION) { "not a sealed record" }
    val cipher = Cipher.getInstance(TRANSFORMATION)
    cipher.init(
      Cipher.DECRYPT_MODE,
      key(),
      GCMParameterSpec(TAG_BITS, sealed, 1, IV_LENGTH),
    )
    return cipher.doFinal(sealed, 1 + IV_LENGTH, sealed.size - 1 - IV_LENGTH)
  }

  private fun key(): SecretKey =
    (keyStore.getKey(alias, null) as? SecretKey) ?: generate()

  private fun generate(): SecretKey {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      try {
        return generate(strongBox = true)
      } catch (unavailable: StrongBoxUnavailableException) {
        // Most phones have no StrongBox; the TEE-backed key below is the normal case.
      }
    }
    return generate(strongBox = false)
  }

  private fun generate(strongBox: Boolean): SecretKey {
    val spec = KeyGenParameterSpec.Builder(
      alias,
      KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
    )
      .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
      .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
      .setKeySize(KEY_BITS)
      .apply {
        if (strongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) setIsStrongBoxBacked(true)
      }
      .build()
    val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
    generator.init(spec)
    return generator.generateKey()
  }

  private companion object {
    const val ANDROID_KEYSTORE = "AndroidKeyStore"
    const val TRANSFORMATION = "AES/GCM/NoPadding"
    const val VERSION: Byte = 1
    const val IV_LENGTH = 12
    const val TAG_BITS = 128
    const val KEY_BITS = 256
  }
}
