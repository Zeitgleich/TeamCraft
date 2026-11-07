package de.zeitgleich.teamcraft.security

import java.io.Closeable
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.security.GeneralSecurityException
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * Photos and videos the media flow's camera took, sealed at rest (#1388, e2ee.md §9.2): AES-256-GCM
 * in segments, so a video can be played, seeked and trimmed from the sealed file without a
 * plaintext copy and without holding it in memory.
 *
 * Layout: `header || segment 0 || segment 1 || …`, the header being
 * `magic "TCSM" (4) || version (1) || segment size (4, big-endian) || salt (16)`. Every segment but
 * the last holds exactly the segment size of plaintext, the last one between 0 and that; each is
 * its ciphertext followed by the 16-byte tag. The file's key is `HMAC-SHA256(key, label || salt)`,
 * so no two files share a key and nonces can count: segment `i` has the nonce
 * `i (8, big-endian) || 0 0 0 || last (1)`, and the header is the associated data of every segment.
 * Reordered, dropped, appended or cut segments and a changed header all fail to open (the STREAM
 * construction of Hoang, Reyhanitabar, Rogaway and Vizár).
 */
object SealedMedia {
  const val SEGMENT_SIZE = 64 * 1024
  const val KEY_LENGTH = 32
  internal const val HEADER_LENGTH = 4 + 1 + 4 + 16
  internal const val TAG_LENGTH = 16
  private val MAGIC = byteArrayOf('T'.code.toByte(), 'C'.code.toByte(), 'S'.code.toByte(), 'M'.code.toByte())
  private const val VERSION: Byte = 1
  private const val SALT_LENGTH = 16
  private const val NONCE_LENGTH = 12
  private const val TRANSFORMATION = "AES/GCM/NoPadding"
  private val FILE_KEY_LABEL = "teamcraft sealed media v1".encodeToByteArray()

  /** A stream that seals what is written to [out]; closing it writes the last segment. */
  fun writer(out: OutputStream, key: ByteArray, segmentSize: Int = SEGMENT_SIZE, random: SecureRandom = SecureRandom()): OutputStream =
    Writer(out, key, segmentSize, random)

  /** Seals [data] into [target], written beside it and renamed (a kill leaves no half file). */
  fun sealTo(target: File, key: ByteArray, data: ByteArray) = sealTo(target, key) { it.write(data) }

  /** Seals [source] into [target] the same way; [source] is left as it is. */
  fun sealTo(target: File, key: ByteArray, source: File) = sealTo(target, key) { out -> source.inputStream().use { it.copyTo(out, SEGMENT_SIZE) } }

  private fun sealTo(target: File, key: ByteArray, write: (OutputStream) -> Unit) {
    val temporary = File(target.parentFile, "${target.name}.tmp")
    try {
      writer(temporary.outputStream().buffered(SEGMENT_SIZE), key).use(write)
      if (!temporary.renameTo(target)) throw IOException("could not write ${target.name}")
    } finally {
      temporary.delete()
    }
  }

  /** The whole plaintext of a sealed file (photos: a few megabytes). */
  fun readAll(file: File, key: ByteArray): ByteArray = Reader.open(file, key).use { reader ->
    val size = reader.size
    require(size <= Int.MAX_VALUE) { "too large to read whole" }
    val out = ByteArray(size.toInt())
    var done = 0
    while (done < out.size) {
      val read = reader.read(done.toLong(), out, done, out.size - done)
      if (read <= 0) throw IOException("sealed file ended early")
      done += read
    }
    out
  }

  /** Decrypts [source] into [target] (for tools that need a plain file, such as Transformer). */
  fun openTo(source: File, key: ByteArray, target: File) {
    try {
      Reader.open(source, key).use { reader -> target.outputStream().use { reader.inputStream().copyTo(it, SEGMENT_SIZE) } }
    } catch (failure: Throwable) {
      target.delete()
      throw failure
    }
  }

  /** Whether [file] starts like a sealed media file (it says nothing about whether it opens). */
  fun looksSealed(file: File): Boolean = runCatching {
    file.inputStream().use { input ->
      val start = ByteArray(MAGIC.size + 1)
      input.read(start) == start.size && start.copyOf(MAGIC.size).contentEquals(MAGIC) && start[MAGIC.size] == VERSION
    }
  }.getOrDefault(false)

  /**
   * Random access to the plaintext of a sealed file: reads decrypt the segments they touch (the
   * last one is kept). Safe to share between threads; reads are serialized.
   */
  class Reader private constructor(private val file: RandomAccessFile, key: ByteArray) : Closeable {
    private val header = ByteArray(HEADER_LENGTH)
    private val segmentSize: Int
    private val segmentCount: Long
    private val fileKey: SecretKeySpec

    /** The length of the plaintext. */
    val size: Long

    private var cachedIndex = -1L
    private var cached = ByteArray(0)

