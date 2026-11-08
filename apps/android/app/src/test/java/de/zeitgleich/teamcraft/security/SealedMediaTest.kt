package de.zeitgleich.teamcraft.security

import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.file.Files
import kotlin.random.Random
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class SealedMediaTest {
  private val key = ByteArray(SealedMedia.KEY_LENGTH) { it.toByte() }
  private val segment = 64

  @Test
  fun whatIsSealedOpensAgainWhateverItsLength() {
    for (length in listOf(0, 1, segment - 1, segment, segment + 1, 3 * segment, 3 * segment + 17)) {
      val plain = Random(length).nextBytes(length)
      val file = sealed(plain)
      SealedMedia.Reader.open(file, key).use { reader ->
        assertEquals(length.toLong(), reader.size)
        assertArrayEquals(plain, reader.inputStream().readBytes())
      }
      assertEquals(length.toLong() + SealedMedia.HEADER_LENGTH + segments(length) * SealedMedia.TAG_LENGTH, file.length())
    }
  }

  @Test
  fun readsAnywhereGiveThePlaintextThere() {
    val plain = Random(1).nextBytes(5 * segment + 9)
    SealedMedia.Reader.open(sealed(plain), key).use { reader ->
      for (position in listOf(0, 1, segment - 1, segment, 2 * segment + 5, plain.size - 1)) {
        val buffer = ByteArray(segment * 2)
        val read = reader.read(position.toLong(), buffer, 3, buffer.size - 3)
        assertTrue(read > 0)
        assertArrayEquals(plain.copyOfRange(position, position + read), buffer.copyOfRange(3, 3 + read))
      }
      assertEquals(-1, reader.read(plain.size.toLong(), ByteArray(4), 0, 4))
    }
  }

  @Test
  fun aWholeFileIsReadAtOnceAndFilesAreSealedAndOpenedThroughFiles() {
    val plain = Random(2).nextBytes(200_000)
    val directory = Files.createTempDirectory("sealed-media").toFile()
    val source = File(directory, "video.mp4").apply { writeBytes(plain) }
    val target = File(directory, "video.mp4.sealed")
    SealedMedia.sealTo(target, key, source)
    assertTrue(SealedMedia.looksSealed(target))
    assertFalse(SealedMedia.looksSealed(source))
    assertFalse(File(directory, "video.mp4.sealed.tmp").exists())
    assertArrayEquals(plain, SealedMedia.readAll(target, key))
  }

  @Test
  fun theSamePlaintextSealsDifferentlyEveryTime() {
    val plain = ByteArray(100)
    assertNotEquals(sealed(plain).readBytes().toList(), sealed(plain).readBytes().toList())
  }

  @Test
  fun anotherKeyDoesNotOpenIt() {
    val file = sealed(Random(3).nextBytes(100))
    val other = ByteArray(SealedMedia.KEY_LENGTH) { 7 }
    assertThrows(IOException::class.java) { SealedMedia.Reader.open(file, other).use { it.inputStream().readBytes() } }
  }

  @Test
  fun aChangedByteAnywhereFailsToOpen() {
    val plain = Random(4).nextBytes(2 * segment + 10)
    val length = sealed(plain).length()
    for (position in listOf(0L, 5L, SealedMedia.HEADER_LENGTH - 1L, SealedMedia.HEADER_LENGTH.toLong(), length / 2, length - 1)) {
      val file = sealed(plain)
      RandomAccessFile(file, "rw").use { raf ->
        raf.seek(position)
        val byte = raf.read()
        raf.seek(position)
        raf.write(byte xor 0x01)
      }
      assertTrue("byte $position", runCatching { SealedMedia.readAll(file, key) }.isFailure)
    }
  }

  @Test
  fun aFileCutAtASegmentBoundaryFailsToOpen() {
    val plain = Random(5).nextBytes(3 * segment)
    val file = sealed(plain)
    // Without its last segment the one before claims to be the last, which it was not sealed as.
    RandomAccessFile(file, "rw").use { it.setLength(SealedMedia.HEADER_LENGTH + 2L * (segment + SealedMedia.TAG_LENGTH)) }
    assertThrows(IOException::class.java) { SealedMedia.readAll(file, key) }
  }

  @Test
  fun aSegmentAppendedOrSwappedFailsToOpen() {
    val plain = Random(6).nextBytes(3 * segment)
    val sealedSegment = segment + SealedMedia.TAG_LENGTH
    val bytes = sealed(plain).readBytes()
    val appended = file(bytes + bytes.copyOfRange(SealedMedia.HEADER_LENGTH, SealedMedia.HEADER_LENGTH + sealedSegment))
    assertThrows(IOException::class.java) { SealedMedia.readAll(appended, key) }
    val swapped = bytes.copyOf()
    val first = SealedMedia.HEADER_LENGTH
    val second = first + sealedSegment
    bytes.copyInto(swapped, first, second, second + sealedSegment)
    bytes.copyInto(swapped, second, first, first + sealedSegment)
    assertThrows(IOException::class.java) { SealedMedia.readAll(file(swapped), key) }
  }

  @Test
  fun somethingElseIsNotTakenForSealedMedia() {
    assertThrows(IllegalArgumentException::class.java) { SealedMedia.Reader.open(file("not sealed at all, just text".encodeToByteArray()), key) }
    assertThrows(Exception::class.java) { SealedMedia.Reader.open(file(ByteArray(3)), key) }
  }

  private fun segments(length: Int): Int = maxOf(1, (length + segment - 1) / segment)

  @Test
  fun aSeekableFileOpensWhateverItsLength() {
    for (length in listOf(0, 1, segment - 1, segment, segment + 1, 3 * segment, 3 * segment + 17)) {
      val plain = Random(length).nextBytes(length)
      val file = seekable { write(ByteBuffer.wrap(plain)) }
      SealedMedia.Reader.open(file, key).use { reader ->
        assertEquals(length.toLong(), reader.size)
        assertArrayEquals(plain, reader.inputStream().readBytes())
      }
      assertTrue(SealedMedia.looksSealed(file))
    }
  }

  @Test
  fun whatAMuxerDoesEndsUpAsThePlaintext() {
    // Writes in pieces, reserves room, goes back to patch sizes, grows, rewrites its index.
    val body = Random(30).nextBytes(5 * segment + 9)
    val expected = ByteArray(40) + body + ByteArray(20) { 7 }
    val file = seekable {
      seek(40)
      var from = 0
      var step = 1
      while (from < body.size) {
        val count = minOf(step, body.size - from)
        write(ByteBuffer.wrap(body, from, count))
        from += count
        step = step * 3 % 101 + 1
      }
      write(ByteBuffer.wrap(ByteArray(20) { 9 }))
      val end = position
      seek(0)
      write(ByteBuffer.wrap(ByteArray(40)))
      seek(end - 20)
      write(ByteBuffer.wrap(ByteArray(20) { 7 }))
    }
    assertArrayEquals(expected, SealedMedia.readAll(file, key))
  }

  @Test
  fun aSeekableWriteAcrossSegmentsReachesThemAll() {
    val file = seekable {
      write(ByteBuffer.wrap(ByteArray(4 * segment) { 1 }))
      seek(segment - 3L)
      write(ByteBuffer.wrap(ByteArray(2 * segment + 6) { 2 }))
    }
    val expected = ByteArray(4 * segment) { 1 }
    expected.fill(2, segment - 3, 3 * segment + 3)
    assertArrayEquals(expected, SealedMedia.readAll(file, key))
  }

  @Test
  fun skippedPartsOfASeekableFileAreZeros() {
    val file = seekable {
      seek(3L * segment + 5)
      write(ByteBuffer.wrap(byteArrayOf(1, 2, 3)))
    }
    val expected = ByteArray(3 * segment + 8)
    expected[3 * segment + 5] = 1
    expected[3 * segment + 6] = 2
    expected[3 * segment + 7] = 3
    assertArrayEquals(expected, SealedMedia.readAll(file, key))
  }

  @Test
  fun truncatingASeekableFileForgetsWhatWasCut() {
    val first = Random(31).nextBytes(3 * segment + 10)
    for (keep in listOf(0, 7, segment, segment + 7, 3 * segment)) {
      val file = seekable {
        write(ByteBuffer.wrap(first))
        truncate(keep.toLong())
        assertEquals(keep.toLong(), size)
        assertEquals(keep.toLong(), position)
        seek(keep + 20L)
        write(ByteBuffer.wrap(ByteArray(2) { 5 }))
      }
      val expected = first.copyOf(keep) + ByteArray(20) + ByteArray(2) { 5 }
      assertArrayEquals("kept $keep", expected, SealedMedia.readAll(file, key))
    }
  }

  @Test
  fun aSeekableFileThatWasNotClosedDoesNotOpen() {
    val file = Files.createTempFile("sealed", ".bin").toFile().apply { deleteOnExit() }
    val writer = SealedMedia.SeekableWriter(file, key, segmentSize = segment)
    writer.write(ByteBuffer.wrap(ByteArray(3 * segment + 5)))
    // A kill: the writer is just gone, and what it left must not pass for a finished export.
    val copy = file(file.readBytes())
    assertThrows(Exception::class.java) { SealedMedia.readAll(copy, key) }
    writer.close()
  }

  @Test
  fun aSegmentRewrittenWithTheSameBytesIsSealedDifferently() {
    val file = Files.createTempFile("sealed", ".bin").toFile().apply { deleteOnExit() }
    SealedMedia.SeekableWriter(file, key, segmentSize = segment).use { writer ->
      val plain = ByteArray(segment) { 3 }
      writer.write(ByteBuffer.wrap(plain))
      writer.seek(2L * segment)
      writer.write(ByteBuffer.wrap(ByteArray(1)))
      val first = sealedSegment(file, 0)
      writer.seek(0)
      writer.write(ByteBuffer.wrap(plain))
      writer.seek(2L * segment)
      writer.write(ByteBuffer.wrap(ByteArray(1)))
      val second = sealedSegment(file, 0)
      assertNotEquals("the nonce must not repeat", first.copyOf(12).toList(), second.copyOf(12).toList())
      assertNotEquals(first.toList(), second.toList())
    }
  }

  @Test
  fun noPlaintextReachesTheFileOfASeekableWriter() {
    val marker = "teamcraft-plaintext-marker".encodeToByteArray()
    val file = seekable {
      repeat(50) { write(ByteBuffer.wrap(marker)) }
      seek(3)
      write(ByteBuffer.wrap(marker))
    }
    assertFalse(file.readBytes().toList().windowed(marker.size).any { it == marker.toList() })
  }

  @Test
  fun changedMovedOrCutSeekableFilesDoNotOpen() {
    val plain = Random(32).nextBytes(4 * segment + 3)
    val original = seekable { write(ByteBuffer.wrap(plain)) }.readBytes()
    assertArrayEquals(plain, SealedMedia.readAll(file(original), key))
    val sealedLength = segment + 12 + 16
    val flipped = original.copyOf().also { it[SealedMedia.HEADER_LENGTH + sealedLength + 40] = (it[SealedMedia.HEADER_LENGTH + sealedLength + 40] + 1).toByte() }
    val swapped = original.copyOf().also {
      val first = it.copyOfRange(SealedMedia.HEADER_LENGTH, SealedMedia.HEADER_LENGTH + sealedLength)
      System.arraycopy(it, SealedMedia.HEADER_LENGTH + sealedLength, it, SealedMedia.HEADER_LENGTH, sealedLength)
      System.arraycopy(first, 0, it, SealedMedia.HEADER_LENGTH + sealedLength, sealedLength)
    }
    val cut = original.copyOf(SealedMedia.HEADER_LENGTH + sealedLength * 2)
    val header = original.copyOf().also { it[7] = (it[7] + 1).toByte() }
    for (changed in listOf(flipped, swapped, cut, header)) {
      assertThrows(Exception::class.java) { SealedMedia.readAll(file(changed), key) }
    }
    assertThrows(IOException::class.java) { SealedMedia.readAll(file(original), ByteArray(SealedMedia.KEY_LENGTH)) }
  }

  @Test
  fun aSeekableWriterCanStartOverInTheSameFile() {
    val file = seekable { write(ByteBuffer.wrap(ByteArray(3 * segment) { 4 })) }
    SealedMedia.SeekableWriter(file, key, segmentSize = segment).use { it.write(ByteBuffer.wrap(byteArrayOf(1))) }
    assertArrayEquals(byteArrayOf(1), SealedMedia.readAll(file, key))
  }

  private fun seekable(write: SealedMedia.SeekableWriter.() -> Unit): File {
    val file = Files.createTempFile("sealed", ".bin").toFile().apply { deleteOnExit() }
    SealedMedia.SeekableWriter(file, key, segmentSize = segment).use { it.write() }
    return file
  }

  private fun sealedSegment(file: File, index: Long): ByteArray {
    val sealedLength = segment + 12 + 16
    return RandomAccessFile(file, "r").use { input ->
      input.seek(SealedMedia.HEADER_LENGTH + index * sealedLength)
      ByteArray(sealedLength).also(input::readFully)
    }
  }

  private fun sealed(plain: ByteArray): File {
    val out = ByteArrayOutputStream()
    SealedMedia.writer(out, key, segmentSize = segment).use { writer ->
      // Written in uneven pieces, as streams are.
      var position = 0
      var step = 1
      while (position < plain.size) {
        val count = minOf(step, plain.size - position)
        writer.write(plain, position, count)
        position += count
        step = step * 3 % 101 + 1
      }
    }
    return file(out.toByteArray())
  }

  private fun file(bytes: ByteArray): File = Files.createTempFile("sealed", ".bin").toFile().apply {
    deleteOnExit()
    writeBytes(bytes)
  }
}
