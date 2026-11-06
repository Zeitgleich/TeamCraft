package de.zeitgleich.teamcraft.ui.media

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import de.zeitgleich.teamcraft.ui.CropFrame

/**
 * What the user is about to send from the camera or the gallery (#1358): the chosen photos and
 * videos with their edits, the caption and the quality. Plain data, saved to a file while the flow
 * is open so a killed app comes back to the same selection and edits (robustness.md, "Lifecycle").
 */
@Serializable
data class MediaDraft(
  /** The opening of the flow this draft belongs to: a draft of an earlier one is not picked up again. */
  val launchId: String = "",
  val items: List<MediaDraftItem> = emptyList(),
  val currentIndex: Int = 0,
  val caption: String = "",
  val quality: MediaQuality = MediaQuality.Standard,
  val stage: MediaStage = MediaStage.Camera,
  /** The album the gallery shows; `null` is the list of albums. */
  val maybeAlbum: MediaAlbumRef? = null,
  /** Where the review goes back to: the camera or the gallery the items came from. */
  val origin: MediaStage = MediaStage.Camera,
) {
  val maybeCurrent: MediaDraftItem? get() = items.getOrNull(currentIndex)

  /** The item with this ID replaced by `change(item)`. */
  fun updated(id: String, change: (MediaDraftItem) -> MediaDraftItem): MediaDraft =
    copy(items = items.map { if (it.id == id) change(it) else it })

  /** Adds items not chosen yet (the same picture twice is one item) and shows the first new one. */
  fun added(new: List<MediaDraftItem>): MediaDraft {
    val known = items.map { it.uri }.toSet()
    val fresh = new.filter { it.uri !in known }
    if (fresh.isEmpty()) return this
    return copy(items = items + fresh, currentIndex = items.size)
  }

  fun removed(id: String): MediaDraft {
    val index = items.indexOfFirst { it.id == id }
    if (index < 0) return this
    val rest = items.filterNot { it.id == id }
    val shown = if (index < currentIndex) currentIndex - 1 else currentIndex
    return copy(items = rest, currentIndex = shown.coerceIn(0, maxOf(rest.size - 1, 0)))
  }
}

/** One photo or video: where it is, and what the user did to it. */
@Serializable
data class MediaDraftItem(
  val id: String,
  /** A `content://` URI from the gallery, or a `file://` URI of a photo or video the camera took. */
  val uri: String,
  val isVideo: Boolean,
  val durationMs: Long = 0,
  /** Taken with the app's camera into its own folder: deleted when the flow ends. */
  val isCaptured: Boolean = false,
  val picture: PictureEdit = PictureEdit(),
  val video: VideoEdit = VideoEdit(),
)

/** Where the flow is. */
@Serializable
enum class MediaStage { Camera, Gallery, Review, Crop, Draw }

/** An album of the device's gallery. */
@Serializable
data class MediaAlbumRef(val kind: MediaAlbumKind, val maybeBucketId: Long? = null, val name: String = "")

@Serializable
enum class MediaAlbumKind { Camera, All, Folder }

/**
 * How the pictures and videos are sent: Standard scales and recompresses them (less data, faster),
 * High keeps more of the original. One choice per send; the default is a setting.
 */
@Serializable
enum class MediaQuality { Standard, High }

/**
 * A picture's edits. Marks are in the upright original's own units (1 = its longer side), so they
 * stay put when the crop changes and don't depend on the size the picture is decoded at.
 */
@Serializable
data class PictureEdit(
  val crop: CropEdit = CropEdit(),
  val marks: List<Mark> = emptyList(),
  val isBlurringFaces: Boolean = false,
  /** Faces the device found, as centre x, centre y and radius triples in units; `null` until looked. */
  val maybeFaces: List<Float>? = null,
) {
  val isEdited: Boolean get() = !crop.isNeutral || marks.isNotEmpty() || isBlurringFaces
}

/**
 * Crop and turn, in the order they apply: quarter turns, the mirror, the straightening (which
 * zooms in just enough to leave no empty corners), then the frame in shares of the turned picture.
 */
@Serializable
data class CropEdit(
  val quarterTurnsLeft: Int = 0,
  val isMirrored: Boolean = false,
  val straightenDegrees: Float = 0f,
  val frame: CropFrame = CropFrame(),
  val isAspectLocked: Boolean = false,
) {
  val isNeutral: Boolean get() = quarterTurnsLeft.mod(4) == 0 && !isMirrored && straightenDegrees == 0f && frame.isWhole

  /** The view turned a quarter to the left: with the mirror on, the picture underneath turns the other way. */
  fun turnedLeft(): CropEdit =
    copy(quarterTurnsLeft = (quarterTurnsLeft + if (isMirrored) -1 else 1).mod(4), frame = frame.turnedLeft())

  /** The view mirrored left to right: the straightening reverses with it. */
  fun mirrored(): CropEdit = copy(isMirrored = !isMirrored, straightenDegrees = -straightenDegrees, frame = frame.mirrored())
}

/** Something drawn on a picture. */
@Serializable
sealed interface Mark

/** A line drawn with a finger: points as x, y pairs; the width in units. */
@Serializable
@SerialName("stroke")
data class StrokeMark(val tool: StrokeTool, val color: Int, val width: Float, val points: List<Float>) : Mark

/**
 * Text or a sticker placed on the picture, at its centre `x`, `y` with `size` (text height or
 * sticker side) in units. `upright` undoes the view's turn and mirror at the time it was placed
 * (a 2 × 2 matrix, row by row), so it reads upright where the user put it.
 */
@Serializable
@SerialName("text")
data class TextMark(
  val text: String,
  val color: Int,
  val x: Float,
  val y: Float,
  val size: Float,
  val upright: List<Float> = IDENTITY,
) : Mark

@Serializable
@SerialName("sticker")
data class StickerMark(
  val sticker: Sticker,
  val color: Int,
  val x: Float,
  val y: Float,
  val size: Float,
  val upright: List<Float> = IDENTITY,
) : Mark

@Serializable
enum class StrokeTool { Pen, Highlighter, Blur }

/** Shapes to point at things in a picture (no emoji: those are typed as text). */
@Serializable
enum class Sticker { Arrow, Circle, Square, Check, Star, Heart, Question }

/** A video's edits: the part to send and whether its sound goes along. */
@Serializable
data class VideoEdit(val startMs: Long = 0, val maybeEndMs: Long? = null, val isMuted: Boolean = false) {
  fun isEdited(durationMs: Long): Boolean = startMs > 0 || (maybeEndMs != null && maybeEndMs < durationMs) || isMuted

  fun endMs(durationMs: Long): Long = maybeEndMs ?: durationMs
}

private val IDENTITY = listOf(1f, 0f, 0f, 1f)
