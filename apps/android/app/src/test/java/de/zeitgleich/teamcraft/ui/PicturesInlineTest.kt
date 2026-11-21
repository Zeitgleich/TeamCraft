package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class PicturesInlineTest {
  @Test
  fun `avif is drawn from the original from Android 12`() {
    assertEquals(false, canDecodeHere("image/avif", sdk = 30))
    assertEquals(true, canDecodeHere("image/avif", sdk = 31))
  }

  @Test
  fun `heic is drawn from the original from Android 9`() {
    assertEquals(false, canDecodeHere("image/heic", sdk = 27))
    assertEquals(true, canDecodeHere("image/HEIF", sdk = 28))
  }

  @Test
  fun `media type parameters and case do not matter`() {
    assertEquals(true, canDecodeHere(" Image/WebP; q=1 ", sdk = 28))
  }

  @Test
  fun `formats no decoder here reads stay file cards`() {
    for (type in listOf("image/jxl", "image/tiff", "image/x-icon", "image/svg+xml", "application/pdf")) {
      assertEquals(type, false, showsAsPicture(type, hasThumbnail = false, isUploading = false, sdk = 36))
    }
  }

  @Test
  fun `a thumbnail or an upload always makes a picture`() {
    assertEquals(true, showsAsPicture("image/jxl", hasThumbnail = true, isUploading = false, sdk = 36))
    assertEquals(true, showsAsPicture("image/jxl", hasThumbnail = false, isUploading = true, sdk = 36))
  }

  @Test
  fun `a picture without a thumbnail shows when the phone can decode it`() {
    assertEquals(true, showsAsPicture("image/avif", hasThumbnail = false, isUploading = false, sdk = 35))
    assertEquals(false, showsAsPicture("image/avif", hasThumbnail = false, isUploading = false, sdk = 30))
  }

  @Test
  fun `non pictures never show as pictures`() {
    assertEquals(false, showsAsPicture("video/mp4", hasThumbnail = true, isUploading = false, sdk = 36))
  }

  @Test
  fun `senders make the thumbnail of what the core cannot decode`() {
    assertEquals(true, needsPictureThumbnail("image/avif", sdk = 33))
    assertEquals(true, needsPictureThumbnail("image/heic", sdk = 33))
    assertEquals(false, needsPictureThumbnail("image/avif", sdk = 30))
    assertEquals(false, needsPictureThumbnail("image/png", sdk = 33))
  }
}
