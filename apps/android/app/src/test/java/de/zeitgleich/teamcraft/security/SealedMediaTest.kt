package de.zeitgleich.teamcraft.security

import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
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
    val opened = File(directory, "open.mp4")
    SealedMedia.openTo(target, key, opened)
    assertArrayEquals(plain, opened.readBytes())
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
