package de.zeitgleich.teamcraft.calls

import android.content.Context
import android.util.Log
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.time.Duration
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.core.CoreHttp
import uniffi.teamcraft_ffi.HttpFetchRedirects
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
  private val TIMEOUT: Duration = Duration.ofSeconds(30)
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
      } catch (cancelled: CancellationException) {
        // The download stops with the call that wanted it; the next call tries again.
        throw cancelled
      } catch (error: Exception) {
        Log.w(TAG, "OpenH264 could not be installed; receiving video stays off", error)
        false
      }
    }
  }

  /** Through the core's HTTP client ([CoreHttp]); the timeout is per read, not for the whole download. */
  private suspend fun download(url: String): ByteArray {
    val answer = CoreHttp.send(
      CoreHttp.request(
        url = url,
        maxBodyBytes = MAX_DOWNLOAD_BYTES,
        connectTimeout = TIMEOUT,
        readTimeout = TIMEOUT,
        redirects = HttpFetchRedirects.FOLLOW,
        // Cisco offers the libraries over plain HTTP only; the core checks the download against
        // Cisco's published hash before it loads it.
        allowInsecureHttp = true,
      ),
    )
    if (answer.status.toInt() != HttpURLConnection.HTTP_OK) throw IOException("status ${answer.status}")
    return answer.body
  }
}
