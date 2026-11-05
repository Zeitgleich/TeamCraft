package de.zeitgleich.teamcraft.ui

import java.io.File
import java.io.InputStream
import java.util.UUID
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** A share nobody picked a conversation for goes after this long (plaintext at rest). */
private const val SHARE_KEPT_MS = 7L * 24 * 60 * 60 * 1000

/** A share without its list was cut short by a kill; its intent is delivered again long before this. */
private const val UNFINISHED_KEPT_MS = 60L * 60 * 1000

private const val LIST_FILE = "share.json"

/**
 * A share copied into the app's own storage, waiting for a conversation (issue #208). A URI grant
 * ends with the task that got it, so the files are copied as soon as they arrive: a share survives
 * a killed process, the sign-in and the app lock, and nothing of it leaves the device before the
 * user pressed send.
 */
@Serializable
data class StagedShare(
  val id: String,
  val text: String,
  val files: List<StagedShareFile>,
  /** Files that couldn't be taken: too many, too large, unreadable or not another app's. */
  val leftOut: Int,
  val maybeTarget: String? = null,
) {
  val isEmpty: Boolean get() = text.isBlank() && files.isEmpty()
}

/** One copied file. On disk it has a number for a name: a sender's name never becomes a path. */
@Serializable
data class StagedShareFile(val name: String, val mediaType: String, val storedAs: String)

/** A file to copy; [open] answers null (or throws) when it can't be read. */
class ShareSource(val name: String, val mediaType: String, val open: () -> InputStream?)

/**
 * The shares waiting on this device, oldest first, one folder each under [root] (`noBackupFilesDir`:
 * private to the app, never in a backup). The list is written last, so a folder without one is a
 * copy that was cut short.
 */
class ShareInbox(private val root: File, private val maxFileBytes: Long) {
  private val json = Json { ignoreUnknownKeys = true }

  /** Copies a share's files and keeps it; files that fail count as left out. */
  fun stage(content: SharedContent, sources: List<ShareSource>, nowMs: Long): StagedShare {
    // The time first, so the folders sort by age.
    val id = "${nowMs.toString().padStart(16, '0')}-${UUID.randomUUID()}"
    val folder = File(root, id)
    folder.mkdirs()
    val files = sources.mapIndexedNotNull { index, source -> copy(source, folder, index.toString()) }
    val share = StagedShare(
      id = id,
      text = content.text,
      files = files,
      leftOut = content.leftOut + sources.size - files.size,
      maybeTarget = content.maybeTarget,
    )
    val list = File(folder, "$LIST_FILE.tmp")
    list.writeText(json.encodeToString(StagedShare.serializer(), share))
    list.renameTo(File(folder, LIST_FILE))
    return share
  }

  /** The oldest share not in [except], or none. */
  fun oldest(except: Set<String> = emptySet()): StagedShare? =
    folders().filterNot { it.name in except }.firstNotNullOfOrNull(::read)

  /** Where the copy of [file] is. */
  fun fileOf(share: StagedShare, file: StagedShareFile): File = File(File(root, share.id), file.storedAs)

  /** The share was handled. The ID comes from a list this class wrote. */
  fun delete(id: String) {
    File(root, id).deleteRecursively()
  }

  /** Everything goes (sign-out): shared content doesn't outlive the session on this device. */
  fun wipe() {
    root.deleteRecursively()
  }

  /** Drops shares past their time and copies that were cut short. */
  fun prune(nowMs: Long) {
    for (folder in folders()) {
      val startedMs = folder.name.substringBefore('-').toLongOrNull() ?: 0
      val age = nowMs - startedMs
      if (age > SHARE_KEPT_MS || (read(folder) == null && age > UNFINISHED_KEPT_MS)) folder.deleteRecursively()
    }
  }

  private fun folders(): List<File> =
    root.listFiles().orEmpty().filter { it.isDirectory }.sortedBy { it.name }

  private fun read(folder: File): StagedShare? =
    runCatching { json.decodeFromString(StagedShare.serializer(), File(folder, LIST_FILE).readText()) }.getOrNull()

  private fun copy(source: ShareSource, folder: File, storedAs: String): StagedShareFile? {
    val target = File(folder, storedAs)
    val isCopied = runCatching {
      source.open()?.use { input -> target.outputStream().use { copyAtMost(input, it, maxFileBytes) } } ?: false
    }.getOrDefault(false)
    if (!isCopied) {
      target.delete()
      return null
    }
    return StagedShareFile(source.name, source.mediaType, storedAs)
  }
}
