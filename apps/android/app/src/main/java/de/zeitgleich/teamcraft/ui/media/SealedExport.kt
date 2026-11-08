package de.zeitgleich.teamcraft.ui.media

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.net.Uri
import android.os.Handler
import android.os.HandlerThread
import android.os.ParcelFileDescriptor
import android.os.ProxyFileDescriptorCallback
import android.os.storage.StorageManager
import android.system.ErrnoException
import android.system.OsConstants
import androidx.annotation.OptIn
import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.Metadata
import androidx.media3.common.MimeTypes
import androidx.media3.common.util.UnstableApi
import androidx.media3.container.Mp4OrientationData
import androidx.media3.muxer.BufferInfo
import androidx.media3.muxer.Mp4Muxer
import androidx.media3.muxer.Muxer
import androidx.media3.muxer.MuxerException
import androidx.media3.muxer.SeekableMuxerOutput
import com.google.common.collect.ImmutableList
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.nio.ByteBuffer
import de.zeitgleich.teamcraft.security.SealedMedia

/**
 * Serves files [SealedCaptures.expose] named to the app's own readers of a URI, the plaintext coming
 * from the sealed file as it is read: a descriptor backed by [readers][SealedMedia.Reader] through
 * the platform's proxy file system, so there is no plain file behind it (#1403). Read only, not
 * exported, and a URI that wasn't exposed (or is no longer) opens nothing.
 */
class SealedMediaProvider : ContentProvider() {
  private val thread by lazy { HandlerThread("sealed-media").apply { start() } }

  override fun onCreate(): Boolean = true

  override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
    if (mode != "r") throw FileNotFoundException("read only")
    val context = requireNotNull(context)
    val file = SealedCaptures.exposedFile(context, uri) ?: throw FileNotFoundException("not exposed")
    val reader = try {
      SealedMedia.Reader.open(file, SealedCaptures.key(context))
    } catch (failure: IOException) {
      throw FileNotFoundException("does not open: ${failure.message}")
    } catch (failure: IllegalArgumentException) {
      throw FileNotFoundException("does not open: ${failure.message}")
    }
    try {
      val storage = context.getSystemService(StorageManager::class.java)
      return storage.openProxyFileDescriptor(ParcelFileDescriptor.MODE_READ_ONLY, ReaderCallback(reader), Handler(thread.looper))
    } catch (failure: IOException) {
      reader.close()
      throw FileNotFoundException("no descriptor: ${failure.message}")
    }
  }

  override fun getType(uri: Uri): String = "video/mp4"

  override fun query(uri: Uri, projection: Array<String>?, selection: String?, selectionArgs: Array<String>?, sortOrder: String?): Cursor? = null

  override fun insert(uri: Uri, values: ContentValues?): Uri? = null

  override fun delete(uri: Uri, selection: String?, selectionArgs: Array<String>?): Int = 0

  override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<String>?): Int = 0

  private class ReaderCallback(private val reader: SealedMedia.Reader) : ProxyFileDescriptorCallback() {
    override fun onGetSize(): Long = reader.size

    override fun onRead(offset: Long, size: Int, data: ByteArray): Int {
      var done = 0
      try {
        while (done < size) {
          val read = reader.read(offset + done, data, done, size - done)
          if (read <= 0) break
          done += read
        }
      } catch (failure: IOException) {
        throw ErrnoException("read", OsConstants.EIO, failure)
      }
      return done
    }

    override fun onRelease() = reader.close()
  }
}

/**
 * Transformer's MP4 muxer writing into a sealed file (the path it is given) instead of a plain one:
 * [SealedMedia.SeekableWriter] takes the writes, seeks and all. Like the export before it, only how
 * to turn the video goes along: no place, no XMP, no camera details.
 */
@OptIn(UnstableApi::class)
class SealedMp4MuxerFactory(private val key: ByteArray) : Muxer.Factory {
  override fun create(path: String): Muxer {
    val writer = try {
      SealedMedia.SeekableWriter(File(path), key)
    } catch (failure: IOException) {
      throw MuxerException("could not create the sealed output", failure)
    }
    return OrientationOnlyMuxer(Mp4Muxer.Builder(SealedMuxerOutput(writer)).setAttemptStreamableOutputEnabled(true).build())
  }

  override fun getSupportedSampleMimeTypes(trackType: Int): ImmutableList<String> = when (trackType) {
    C.TRACK_TYPE_VIDEO -> Mp4Muxer.SUPPORTED_VIDEO_SAMPLE_MIME_TYPES
    C.TRACK_TYPE_AUDIO -> Mp4Muxer.SUPPORTED_AUDIO_SAMPLE_MIME_TYPES
    else -> ImmutableList.of()
  }

  override fun supportsWritingNegativeTimestampsInEditList(): Boolean = true
}

/** [Mp4Muxer] keeping only the video's turn from the metadata it is given. */
@OptIn(UnstableApi::class)
private class OrientationOnlyMuxer(private val muxer: Mp4Muxer) : Muxer {
  override fun addTrack(format: Format): Int {
    val trackId = muxer.addTrack(format)
    if (MimeTypes.isVideo(format.sampleMimeType)) muxer.addMetadataEntry(Mp4OrientationData(format.rotationDegrees))
    return trackId
  }

  override fun writeSampleData(trackId: Int, byteBuffer: ByteBuffer, bufferInfo: BufferInfo) = muxer.writeSampleData(trackId, byteBuffer, bufferInfo)

  override fun addMetadataEntry(metadataEntry: Metadata.Entry) {
    if (metadataEntry is Mp4OrientationData) muxer.addMetadataEntry(metadataEntry)
  }

  override fun close() = muxer.close()
}

/** The muxer's output: seeks and writes on a [SealedMedia.SeekableWriter]. */
@OptIn(UnstableApi::class)
private class SealedMuxerOutput(private val writer: SealedMedia.SeekableWriter) : SeekableMuxerOutput {
  private var isClosed = false

  override fun write(source: ByteBuffer): Int = writer.write(source)

  override fun getPosition(): Long = writer.position

  override fun setPosition(position: Long) = writer.seek(position)

  override fun getSize(): Long = writer.size

  override fun truncate(size: Long) = writer.truncate(size)

  override fun isOpen(): Boolean = !isClosed

  override fun close() {
    isClosed = true
    writer.close()
  }
}
