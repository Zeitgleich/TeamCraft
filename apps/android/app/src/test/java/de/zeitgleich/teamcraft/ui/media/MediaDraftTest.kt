package de.zeitgleich.teamcraft.ui.media

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import de.zeitgleich.teamcraft.ui.CropCorner
import de.zeitgleich.teamcraft.ui.CropFrame

class MediaDraftTest {
  @Test
  fun theSamePictureIsAddedOnce() {
    val draft = MediaDraft().added(listOf(item("a"), item("b"))).added(listOf(item("b"), item("c")))
    assertEquals(listOf("a", "b", "c"), draft.items.map { it.uri })
    // The first new one is shown.
    assertEquals(2, draft.currentIndex)
  }

  @Test
  fun removingAnItemBeforeTheShownOneKeepsShowingIt() {
    val draft = MediaDraft(items = listOf(item("a"), item("b"), item("c")), currentIndex = 2)
    val removed = draft.removed("a")
    assertEquals("c", removed.maybeCurrent?.uri)
  }

  @Test
  fun removingTheLastItemShowsTheOneBefore() {
    val draft = MediaDraft(items = listOf(item("a"), item("b")), currentIndex = 1)
    assertEquals("a", draft.removed("b").maybeCurrent?.uri)
    assertEquals(0, draft.removed("b").removed("a").currentIndex)
  }

  @Test
  fun aDraftSurvivesItsFile() {
    // Process death: the draft is written as JSON and read back with every edit.
    val draft = MediaDraft(
      launchId = "launch",
      items = listOf(
        item("a").copy(
          picture = PictureEdit(
            crop = CropEdit(quarterTurnsLeft = 1, isMirrored = true, straightenDegrees = 4.5f, frame = CropFrame(0.1f, 0.2f, 0.9f, 0.8f)),
            marks = listOf(
              StrokeMark(StrokeTool.Blur, 0x7f112233, 0.05f, listOf(0.1f, 0.2f, 0.3f, 0.4f)),
              TextMark("Hello", -1, 0.5f, 0.5f, 0.06f),
              StickerMark(Sticker.Arrow, -16777216, 0.2f, 0.3f, 0.1f, listOf(0f, -1f, 1f, 0f)),
            ),
            isBlurringFaces = true,
            maybeFaces = listOf(0.4f, 0.3f, 0.05f),
          ),
        ),
        item("b").copy(isVideo = true, durationMs = 9000, video = VideoEdit(1000, 5000, isMuted = true)),
      ),
      caption = "Look",
      quality = MediaQuality.High,
      stage = MediaStage.Draw,
    )
    val json = Json.encodeToString(MediaDraft.serializer(), draft)
    assertEquals(draft, Json.decodeFromString(MediaDraft.serializer(), json))
  }

  @Test
  fun anUnchangedPictureIsNotEdited() {
    assertFalse(PictureEdit().isEdited)
    assertTrue(PictureEdit(crop = CropEdit(straightenDegrees = 1f)).isEdited)
  }

  @Test
  fun aVideoIsEditedWhenTrimmedOrMuted() {
    assertFalse(VideoEdit().isEdited(10_000))
    assertFalse(VideoEdit(maybeEndMs = 10_000).isEdited(10_000))
    assertTrue(VideoEdit(startMs = 1).isEdited(10_000))
    assertTrue(VideoEdit(isMuted = true).isEdited(10_000))
  }

  @Test
  fun turningTheViewLeftWhileMirroredTurnsThePictureRight() {
    assertEquals(1, CropEdit().turnedLeft().quarterTurnsLeft)
    assertEquals(3, CropEdit(isMirrored = true).turnedLeft().quarterTurnsLeft)
  }

  @Test
  fun mirroringTwiceGivesTheSameCrop() {
    val crop = CropEdit(straightenDegrees = 7f, frame = CropFrame(0.1f, 0.2f, 0.5f, 0.9f))
    val back = crop.mirrored().mirrored()
    assertEquals(crop.straightenDegrees, back.straightenDegrees)
    assertEquals(crop.isMirrored, back.isMirrored)
    assertFrame(crop.frame, back.frame)
    assertFrame(CropFrame(0.5f, 0.2f, 0.9f, 0.9f), crop.mirrored().frame)
  }

  @Test
  fun noStraighteningNeedsNoZoom() {
    assertEquals(1f, PictureGeometry.coverScale(4000f, 3000f, 0f))
  }

  @Test
  fun aStraightenedSquareZoomsByTheDiagonal() {
    // A square turned 45° covers itself only when its side reaches the diagonal.
    assertEquals(Math.sqrt(2.0).toFloat(), PictureGeometry.coverScale(100f, 100f, 45f), 0.0001f)
    assertEquals(PictureGeometry.coverScale(100f, 100f, 10f), PictureGeometry.coverScale(100f, 100f, -10f), 0.0001f)
  }

  @Test
  fun aWidePictureZoomsMoreThanItsTurnAlone() {
    val scale = PictureGeometry.coverScale(4000f, 1000f, 5f)
    // The short side has to cover the long side's turn: far more than cos(5°) would suggest.
    assertTrue(scale > 1.3f)
  }

  @Test
  fun theOutputIsTheFrameOfTheTurnedPicture() {
    val crop = CropEdit(quarterTurnsLeft = 1, frame = CropFrame(0f, 0f, 0.5f, 0.25f))
    assertEquals(1500f to 1000f, PictureGeometry.outputSize(4000f, 3000f, crop))
  }

  @Test
  fun aLockedFrameKeepsItsShape() {
    val frame = CropFrame(0.2f, 0.2f, 0.6f, 0.4f)
    val aspect = frame.width / frame.height
    for (corner in CropCorner.entries) {
      for ((dx, dy) in listOf(0.1f to 0.02f, -0.05f to 0.3f, 0.5f to 0.5f, -0.5f to -0.5f)) {
        val dragged = frame.draggedLocked(corner, dx, dy, aspect)
        assertEquals("$corner $dx $dy", aspect, dragged.width / dragged.height, 0.001f)
        assertTrue("$corner $dx $dy inside", dragged.left >= -0.0001f && dragged.top >= -0.0001f && dragged.right <= 1.0001f && dragged.bottom <= 1.0001f)
      }
    }
  }

  @Test
  fun aLockedFrameKeepsItsOppositeCorner() {
    val frame = CropFrame(0.2f, 0.2f, 0.6f, 0.4f)
    val dragged = frame.draggedLocked(CropCorner.BottomRight, 0.1f, 0f, 2f)
    assertEquals(0.2f, dragged.left, 0.0001f)
    assertEquals(0.2f, dragged.top, 0.0001f)
    assertEquals(0.5f, dragged.width, 0.0001f)
  }

  private fun item(uri: String) = MediaDraftItem(id = "id-$uri", uri = uri, isVideo = false)

  private fun assertFrame(expected: CropFrame, actual: CropFrame) {
    assertEquals(expected.left, actual.left, 0.0001f)
    assertEquals(expected.top, actual.top, 0.0001f)
    assertEquals(expected.right, actual.right, 0.0001f)
    assertEquals(expected.bottom, actual.bottom, 0.0001f)
  }
}
