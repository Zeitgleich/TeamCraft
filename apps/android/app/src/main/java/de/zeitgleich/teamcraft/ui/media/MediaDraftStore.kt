package de.zeitgleich.teamcraft.ui.media

import android.content.Context
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json

/**
 * Keeps the open media flow on disk, so a killed app comes back to the same selection and edits
 * (robustness.md, "Lifecycle"). A file and not the saved instance state: drawings can be larger
 * than a bundle may be. Photos and videos taken with the app's camera wait in a folder next to it,
 * in the app's private storage, and go when the flow ends.
 */
class MediaDraftStore(context: Context) {
  private val directory = File(context.filesDir, DIRECTORY)

  /** Where the camera writes for this flow. */
  fun capturesDirectory(key: String): File = File(directory, "$CAPTURES-${safe(key)}").apply { mkdirs() }

  suspend fun load(key: String): MediaDraft? = withContext(Dispatchers.IO) {
    val file = fileOf(key)
    if (!file.exists()) return@withContext null
    runCatching { json.decodeFromString(MediaDraft.serializer(), file.readText()) }.getOrNull()
  }

  suspend fun save(key: String, draft: MediaDraft) = withContext(Dispatchers.IO) {
    directory.mkdirs()
    val target = fileOf(key)
    // Written beside and renamed, so a kill while writing leaves the previous state.
    val partial = File(directory, "${target.name}.part")
    runCatching {
      partial.writeText(json.encodeToString(MediaDraft.serializer(), draft))
      partial.renameTo(target)
    }
  }

  /** The flow ended (sent or closed): its state and the camera's files go. */
  suspend fun clear(key: String) = withContext(Dispatchers.IO) {
    fileOf(key).delete()
    File(directory, "$CAPTURES-${safe(key)}").deleteRecursively()
  }

  private fun fileOf(key: String): File = File(directory, "draft-${safe(key)}.json")

  private fun safe(key: String): String = key.filter { it.isLetterOrDigit() || it == '-' }

  companion object {
    private const val DIRECTORY = "media-flow"
    private const val CAPTURES = "captures"
    private val json = Json { ignoreUnknownKeys = true }
  }
}
