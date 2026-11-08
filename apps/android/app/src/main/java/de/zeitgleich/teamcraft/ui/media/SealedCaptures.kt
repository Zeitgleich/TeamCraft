package de.zeitgleich.teamcraft.ui.media

import android.content.Context
import android.media.MediaDataSource
import android.media.MediaMetadataRetriever
import android.net.Uri
import androidx.annotation.OptIn
import androidx.media3.common.C
import androidx.media3.common.PlaybackException
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.BaseDataSource
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DataSourceException
import androidx.media3.datasource.DataSpec
import java.io.Closeable
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import de.zeitgleich.teamcraft.security.SealedMedia

/**
 * The media flow's camera files, sealed at rest (#1388, e2ee.md §9.2): photos are sealed from
 * memory before they touch the disk, videos as soon as the recording ends. Their names end in
 * [SUFFIX]; every reader of the flow goes through here to get the plaintext.
 */
object SealedCaptures {
  const val SUFFIX = ".sealed"

  /** Plain copies older versions made for Transformer; only the start sweep still looks for them. */
  const val OPENED_PREFIX = "open-"

  /** Transformer's output in the app's cache while a video is sent or saved; sealed ([SUFFIX]). */
  const val EXPORT_PREFIX = "export-"
  const val SAVE_PREFIX = "save-"

  /** Files [expose] made readable, by token; only for as long as an export runs. */
  private val exposed = ConcurrentHashMap<String, File>()

  fun isSealed(uri: Uri): Boolean = uri.scheme == "file" && uri.path?.endsWith(SUFFIX) == true

  fun isSealed(uri: String): Boolean = isSealed(Uri.parse(uri))

  /** The camera file's name as it is sent: without [SUFFIX]. */
  fun plainName(name: String): String = name.removeSuffix(SUFFIX)

  /** The key of this device's camera files; throws when the Keystore can't give it. */
  fun key(context: Context): ByteArray = MediaDraftStore.captureKey(context) ?: throw IOException("no key for camera files")

  fun readAll(context: Context, uri: Uri): ByteArray = SealedMedia.readAll(fileOf(uri), key(context))

  fun inputStream(context: Context, uri: Uri): InputStream {
    val reader = SealedMedia.Reader.open(fileOf(uri), key(context))
    val stream = reader.inputStream()
    return object : InputStream() {
      override fun read(): Int = stream.read()

      override fun read(b: ByteArray, off: Int, len: Int): Int = stream.read(b, off, len)

      override fun available(): Int = stream.available()

      override fun close() = reader.close()
    }
  }

  /** The plaintext's length. */
  fun size(context: Context, uri: Uri): Long = SealedMedia.Reader.open(fileOf(uri), key(context)).use { it.size }

  /**
   * Makes a sealed file readable through a `content:` URI of this app ([SealedMediaProvider]) until
   * [Exposure.close]. For readers that build their own data source from a URI and can't be given
   * [Factory] (Transformer's trim optimization): the plaintext is only ever decrypted into what
   * they read. The token is random and the provider isn't exported.
   */
  fun expose(context: Context, uri: Uri): Exposure {
    val file = fileOf(uri)
    val token = UUID.randomUUID().toString()
    exposed[token] = file
    val exposedUri = Uri.Builder().scheme("content").authority(authority(context)).appendPath(token).appendPath(plainName(file.name)).build()
    return Exposure(exposedUri) { exposed.remove(token) }
  }

  /** The file a URI of [expose] stands for; `null` for any other URI or once it was closed. */
  internal fun exposedFile(context: Context, uri: Uri): File? {
    if (uri.scheme != "content" || uri.authority != authority(context)) return null
    return uri.pathSegments.firstOrNull()?.let { exposed[it] }
  }

  private fun authority(context: Context): String = "${context.packageName}.sealedmedia"

  /** A sealed file readable as [uri] until closed. */
  class Exposure internal constructor(val uri: Uri, private val onClose: () -> Unit) : Closeable {
    override fun close() = onClose()
  }

