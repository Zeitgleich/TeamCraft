package de.zeitgleich.teamcraft.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AvatarCropTest {
  @Test
  fun atZoomOneTheShorterSideFillsTheCircleInTheMiddle() {
    assertEquals(CropSquare(left = 150, top = 0, side = 300), AvatarCrop(600, 300).square())
    assertEquals(CropSquare(left = 0, top = 150, side = 300), AvatarCrop(300, 600).square())
  }

  @Test
  fun theSquareNeverLeavesThePicture() {
    val dragged = AvatarCrop(600, 300).dragged(dx = -10_000f, dy = 10_000f, viewSide = 300f)
    assertEquals(CropSquare(left = 300, top = 0, side = 300), dragged.square())
    val zoomed = AvatarCrop(600, 300).withZoom(2f).dragged(dx = 10_000f, dy = 10_000f, viewSide = 300f)
    assertEquals(CropSquare(left = 0, top = 0, side = 150), zoomed.square())
  }

  @Test
  fun thePictureFollowsTheFinger() {
    // Dragging right shows more of the left: the square moves left, scaled from view to picture.
    val moved = AvatarCrop(600, 300).dragged(dx = 50f, dy = 0f, viewSide = 150f)
    assertEquals(150 - 100, moved.square().left)
  }

  @Test
  fun zoomStaysWithinItsLimits() {
    val crop = AvatarCrop(960, 960)
    assertEquals(5f, AvatarCrop(480, 600).maxZoom, 0.01f)
    assertEquals(AvatarCrop.MAX_ZOOM, AvatarCrop(4000, 4000).maxZoom, 0.01f)
    assertEquals(1f, AvatarCrop(50, 80).maxZoom, 0.01f)
    assertEquals(1f, crop.withZoom(0.2f).zoom, 0.01f)
    assertTrue(crop.withZoom(100f).square().side >= AvatarCrop.MIN_SIDE_PX.toInt())
  }
}
