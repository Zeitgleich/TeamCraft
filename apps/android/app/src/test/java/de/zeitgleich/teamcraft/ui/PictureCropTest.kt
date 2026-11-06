package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PictureCropTest {
  @Test
  fun turningLeftFourTimesGivesTheSameFrame() {
    val frame = CropFrame(0.1f, 0.2f, 0.5f, 0.9f)
    assertFrame(frame, frame.turnedLeft().turnedLeft().turnedLeft().turnedLeft())
  }

  @Test
  fun aFrameInTheTopLeftIsInTheBottomLeftAfterATurnLeft() {
    // The top-left quarter of a picture is its bottom-left quarter once it turned counter-clockwise.
    assertFrame(CropFrame(0f, 0.5f, 0.5f, 1f), CropFrame(0f, 0f, 0.5f, 0.5f).turnedLeft())
  }

  @Test
  fun theOriginalFrameUndoesTheTurns() {
    val frame = CropFrame(0.1f, 0.2f, 0.5f, 0.9f)
    for (turns in 0..5) {
      var shown = frame
      repeat(turns) { shown = shown.turnedLeft() }
      assertFrame(frame, shown.inOriginal(turns))
    }
  }

  @Test
  fun aCornerNeverFoldsTheFrameOrLeavesThePicture() {
    val frame = CropFrame().dragged(CropCorner.TopLeft, 2f, -1f)
    assertEquals(0f, frame.top)
    assertEquals(1f - MIN_CROP_SHARE, frame.left, 0.0001f)
    val other = CropFrame().dragged(CropCorner.BottomRight, -0.95f, 0.3f)
    assertEquals(MIN_CROP_SHARE, other.width, 0.0001f)
    assertEquals(1f, other.bottom)
  }

  @Test
  fun movingKeepsTheSizeInsideThePicture() {
    val frame = CropFrame(0.2f, 0.2f, 0.6f, 0.5f).moved(0.9f, -0.9f)
    assertEquals(0.4f, frame.width, 0.0001f)
    assertEquals(0.3f, frame.height, 0.0001f)
    assertEquals(1f, frame.right, 0.0001f)
    assertEquals(0f, frame.top, 0.0001f)
  }

  @Test
  fun theWholePictureIsNoCrop() {
    assertTrue(CropFrame().isWhole)
  }

  private fun assertFrame(expected: CropFrame, actual: CropFrame) {
    assertEquals(expected.left, actual.left, 0.0001f)
    assertEquals(expected.top, actual.top, 0.0001f)
    assertEquals(expected.right, actual.right, 0.0001f)
    assertEquals(expected.bottom, actual.bottom, 0.0001f)
  }
}
