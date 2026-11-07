package de.zeitgleich.teamcraft.ui.media

import java.io.ByteArrayOutputStream

/**
 * A photo of the media flow's camera, taken into memory (#1388) and not into a file: CameraX then
 * leaves turning and mirroring to the app. The JPEG keeps its pixels and gets a new EXIF block
 * that holds only the orientation; the camera's own (make, model, time, a thumbnail) and any XMP
 * are dropped, so the sealed file holds no more than the picture.
 */
internal object CapturedJpeg {
  /**
   * [jpeg] with its APP1 segments (EXIF, XMP) replaced by one EXIF segment saying how to show it:
   * turned by [rotationDegrees] clockwise, then mirrored when [isMirrored] (the front camera, as
   * its preview shows). Throws [IllegalArgumentException] for data that isn't a JPEG.
   */
  fun withOrientation(jpeg: ByteArray, rotationDegrees: Int, isMirrored: Boolean): ByteArray {
    require(jpeg.size >= 4 && byte(jpeg, 0) == 0xFF && byte(jpeg, 1) == SOI) { "not a JPEG" }
    val out = ByteArrayOutputStream(jpeg.size + EXIF_SEGMENT_LENGTH)
    out.write(jpeg, 0, 2)
    out.write(exifSegment(orientation(rotationDegrees, isMirrored)))
    var position = 2
    while (true) {
      require(position + 1 < jpeg.size && byte(jpeg, position) == 0xFF) { "broken JPEG segment" }
      // Fill bytes before a marker.
      var markerAt = position + 1
      while (markerAt < jpeg.size && byte(jpeg, markerAt) == 0xFF) markerAt += 1
      require(markerAt < jpeg.size) { "broken JPEG marker" }
      val marker = byte(jpeg, markerAt)
      if (marker == SOS || marker == EOI) {
        // The image data and what follows go as they are.
        out.write(jpeg, position, jpeg.size - position)
        return out.toByteArray()
      }
      if (marker == TEM || marker in RST0..RST7) {
        out.write(jpeg, position, markerAt + 1 - position)
        position = markerAt + 1
        continue
      }
      require(markerAt + 2 < jpeg.size) { "broken JPEG length" }
      val length = (byte(jpeg, markerAt + 1) shl 8) or byte(jpeg, markerAt + 2)
      val end = markerAt + 1 + length
      require(length >= 2 && end <= jpeg.size) { "broken JPEG length" }
      if (marker != APP1) out.write(jpeg, position, end - position)
      position = end
    }
  }

  /** The EXIF orientation (1 to 8) for a turn clockwise and then a horizontal mirror. */
  fun orientation(rotationDegrees: Int, isMirrored: Boolean): Int = when (((rotationDegrees % 360) + 360) % 360) {
    90 -> if (isMirrored) 5 else 6
    180 -> if (isMirrored) 4 else 3
    270 -> if (isMirrored) 7 else 8
    else -> if (isMirrored) 2 else 1
  }

  /** `APP1 "Exif\0\0"` with a big-endian TIFF header and one IFD holding only Orientation. */
  private fun exifSegment(orientation: Int): ByteArray = byteArrayOf(
    0xFF.toByte(), APP1.toByte(), 0, (EXIF_SEGMENT_LENGTH - 2).toByte(),
    'E'.code.toByte(), 'x'.code.toByte(), 'i'.code.toByte(), 'f'.code.toByte(), 0, 0,
    // TIFF header: "MM", 42, first IFD at 8.
    'M'.code.toByte(), 'M'.code.toByte(), 0, 42, 0, 0, 0, 8,
    // One entry: Orientation (0x0112), SHORT, count 1, the value and two bytes of padding.
    0, 1,
    0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, orientation.toByte(), 0, 0,
    // No next IFD.
    0, 0, 0, 0,
  )

  private fun byte(data: ByteArray, index: Int): Int = data[index].toInt() and 0xFF

  private const val SOI = 0xD8
  private const val EOI = 0xD9
  private const val SOS = 0xDA
  private const val APP1 = 0xE1
  private const val TEM = 0x01
  private const val RST0 = 0xD0
  private const val RST7 = 0xD7
  private const val EXIF_SEGMENT_LENGTH = 36
}
