package de.zeitgleich.teamcraft.ui.media

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class CapturedJpegTest {
  @Test
  fun theCamerasExifGoesAndOnlyTheOrientationIsWritten() {
    val exif = segment(0xE1, "Exif\u0000\u0000MM camera make, model, time".encodeToByteArray())
    val xmp = segment(0xE1, "http://ns.adobe.com/xap/1.0/ place".encodeToByteArray())
    val quantization = segment(0xDB, ByteArray(10) { 1 })
    val scan = bytes(0xFF, 0xDA, 0x00, 0x04, 0x01, 0x02) + ByteArray(20) { 0x33 } + bytes(0xFF, 0xD9)
    val jpeg = bytes(0xFF, 0xD8) + exif + xmp + quantization + scan
    val result = CapturedJpeg.withOrientation(jpeg, rotationDegrees = 90, isMirrored = false)
    val text = String(result, Charsets.ISO_8859_1)
    assertFalse(text.contains("camera make"))
    assertFalse(text.contains("ns.adobe.com"))
    assertArrayEquals(bytes(0xFF, 0xD8, 0xFF, 0xE1, 0x00, 34), result.copyOf(6))
    assertEquals(6, orientationOf(result))
    // The rest of the picture is kept as it was.
    assertArrayEquals(quantization + scan, result.copyOfRange(2 + 36, result.size))
  }

  @Test
  fun everyTurnAndMirrorHasItsOrientation() {
    val expected = mapOf(
      (0 to false) to 1, (90 to false) to 6, (180 to false) to 3, (270 to false) to 8,
      (0 to true) to 2, (90 to true) to 5, (180 to true) to 4, (270 to true) to 7,
    )
    expected.forEach { (turn, orientation) -> assertEquals("$turn", orientation, CapturedJpeg.orientation(turn.first, turn.second)) }
    assertEquals(8, CapturedJpeg.orientation(-90, false))
    assertEquals(1, CapturedJpeg.orientation(360, false))
  }

  @Test
  fun aJpegWithoutExifGetsOne() {
    val jpeg = bytes(0xFF, 0xD8) + segment(0xE0, "JFIF\u0000".encodeToByteArray()) + bytes(0xFF, 0xDA, 0x00, 0x02, 0x55, 0xFF, 0xD9)
    val result = CapturedJpeg.withOrientation(jpeg, rotationDegrees = 270, isMirrored = true)
    assertEquals(7, orientationOf(result))
    assertTrue(String(result, Charsets.ISO_8859_1).contains("JFIF"))
  }

  @Test
  fun somethingElseIsRefused() {
    assertThrows(IllegalArgumentException::class.java) { CapturedJpeg.withOrientation("not a picture".encodeToByteArray(), 0, false) }
    // A segment longer than the data.
    assertThrows(IllegalArgumentException::class.java) { CapturedJpeg.withOrientation(bytes(0xFF, 0xD8, 0xFF, 0xE0, 0x10, 0x00, 0x01), 0, false) }
  }

  /** The Orientation value of the EXIF segment right after SOI (the layout CapturedJpeg writes). */
  private fun orientationOf(jpeg: ByteArray): Int {
    assertArrayEquals("Exif\u0000\u0000MM".encodeToByteArray(), jpeg.copyOfRange(6, 14))
    // SOI 2, marker and length 4, "Exif\0\0" 6, TIFF header 8, count 2: the entry's tag at 22.
    assertArrayEquals(bytes(0x01, 0x12, 0x00, 0x03), jpeg.copyOfRange(22, 26))
    return jpeg[31].toInt()
  }

  private fun segment(marker: Int, payload: ByteArray): ByteArray {
    val length = payload.size + 2
    return bytes(0xFF, marker, length shr 8, length and 0xFF) + payload
  }

  private fun bytes(vararg values: Int): ByteArray = ByteArray(values.size) { values[it].toByte() }
}
