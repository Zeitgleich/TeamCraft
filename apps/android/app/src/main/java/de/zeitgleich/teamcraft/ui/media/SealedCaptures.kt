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
import java.io.File
import java.io.IOException
import java.io.InputStream
import de.zeitgleich.teamcraft.security.SealedMedia

/**
 * The media flow's camera files, sealed at rest (#1388, e2ee.md §9.2): photos are sealed from
 * memory before they touch the disk, videos as soon as the recording ends. Their names end in
 * [SUFFIX]; every reader of the flow goes through here to get the plaintext.
 */
object SealedCaptures {
  const val SUFFIX = ".sealed"

  /** Plain copies made for a moment (Transformer and the upload need a plain file). */
  const val OPENED_PREFIX = "open-"

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
   * A plain copy beside the sealed file, for what can only read files (Transformer, the upload).
   * The caller deletes it; one left by a kill goes at the next start ([MediaDraftStore.sweep]).
   */
  fun openToFile(context: Context, uri: Uri): File {
    val source = fileOf(uri)
    val ending = plainName(source.name).substringAfterLast('.', "bin")
    val target = File(source.parentFile, "$OPENED_PREFIX${System.nanoTime()}.$ending")
    SealedMedia.openTo(source, key(context), target)
    return target
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
   * Media3's source for the camera's sealed videos (ExoPlayer in the review): seekable, it decrypts
   * the segments it reads. Other URIs go to [fallback].
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
      if (!isSealed(dataSpec.uri)) {
        isFallback = true
        return fallback.open(dataSpec)
      }
      transferInitializing(dataSpec)
      val reader = try {
        SealedMedia.Reader.open(fileOf(dataSpec.uri), key(context))
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
