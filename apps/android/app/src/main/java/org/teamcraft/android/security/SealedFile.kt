package org.teamcraft.android.security

import java.io.File
import java.security.SecureRandom

/** A small secret kept sealed in the app's private files. */
class SealedFile(private val file: File, private val box: KeystoreBox) {
  fun read(): ByteArray? = if (file.exists()) box.open(file.readBytes()) else null

  fun write(secret: ByteArray) {
    file.parentFile?.mkdirs()
    val temporary = File(file.parentFile, "${file.name}.tmp")
    temporary.writeBytes(box.seal(secret))
    check(temporary.renameTo(file)) { "could not replace ${file.name}" }
  }

  fun delete() {
    file.delete()
  }

  /** The stored secret, or a new random one of [length] bytes stored first. */
  fun readOrCreate(length: Int): ByteArray =
    read() ?: ByteArray(length).also {
      SecureRandom().nextBytes(it)
      write(it)
    }
}
