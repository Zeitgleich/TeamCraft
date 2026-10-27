package de.zeitgleich.teamcraft.calls

import android.content.Context
import android.util.Log
import java.io.ByteArrayOutputStream
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.installVideoCodec
import uniffi.teamcraft_ffi.useInstalledVideoCodec
import uniffi.teamcraft_ffi.videoCodecDownloadUrl

/**
 * Cisco's OpenH264 for decoding video, downloaded by the phone itself on first use (only then do
 * Cisco's patent licenses cover it, so the app can't ship it; teamcraft-media `openh264_library`).
 * The core checks the download against Cisco's published hashes before loading it.
 */
object VideoCodecInstaller {
  private const val TAG = "TeamCraftVideo"
  /** Cisco's compressed libraries are about 1 MB. */
  private const val MAX_DOWNLOAD_BYTES = 16 * 1024 * 1024
  private const val TIMEOUT_MS = 30_000
  private val lock = Mutex()

  /** `true` once video can be decoded; a failed download is retried on the next call. */
  suspend fun ensure(context: Context): Boolean = lock.withLock {
    withContext(Dispatchers.IO) {
      val directory = File(context.filesDir, "codecs").absolutePath
      if (useInstalledVideoCodec(directory)) return@withContext true
      val url = videoCodecDownloadUrl() ?: return@withContext false
      try {
        installVideoCodec(directory, download(url))
        true
      } catch (error: Exception) {
        Log.w(TAG, "OpenH264 could not be installed; receiving video stays off", error)
        false
      }
    }
  }

  private fun download(url: String): ByteArray {
    val connection = URL(url).openConnection() as HttpURLConnection
    connection.connectTimeout = TIMEOUT_MS
    connection.readTimeout = TIMEOUT_MS
    try {
      val body = ByteArrayOutputStream()
      connection.inputStream.use { input ->
        val buffer = ByteArray(64 * 1024)
        while (true) {
          val read = input.read(buffer)
          if (read < 0) break
          body.write(buffer, 0, read)
          check(body.size() <= MAX_DOWNLOAD_BYTES) { "download too large" }
        }
      }
      return body.toByteArray()
    } finally {
      connection.disconnect()
    }
  }
}
