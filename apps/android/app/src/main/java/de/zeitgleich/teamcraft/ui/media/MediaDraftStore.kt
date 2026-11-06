package de.zeitgleich.teamcraft.ui.media

import android.content.Context
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.SealedCache
import de.zeitgleich.teamcraft.security.SealedFile

/**
 * Keeps the open media flow on disk, so a killed app comes back to the same selection and edits
 * (robustness.md, "Lifecycle"). A file and not the saved instance state: drawings can be larger
 * than a bundle may be. The draft holds the caption and the drawn text, so it is sealed like the
 * database key (e2ee.md §9.2). Photos and videos taken with the app's camera wait in the core's
 * staging folder ([capturesRoot]), which sign-out wipes, and go when the flow ends.
 */
class MediaDraftStore(context: Context, private val capturesRoot: suspend () -> File?) {
  private val directory = File(context.filesDir, DIRECTORY)

  /** Where the camera writes for this flow; `null` while no account is open. */
  suspend fun capturesDirectory(key: String): File? = withContext(Dispatchers.IO) {
    capturesRoot()?.let { File(it, "$CAPTURES-${safe(key)}").apply { mkdirs() } }
  }

  suspend fun load(key: String): MediaDraft? = withContext(Dispatchers.IO) {
    val bytes = runCatching { sealedOf(key).read() }.getOrNull() ?: return@withContext null
    runCatching { json.decodeFromString(MediaDraft.serializer(), bytes.decodeToString()) }.getOrNull()
  }

  suspend fun save(key: String, draft: MediaDraft) = withContext(Dispatchers.IO) {
    // Written beside and renamed (SealedCache), so a kill while writing leaves the previous state.
    runCatching { sealedOf(key).write(json.encodeToString(MediaDraft.serializer(), draft).encodeToByteArray()) }
  }

  /** The flow ended (sent or closed): its state and the camera's files go. */
  suspend fun clear(key: String) = withContext(Dispatchers.IO) {
    sealedOf(key).delete()
    capturesRoot()?.let { File(it, "$CAPTURES-${safe(key)}").deleteRecursively() }
  }

  private fun sealedOf(key: String): SealedCache = SealedCache(
    File(directory, "draft-${safe(key)}.sealed"),
    SealedFile(File(directory, "draft-${safe(key)}.key"), KeystoreBox(KEY_ALIAS)),
  )

  private fun safe(key: String): String = key.filter { it.isLetterOrDigit() || it == '-' }

  companion object {
    private const val DIRECTORY = "media-flow"
    private const val CAPTURES = "media-flow-captures"
    private const val KEY_ALIAS = "teamcraft_media_draft"

    /** A draft nobody came back to for this long is dropped at the next start. */
    private const val ABANDONED_MILLIS = 7L * 24 * 60 * 60 * 1000
    private val json = Json { ignoreUnknownKeys = true }

    /** Every draft of the device: the session ended. The captures go with the core's staging folder. */
    fun deleteAll(context: Context) {
      File(context.filesDir, DIRECTORY).deleteRecursively()
    }

    /** Drafts left for a week (a flow the app never came back to), and those of older versions. */
    fun sweep(context: Context, nowMillis: Long) {
      File(context.filesDir, DIRECTORY).listFiles()?.forEach { file ->
        val isOld = nowMillis - file.lastModified() > ABANDONED_MILLIS
        val isUnsealed = file.name.endsWith(".json") || file.name.endsWith(".part") || file.isDirectory
        if (isOld || isUnsealed) file.deleteRecursively()
      }
    }
  }
}
