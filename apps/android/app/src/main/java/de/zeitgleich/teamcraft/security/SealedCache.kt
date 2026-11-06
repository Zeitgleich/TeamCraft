package de.zeitgleich.teamcraft.security

import java.io.File
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * Data larger than a key kept sealed in the app's private files, such as the chat list shown at
 * start (e2ee.md §9.2): AES-256-GCM in the app with a random key that the Keystore seals
 * ([SealedFile]). The Keystore's own cipher runs in the TEE and takes tens of milliseconds for a few
 * kilobytes on an older phone; unsealing only the key keeps a read to a few.
 *
 * Layout: `version (1) || iv (12) || ciphertext and tag`.
 */
class SealedCache(private val file: File, private val key: SealedFile) {
  /** The stored data, or `null` when there is none or it can't be opened (then it is dropped). */
  fun read(): ByteArray? {
    if (!file.exists()) return null
    val maybeKey = key.read() ?: return null
    return runCatching { open(file.readBytes(), maybeKey) }
      .onFailure { delete() }
      .getOrNull()
  }

  fun write(data: ByteArray) {
    file.parentFile?.mkdirs()
    val temporary = File(file.parentFile, "${file.name}.tmp")
    temporary.writeBytes(seal(data, key.readOrCreate(KEY_LENGTH)))
    check(temporary.renameTo(file)) { "could not replace ${file.name}" }
  }

  fun delete() {
    file.delete()
    key.delete()
  }

  private fun seal(data: ByteArray, secret: ByteArray): ByteArray {
    val iv = ByteArray(IV_LENGTH).also { SecureRandom().nextBytes(it) }
    val cipher = Cipher.getInstance(TRANSFORMATION)
    cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(secret, ALGORITHM), GCMParameterSpec(TAG_BITS, iv))
    return byteArrayOf(VERSION) + iv + cipher.doFinal(data)
  }

  private fun open(sealed: ByteArray, secret: ByteArray): ByteArray {
    require(sealed.size > 1 + IV_LENGTH && sealed[0] == VERSION) { "not a sealed cache" }
    val cipher = Cipher.getInstance(TRANSFORMATION)
    cipher.init(Cipher.DECRYPT_MODE, SecretKeySpec(secret, ALGORITHM), GCMParameterSpec(TAG_BITS, sealed, 1, IV_LENGTH))
    return cipher.doFinal(sealed, 1 + IV_LENGTH, sealed.size - 1 - IV_LENGTH)
  }

  private companion object {
    const val ALGORITHM = "AES"
    const val TRANSFORMATION = "AES/GCM/NoPadding"
    const val VERSION: Byte = 1
    const val KEY_LENGTH = 32
    const val IV_LENGTH = 12
    const val TAG_BITS = 128
  }
}