    init {
      try {
        file.seek(0)
        file.readFully(header)
        require(header.copyOf(MAGIC.size).contentEquals(MAGIC) && header[MAGIC.size] == VERSION) { "not sealed media" }
        segmentSize = ByteBuffer.wrap(header, MAGIC.size + 1, 4).int
        require(segmentSize in 1..(16 * 1024 * 1024)) { "bad segment size" }
        val body = file.length() - HEADER_LENGTH
        val sealedSegment = segmentSize.toLong() + TAG_LENGTH
        require(body >= TAG_LENGTH) { "no segments" }
        segmentCount = (body + sealedSegment - 1) / sealedSegment
        val last = body - (segmentCount - 1) * sealedSegment
        require(last >= TAG_LENGTH) { "cut segment" }
        size = (segmentCount - 1) * segmentSize + (last - TAG_LENGTH)
        fileKey = fileKey(key, header.copyOfRange(HEADER_LENGTH - SALT_LENGTH, HEADER_LENGTH))
      } catch (failure: Throwable) {
        file.close()
        throw failure
      }
    }

    /**
     * Up to [length] bytes of plaintext from [position] into [buffer]; -1 at the end. Throws
     * [IOException] when a segment doesn't open (changed or cut).
     */
    @Synchronized
    fun read(position: Long, buffer: ByteArray, offset: Int, length: Int): Int {
      require(position >= 0 && offset >= 0 && length >= 0 && offset + length <= buffer.size)
      if (position >= size) return -1
      if (length == 0) return 0
      val index = position / segmentSize
      val segment = segment(index)
      val within = (position - index * segmentSize).toInt()
      val count = minOf(length, segment.size - within)
      System.arraycopy(segment, within, buffer, offset, count)
      return count
    }

    /** The plaintext from the start as a stream. */
    fun inputStream(): InputStream = object : InputStream() {
      private var position = 0L

      override fun read(): Int {
        val one = ByteArray(1)
        return if (read(one, 0, 1) <= 0) -1 else one[0].toInt() and 0xff
      }

      override fun read(b: ByteArray, off: Int, len: Int): Int {
        val count = this@Reader.read(position, b, off, len)
        if (count > 0) position += count
        return count
      }

      override fun available(): Int = (size - position).coerceIn(0, Int.MAX_VALUE.toLong()).toInt()
    }

    private fun segment(index: Long): ByteArray {
      if (index == cachedIndex) return cached
      val sealedSegment = segmentSize.toLong() + TAG_LENGTH
      val isLast = index == segmentCount - 1
      val sealedLength = if (isLast) (file.length() - HEADER_LENGTH - index * sealedSegment).toInt() else sealedSegment.toInt()
      val sealed = ByteArray(sealedLength)
      file.seek(HEADER_LENGTH + index * sealedSegment)
      file.readFully(sealed)
      val plain = try {
        cipher(Cipher.DECRYPT_MODE, fileKey, header, index, isLast).doFinal(sealed)
      } catch (failure: GeneralSecurityException) {
        throw IOException("sealed media segment $index does not open", failure)
      }
      cachedIndex = index
      cached = plain
      return plain
    }

    override fun close() = file.close()

    companion object {
      fun open(file: File, key: ByteArray): Reader = Reader(RandomAccessFile(file, "r"), key)
    }
  }

  private class Writer(private val out: OutputStream, key: ByteArray, private val segmentSize: Int, random: SecureRandom) : OutputStream() {
    private val header: ByteArray
    private val fileKey: SecretKeySpec
    private val buffer = ByteArray(segmentSize)
    private var filled = 0
    private var index = 0L
    private var isClosed = false

    init {
      require(segmentSize > 0)
      val salt = ByteArray(SALT_LENGTH).also(random::nextBytes)
      header = ByteBuffer.allocate(HEADER_LENGTH).put(MAGIC).put(VERSION).putInt(segmentSize).put(salt).array()
      fileKey = fileKey(key, salt)
      out.write(header)
    }

    override fun write(b: Int) = write(byteArrayOf(b.toByte()), 0, 1)

    override fun write(b: ByteArray, off: Int, len: Int) {
      check(!isClosed) { "closed" }
      var from = off
      var left = len
      while (left > 0) {
        // A full segment goes once more follows: only close knows which one is the last.
        if (filled == segmentSize) emit(isLast = false)
        val count = minOf(left, segmentSize - filled)
        System.arraycopy(b, from, buffer, filled, count)
        filled += count
        from += count
        left -= count
      }
    }

    override fun flush() = out.flush()

    override fun close() {
      if (isClosed) return
      isClosed = true
      try {
        emit(isLast = true)
        out.flush()
      } finally {
        out.close()
        buffer.fill(0)
      }
    }

    private fun emit(isLast: Boolean) {
      out.write(cipher(Cipher.ENCRYPT_MODE, fileKey, header, index, isLast).doFinal(buffer, 0, filled))
      index += 1
      filled = 0
    }
  }

  private fun fileKey(key: ByteArray, salt: ByteArray): SecretKeySpec {
    require(key.size == KEY_LENGTH) { "bad key" }
    val mac = Mac.getInstance("HmacSHA256")
    mac.init(SecretKeySpec(key, "HmacSHA256"))
    mac.update(FILE_KEY_LABEL)
    mac.update(salt)
    return SecretKeySpec(mac.doFinal(), "AES")
  }

  private fun cipher(mode: Int, fileKey: SecretKeySpec, header: ByteArray, index: Long, isLast: Boolean): Cipher {
    val nonce = ByteBuffer.allocate(NONCE_LENGTH).putLong(index).put(0).put(0).put(0).put(if (isLast) 1 else 0).array()
    return Cipher.getInstance(TRANSFORMATION).apply {
      init(mode, fileKey, GCMParameterSpec(TAG_LENGTH * 8, nonce))
      updateAAD(header)
    }
  }
}