  /** Points [retriever] at [uri], decrypting while it reads when the file is sealed. */
  fun setDataSource(retriever: MediaMetadataRetriever, context: Context, uri: Uri) {
    if (isSealed(uri)) {
      retriever.setDataSource(ReaderMediaDataSource(SealedMedia.Reader.open(fileOf(uri), key(context))))
    } else {
      retriever.setDataSource(context, uri)
    }
  }

  private fun fileOf(uri: Uri): File = File(requireNotNull(uri.path) { "no path" })

  /** The platform's random access source (MediaMetadataRetriever), over a sealed file. */
  private class ReaderMediaDataSource(private val reader: SealedMedia.Reader) : MediaDataSource() {
    override fun readAt(position: Long, buffer: ByteArray, offset: Int, size: Int): Int = reader.read(position, buffer, offset, size)

    override fun getSize(): Long = reader.size

    override fun close() = reader.close()
  }

  /**
   * Media3's source for the camera's sealed videos (ExoPlayer in the review, Transformer): seekable,
   * it decrypts the segments it reads. Other URIs go to [fallback].
   */
  @OptIn(UnstableApi::class)
  class Factory(context: Context, private val fallback: DataSource.Factory) : DataSource.Factory {
    private val context = context.applicationContext

    override fun createDataSource(): DataSource = SealedOrFallbackDataSource(context, fallback.createDataSource())
  }

  @OptIn(UnstableApi::class)
  private class SealedOrFallbackDataSource(private val context: Context, private val fallback: DataSource) : BaseDataSource(false) {
    private var maybeReader: SealedMedia.Reader? = null
    private var maybeUri: Uri? = null
    private var position = 0L
    private var bytesLeft = 0L
    private var isFallback = false

    override fun open(dataSpec: DataSpec): Long {
      val maybeFile = if (isSealed(dataSpec.uri)) fileOf(dataSpec.uri) else exposedFile(context, dataSpec.uri)
      if (maybeFile == null) {
        isFallback = true
        return fallback.open(dataSpec)
      }
      transferInitializing(dataSpec)
      val reader = try {
        SealedMedia.Reader.open(maybeFile, key(context))
      } catch (failure: IOException) {
        throw DataSourceException(failure, PlaybackException.ERROR_CODE_IO_FILE_NOT_FOUND)
      } catch (failure: IllegalArgumentException) {
        throw DataSourceException(failure, PlaybackException.ERROR_CODE_IO_UNSPECIFIED)
      }
      if (dataSpec.position > reader.size) {
        reader.close()
        throw DataSourceException(PlaybackException.ERROR_CODE_IO_READ_POSITION_OUT_OF_RANGE)
      }
      maybeReader = reader
      maybeUri = dataSpec.uri
      position = dataSpec.position
      bytesLeft = if (dataSpec.length != C.LENGTH_UNSET.toLong()) minOf(dataSpec.length, reader.size - position) else reader.size - position
      transferStarted(dataSpec)
      return bytesLeft
    }

    override fun read(buffer: ByteArray, offset: Int, length: Int): Int {
      if (isFallback) return fallback.read(buffer, offset, length)
      if (length == 0) return 0
      if (bytesLeft == 0L) return C.RESULT_END_OF_INPUT
      val reader = maybeReader ?: throw IllegalStateException("not open")
      val count = try {
        reader.read(position, buffer, offset, minOf(length.toLong(), bytesLeft).toInt())
      } catch (failure: IOException) {
        throw DataSourceException(failure, PlaybackException.ERROR_CODE_IO_UNSPECIFIED)
      }
      if (count < 0) return C.RESULT_END_OF_INPUT
      position += count
      bytesLeft -= count
      bytesTransferred(count)
      return count
    }

    override fun getUri(): Uri? = if (isFallback) fallback.uri else maybeUri

    override fun close() {
      if (isFallback) {
        isFallback = false
        fallback.close()
        return
      }
      maybeUri = null
      val reader = maybeReader ?: return
      maybeReader = null
      reader.close()
      transferEnded()
    }
  }
}
