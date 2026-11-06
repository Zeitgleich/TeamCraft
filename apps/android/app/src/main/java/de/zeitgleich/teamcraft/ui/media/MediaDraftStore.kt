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

    /** `StoragePaths.storeDir` and the core's `UPLOAD_STAGING_DIR`. */
    private const val STORES = "store"
    private const val OUTGOING = "outgoing"

    /** A draft nobody came back to for this long is dropped at the next start. */
    private const val ABANDONED_MILLIS = 7L * 24 * 60 * 60 * 1000
    private val json = Json { ignoreUnknownKeys = true }

    /**
     * Every draft of the device and the camera's files of every account: the session ended. Also
     * on an expired or revoked token, where the core's own wipe of its staging folder doesn't run.
     */
    fun deleteAll(context: Context) = deleteAll(context.filesDir)

    /** Drafts left for a week (a flow the app never came back to), those of older versions, and
     * camera folders whose draft is gone. */
    fun sweep(context: Context, nowMillis: Long) = sweep(context.filesDir, nowMillis)

    /**
     * Drafts of conversations no longer listed – deleted here (e2ee.md §4.6), deleted elsewhere,
     * or left – with their camera files: they could never be sent, and what they hold of the
     * conversation goes with it (#1161). [listed] is the list just read, never empty.
     */
    fun dropUnlisted(context: Context, listed: Set<String>) = dropUnlisted(context.filesDir, listed)

    internal fun deleteAll(filesDir: File) {
      File(filesDir, DIRECTORY).deleteRecursively()
      captureFolders(filesDir).forEach { it.deleteRecursively() }
    }

    internal fun sweep(filesDir: File, nowMillis: Long) {
      val drafts = File(filesDir, DIRECTORY)
      drafts.listFiles()?.forEach { file ->
        val isOld = nowMillis - file.lastModified() > ABANDONED_MILLIS
        val isUnsealed = file.name.endsWith(".json") || file.name.endsWith(".part") || file.isDirectory
        if (isOld || isUnsealed) file.deleteRecursively()
      }
      dropOrphanedCaptures(filesDir)
    }

    internal fun dropUnlisted(filesDir: File, listed: Set<String>) {
      if (listed.isEmpty()) return
      File(filesDir, DIRECTORY).listFiles()?.forEach { file ->
        // `draft-{conversation}-{thread}.sealed` and its `.key` (keys as [MediaFlow] names them).
        val conversation = file.name.removePrefix("draft-").substringBefore('-')
        val isDraft = file.name.startsWith("draft-") && conversation.isNotEmpty() && conversation.all(Char::isDigit)
        if (isDraft && conversation !in listed) file.delete()
      }
      dropOrphanedCaptures(filesDir)
    }

    /** The core's sweep of its staging folder skips folders, so these are this store's to remove. */
    private fun dropOrphanedCaptures(filesDir: File) {
      val drafts = File(filesDir, DIRECTORY)
      captureFolders(filesDir)
        .filter { folder -> !File(drafts, "draft-${folder.name.removePrefix("$CAPTURES-")}.sealed").exists() }
        .forEach { it.deleteRecursively() }
    }

    /** `store/<server>/outgoing/<account>/media-flow-captures-<key>`: the core's staging folders. */
    private fun captureFolders(filesDir: File): List<File> =
      File(filesDir, STORES).listFiles().orEmpty()
        .flatMap { server -> File(server, OUTGOING).listFiles().orEmpty().toList() }
        .flatMap { account -> account.listFiles { file -> file.isDirectory && file.name.startsWith("$CAPTURES-") }.orEmpty().toList() }
  }
}
