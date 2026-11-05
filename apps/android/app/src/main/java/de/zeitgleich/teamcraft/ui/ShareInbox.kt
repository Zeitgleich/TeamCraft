package de.zeitgleich.teamcraft.ui

import java.io.File
import java.io.IOException
import java.io.InputStream
import java.security.MessageDigest
import java.util.UUID
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** A share nobody picked a conversation for goes after this long (plaintext at rest). */
private const val SHARE_KEPT_MS = 7L * 24 * 60 * 60 * 1000

/** A share without its list was cut short by a kill; its intent is delivered again long before this. */
private const val UNFINISHED_KEPT_MS = 60L * 60 * 1000

private const val LIST_FILE = "share.json"

/** Who signed in last; a share waits through a session that ran out, for them only. */
private const val OWNER_FILE = "owner"

/**
 * How long a sign-out or a discard waits for a copy to stop. Cancelling can't interrupt a provider
 * blocked in a read or a query: such a copy finds out later, and leaves nothing ([ShareInbox.stage]).
 */
const val SHARE_COPY_STOP_TIMEOUT_MS = 2_000L

/** Shares that may wait at once; a further one can't be kept. */
const val MAX_WAITING_SHARES = 5

/** Bytes all waiting shares may take; a file that doesn't fit counts as left out. */
const val MAX_INBOX_BYTES = 8L * 1024 * 1024 * 1024

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
  /** Identifies what was shared: the same intent delivered again doesn't make a second share. */
  val sourceKey: String = "",
) {
  val isEmpty: Boolean get() = text.isBlank() && files.isEmpty()
}

/** One copied file. On disk it has a number for a name: a sender's name never becomes a path. */
@Serializable
data class StagedShareFile(val name: String, val mediaType: String, val storedAs: String)

/** What became of a share: kept, stopped on request (nothing stays), or impossible to keep. */
sealed interface ShareStageResult {
  data class Stored(val share: StagedShare) : ShareStageResult
  data object Cancelled : ShareStageResult
  data object Failed : ShareStageResult
}

/** A file to copy; [open] answers null (or throws) when it can't be read. */
class ShareSource(val name: String, val mediaType: String, val open: () -> InputStream?)

/**
 * The shares waiting on this device, oldest first, one folder each under [root] (`noBackupFilesDir`:
 * private to the app, never in a backup). The list is written last, so a folder without one is a
 * copy that was cut short.
 */
