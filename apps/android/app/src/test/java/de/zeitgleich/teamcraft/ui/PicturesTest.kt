package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class PicturesTest {
  @Test
  fun samplingKeepsTheLongerSideAtLeastTheWantedSize() {
    assertEquals(1, sampleSize(640, 480, 1024))
    assertEquals(1, sampleSize(4000, 3000, 2001))
    assertEquals(2, sampleSize(4000, 3000, 2000))
    assertEquals(4, sampleSize(3000, 4000, 1000))
    assertEquals(1, sampleSize(0, 0, 100))
  }
}