class ShareInbox(
  private val root: File,
  private val maxFileBytes: Long,
  private val maxTotalBytes: Long = MAX_INBOX_BYTES,
  private val maxShares: Int = MAX_WAITING_SHARES,
) {
  private val json = Json { ignoreUnknownKeys = true }

  /**
   * Copies a share's files and keeps it; files that fail or don't fit count as left out. Once
   * [isCancelled] says so the copy stops and nothing of it stays. A share that can't be written (full
   * disk) or that is one too many is [ShareStageResult.Failed], with nothing left behind.
   */
  fun stage(
    content: SharedContent,
    sources: List<ShareSource>,
    nowMs: Long,
    isCancelled: () -> Boolean = { false },
  ): ShareStageResult {
    if (isCancelled()) return ShareStageResult.Cancelled
    val sourceKey = sourceKeyOf(content)
    val waiting = folders().mapNotNull(::read)
    waiting.firstOrNull { it.sourceKey == sourceKey }?.let { return ShareStageResult.Stored(it) }
    if (waiting.size >= maxShares) return ShareStageResult.Failed
    // The time first, so the folders sort by age.
    val id = "${nowMs.toString().padStart(16, '0')}-${UUID.randomUUID()}"
    val folder = File(root, id)
    var room = maxTotalBytes - usedBytes()
    return try {
      if (!folder.mkdirs()) throw IOException("the share's folder could not be made")
      val files = ArrayList<StagedShareFile>()
      for ((index, source) in sources.withIndex()) {
        if (isCancelled()) break
        val copied = copy(source, folder, index.toString(), minOf(maxFileBytes, room), isCancelled) ?: continue
        room -= fileOf(id, copied).length()
        files.add(copied)
      }
      // Checked once more right before the list: with it, the share would count as kept.
      if (isCancelled()) {
        folder.deleteRecursively()
        return ShareStageResult.Cancelled
      }
      val share = StagedShare(
        id = id,
        text = content.text,
        files = files,
        leftOut = content.leftOut + sources.size - files.size,
        maybeTarget = content.maybeTarget,
        sourceKey = sourceKey,
      )
      writeList(folder, share)
      // A cancel while the list went out: whoever cancelled may already have wiped and moved on.
      if (isCancelled()) {
        folder.deleteRecursively()
        return ShareStageResult.Cancelled
      }
      ShareStageResult.Stored(share)
    } catch (_: IOException) {
      folder.deleteRecursively()
      ShareStageResult.Failed
    }
  }

  /**
   * The composer took the rest of [share]; [failed] waits on, for another try. The text went with the
   * rest, and the shortcut's conversation isn't picked by itself again.
   */
  fun keepOnly(share: StagedShare, failed: List<StagedShareFile>) {
    val rest = share.copy(text = "", files = failed, leftOut = 0, maybeTarget = null)
    try {
      writeList(File(root, share.id), rest)
    } catch (_: IOException) {
      // The old list stays; it names files already gone, which then fail again and say so.
    }
  }

  /** The oldest share not in [except], or none. */
  fun oldest(except: Set<String> = emptySet()): StagedShare? =
    folders().filterNot { it.name in except }.firstNotNullOfOrNull(::read)

  /** Where the copy of [file] is. */
  fun fileOf(share: StagedShare, file: StagedShareFile): File = fileOf(share.id, file)

  /** The share was handled. The ID comes from a list this class wrote. */
  fun delete(id: String) {
    File(root, id).deleteRecursively()
  }

  /** Everything goes (sign-out): shared content doesn't outlive the session on this device. */
  fun wipe() {
    root.deleteRecursively()
  }

  /**
   * [userId] signed in. What waits from before goes if somebody else signed in last: they would see
   * another person's plaintext. Answers whether a share went that way. After a [wipe] (the user signed
   * out) nobody is the owner, so what is shared until the next sign-in stays for whoever that is.
   */
  fun claim(userId: String): Boolean {
    val owner = File(root, OWNER_FILE)
    val maybeOwner = runCatching { owner.readText() }.getOrNull()
    val isOther = maybeOwner != null && maybeOwner != userId
    val hadShare = isOther && folders().any { read(it) != null }
    if (isOther) wipe()
    runCatching {
      root.mkdirs()
      owner.writeText(userId)
    }
    return hadShare
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

  private fun sourceKeyOf(content: SharedContent): String {
    val source = (content.uris.map { it.toString() } + content.text + content.maybeTarget.orEmpty()).joinToString("\n")
    val digest = MessageDigest.getInstance("SHA-256").digest(source.toByteArray())
    return digest.joinToString("") { "%02x".format(it) }
  }

  private fun fileOf(id: String, file: StagedShareFile): File = File(File(root, id), file.storedAs)

  private fun usedBytes(): Long = root.walkTopDown().filter { it.isFile }.sumOf { it.length() }

  private fun writeList(folder: File, share: StagedShare) {
    val list = File(folder, "$LIST_FILE.tmp")
    list.writeText(json.encodeToString(StagedShare.serializer(), share))
    if (!list.renameTo(File(folder, LIST_FILE))) throw IOException("the share's list could not be written")
  }

  private fun copy(
    source: ShareSource,
    folder: File,
    storedAs: String,
    maxBytes: Long,
    isCancelled: () -> Boolean,
  ): StagedShareFile? {
    val target = File(folder, storedAs)
    val isCopied = runCatching {
      source.open()?.use { input -> target.outputStream().use { copyAtMost(input, it, maxBytes, isCancelled) } } ?: false
    }.getOrDefault(false)
    if (!isCopied) {
      target.delete()
      return null
    }
    return StagedShareFile(source.name, source.mediaType, storedAs)
  }
}

/**
 * Takes shares for all of the app's activities, on the process's scope: an activity that finishes
 * doesn't end a copy, only a discard or a sign-out does.
 */
class ShareReceiver(root: File, maxFileBytes: Long, private val scope: CoroutineScope) {
  val inbox = ShareInbox(root, maxFileBytes)
  private val copies = ArrayList<Copy>()
  private val handedOver = HashSet<String>()
  private val changeCount = MutableStateFlow(0L)
  private val failure = MutableStateFlow(false)

  /** Goes up when a copy ended: the inbox may look different. */
  val changes: StateFlow<Long> = changeCount

  /** A share couldn't be kept (full disk, too many waiting); the user has to be told. */
  val hasFailed: StateFlow<Boolean> = failure

  val isCopying: Boolean get() = synchronized(copies) { copies.any { it.job.isActive } }

  fun isCopying(content: SharedContent): Boolean =
    synchronized(copies) { copies.any { it.content == content && it.job.isActive } }

  fun failureShown() {
    failure.value = false
  }

  /** Copies [content] into the inbox unless that is under way already; [sources] may block. */
  fun receive(content: SharedContent, sources: () -> List<ShareSource>) {
    synchronized(copies) {
      if (copies.any { it.content == content && it.job.isActive }) return
      copies.removeAll { !it.job.isActive }
      val maybePrevious = copies.lastOrNull()?.job
      val job = scope.launch(Dispatchers.IO) {
        maybePrevious?.join()
        val copy = currentCoroutineContext()[Job]
        val isCancelled = { copy?.isActive == false }
        if (isCancelled()) return@launch
        val result = inbox.stage(content, sources(), System.currentTimeMillis(), isCancelled)
        if (result is ShareStageResult.Failed) failure.value = true
      }
      job.invokeOnCompletion { changeCount.update { it + 1 } }
      copies.add(Copy(content, job))
    }
  }

  /**
   * Stops every copy, waiting at most [timeoutMs]: a provider blocked in a read can't be interrupted,
   * and a sign-out must not hang on it. A late copy sees the cancellation and leaves nothing behind.
   */
  suspend fun stopCopies(timeoutMs: Long = SHARE_COPY_STOP_TIMEOUT_MS) {
    val jobs = synchronized(copies) {
      val running = copies.map { it.job }
      copies.clear()
      running
    }
    jobs.forEach { it.cancel() }
    withTimeoutOrNull(timeoutMs) { jobs.joinAll() }
  }

  /** The oldest share not on its way into a composer. */
  fun next(): StagedShare? = inbox.oldest(except = synchronized(handedOver) { handedOver.toSet() })

  fun handOver(share: StagedShare) {
    synchronized(handedOver) { handedOver.add(share.id) }
  }

  fun finished(share: StagedShare) {
    synchronized(handedOver) { handedOver.remove(share.id) }
  }

  fun forgetHandedOver() {
    synchronized(handedOver) { handedOver.clear() }
  }

  private class Copy(val content: SharedContent, val job: Job)
}
