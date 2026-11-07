package de.zeitgleich.teamcraft.ui.media

import android.content.Context
import java.io.File
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.SealedCache
import de.zeitgleich.teamcraft.security.SealedFile
import de.zeitgleich.teamcraft.security.SealedMedia

/** Which conversation (and thread) a media flow sends to: one draft each. */
data class MediaDraftKey(val conversation: String, val maybeThread: String?)

/**
 * Keeps the open media flow on disk, so a killed app comes back to the same selection and edits
 * (robustness.md, "Lifecycle"). A file and not the saved instance state: drawings can be larger
 * than a bundle may be. The draft holds the caption and the drawn text, so it is sealed like the
 * database key (e2ee.md §9.2). Photos and videos taken with the app's camera wait in the core's
 * staging folder ([capturesRoot]), which sign-out wipes, and go when the flow ends. They are sealed
 * there ([SealedCaptures], #1388) with a key derived from the naming secret ([captureKey]).
 *
 * Files and camera folders are named by a keyed derivation ([MediaDraftNames], #1377), so their
 * names show no conversation or thread. The name starts with a group per conversation, which is
 * how [dropConversation] finds a conversation's drafts without an index.
 */
class MediaDraftStore(context: Context, private val capturesRoot: suspend () -> File?) {
  private val filesDir = context.filesDir
  private val directory = File(filesDir, DIRECTORY)

  /** Where the camera writes for this flow; `null` while no account is open. */
  suspend fun capturesDirectory(key: MediaDraftKey): File? = withContext(Dispatchers.IO) {
    val name = names(filesDir)?.name(key) ?: return@withContext null
    capturesRoot()?.let { File(it, "$CAPTURES-$name").apply { mkdirs() } }
  }

  suspend fun load(key: MediaDraftKey): MediaDraft? = withContext(Dispatchers.IO) {
    val name = names(filesDir)?.name(key) ?: return@withContext null
    val bytes = runCatching { sealedOf(name).read() }.getOrNull() ?: return@withContext null
    val draft = runCatching { json.decodeFromString(MediaDraft.serializer(), bytes.decodeToString()) }.getOrNull()
      ?: return@withContext null
    val captures = capturesRoot()?.let { File(it, "$CAPTURES-$name") } ?: return@withContext draft
    // A draft from before #1377 names its camera files in the folder they had then, one from
    // before #1388 has them in plain files.
    val relocated = relocated(draft, captures)
    val maybeKey = names(filesDir)?.captureKey()
    if (maybeKey == null) relocated else sealPlainCaptures(relocated, captures, maybeKey)
  }

  suspend fun save(key: MediaDraftKey, draft: MediaDraft) = withContext(Dispatchers.IO) {
    val name = names(filesDir)?.name(key) ?: return@withContext
    // Written beside and renamed (SealedCache), so a kill while writing leaves the previous state.
    runCatching { sealedOf(name).write(json.encodeToString(MediaDraft.serializer(), draft).encodeToByteArray()) }
  }

  /** The flow ended (sent or closed): its state and the camera's files go. */
  suspend fun clear(key: MediaDraftKey) = withContext(Dispatchers.IO) {
    val name = names(filesDir)?.name(key) ?: return@withContext
    sealedOf(name).delete()
    capturesRoot()?.let { File(it, "$CAPTURES-$name").deleteRecursively() }
  }

  private fun sealedOf(name: String): SealedCache = SealedCache(
    File(directory, "$name.sealed"),
    SealedFile(File(directory, "$name.key"), KeystoreBox(KEY_ALIAS)),
  )

  companion object {
    private const val DIRECTORY = "media-flow"
    private const val CAPTURES = "media-flow-captures"
    private const val KEY_ALIAS = "teamcraft_media_draft"

    /** The naming secret, sealed by the Keystore key like the drafts' own keys. */
    private const val NAMES_FILE = "names.key"
    private const val NAMES_SECRET_LENGTH = 32

    /** `StoragePaths.storeDir` and the core's `UPLOAD_STAGING_DIR`. */
    private const val STORES = "store"
    private const val OUTGOING = "outgoing"

    /** A draft nobody came back to for this long is dropped at the next start. */
    private const val ABANDONED_MILLIS = 7L * 24 * 60 * 60 * 1000
    private val json = Json { ignoreUnknownKeys = true }

    /** Names before #1377: `draft-{conversation}-{thread}.sealed` (and `.key`), and the camera's folder. */
    private val LEGACY_DRAFT = Regex("""draft-(\d+)-([A-Za-z0-9-]*)\.(sealed|key)""")
    private val LEGACY_CAPTURES = Regex("""(\d+)-([A-Za-z0-9-]*)""")

    /** Naming, migration, sweep and drops of the process go one at a time (they rename and delete). */
    private val lock = Any()

    @Volatile
    private var maybeCachedNames: Pair<File, MediaDraftNames>? = null

    /** Plain copies of camera files ([SealedCaptures.openToFile]) and unfinished seals left this long are a kill's. */
    private const val LEFTOVER_MILLIS = 60L * 60 * 1000

    /**
     * The key of this device's camera files ([SealedCaptures]), derived from the naming secret: it
     * goes with it at sign-out, and a Keystore that lost its key loses both. `null` when the
     * Keystore can't give it.
     */
    fun captureKey(context: Context): ByteArray? = names(context.filesDir)?.captureKey()

    /**
     * Every draft of the device and the camera's files of every account: the session ended. Also
     * on an expired or revoked token, where the core's own wipe of its staging folder doesn't run.
     */
    fun deleteAll(context: Context) = deleteAll(context.filesDir)

    /** Drafts left for a week (a flow the app never came back to), those of older versions, and
     * camera folders whose draft is gone. */
    fun sweep(context: Context, nowMillis: Long) = sweep(context.filesDir, nowMillis, names(context.filesDir))

    /**
     * The drafts of [conversation], purged (e2ee.md §4.6) or no longer listed, with their camera
     * files: what they hold of it goes with it (#1161). Whether none is left.
     */
    fun dropConversation(context: Context, conversation: String): Boolean =
      dropConversation(context.filesDir, conversation, names(context.filesDir))

    /**
     * The naming of this device's drafts, `null` when the Keystore can't give it. The first call of
     * a process renames drafts and camera folders from before #1377.
     */
    private fun names(filesDir: File): MediaDraftNames? = synchronized(lock) {
      maybeCachedNames?.takeIf { it.first == filesDir }?.let { return@synchronized it.second }
      val file = runCatching { SealedFile(File(File(filesDir, DIRECTORY), NAMES_FILE), KeystoreBox(KEY_ALIAS)) }
        .getOrNull() ?: return@synchronized null
      val secret = runCatching { file.readOrCreate(NAMES_SECRET_LENGTH) }.getOrElse {
        // The Keystore key changed (a restored backup, a reset): the drafts it sealed can't be
        // opened either, and the sweep removes them.
        runCatching {
          file.delete()
          file.readOrCreate(NAMES_SECRET_LENGTH)
        }.getOrNull()
      } ?: return@synchronized null
      val names = MediaDraftNames(secret)
      migrate(filesDir, names)
      maybeCachedNames = filesDir to names
      names
    }

    internal fun deleteAll(filesDir: File) = synchronized(lock) {
      File(filesDir, DIRECTORY).deleteRecursively()
      captureFolders(filesDir).forEach { it.deleteRecursively() }
      maybeCachedNames = null
    }

    internal fun sweep(filesDir: File, nowMillis: Long, maybeNames: MediaDraftNames?) = synchronized(lock) {
      if (maybeNames != null) migrate(filesDir, maybeNames)
      val drafts = File(filesDir, DIRECTORY)
      drafts.listFiles()?.forEach { file ->
        if (file.name == NAMES_FILE) return@forEach
        // A draft's key is written once and its sealed state at every save: its age is the state's.
        val sealed = File(drafts, "${file.name.substringBefore('.')}.sealed")
        val lastUsed = if (sealed.exists()) sealed.lastModified() else file.lastModified()
        val isOld = nowMillis - lastUsed > ABANDONED_MILLIS
        val isUnsealed = file.name.endsWith(".json") || file.name.endsWith(".part") || file.isDirectory
        if (isOld || isUnsealed) file.deleteRecursively()
      }
      dropOrphanedCaptures(filesDir)
      dropLeftovers(filesDir, nowMillis)
    }

    internal fun dropConversation(filesDir: File, conversation: String, maybeNames: MediaDraftNames?): Boolean = synchronized(lock) {
      if (conversation.isEmpty() || !conversation.all(Char::isDigit)) return@synchronized true
      val directory = File(filesDir, DIRECTORY)
      // Names from before #1377 (`draft-{conversation}-{thread}`), should any be left, and the
      // conversation's group.
      val prefixes = listOfNotNull("draft-$conversation-", maybeNames?.groupPrefix(conversation))
      val drafts = directory.listFiles { file -> prefixes.any { file.name.startsWith(it) } }.orEmpty()
      drafts.forEach { it.delete() }
      val capturePrefixes = listOfNotNull("$conversation-", maybeNames?.groupPrefix(conversation)).map { "$CAPTURES-$it" }
      captureFolders(filesDir)
        .filter { folder -> capturePrefixes.any { folder.name.startsWith(it) } }
        .forEach { it.deleteRecursively() }
      // Without the naming its drafts can't be told from others': kept, and asked again later.
      val isUnknown = maybeNames == null && directory.listFiles { file -> file.name.startsWith(MediaDraftNames.PREFIX) }.orEmpty().isNotEmpty()
      !isUnknown && drafts.none { it.exists() }
    }

    /**
     * Drafts and camera folders named before #1377 get their keyed names. Each file is renamed on
     * its own (atomic, and its target follows from its name alone), so a kill between two steps
     * leaves files the next start renames.
     */
    internal fun migrate(filesDir: File, names: MediaDraftNames) {
      File(filesDir, DIRECTORY).listFiles().orEmpty().forEach { file ->
        if (!file.isFile || !file.name.startsWith("draft-")) return@forEach
        val match = LEGACY_DRAFT.matchEntire(file.name)
        if (match == null) {
          // An unfinished write (`.tmp`) of an old draft.
          file.delete()
          return@forEach
        }
        val (conversation, thread, extension) = match.destructured
        val target = File(file.parentFile, "${names.name(MediaDraftKey(conversation, thread.ifEmpty { null }))}.$extension")
        if (target.exists() || !file.renameTo(target)) file.delete()
      }
      captureFolders(filesDir).forEach { folder ->
        val match = LEGACY_CAPTURES.matchEntire(folder.name.removePrefix("$CAPTURES-")) ?: return@forEach
        val (conversation, thread) = match.destructured
        val target = File(folder.parentFile, "$CAPTURES-${names.name(MediaDraftKey(conversation, thread.ifEmpty { null }))}")
        if (!target.exists() && folder.renameTo(target)) return@forEach
        // A folder half moved: what is still here goes over file by file.
        target.mkdirs()
        folder.listFiles().orEmpty().forEach { child ->
          val moved = File(target, child.name)
          if (!moved.exists()) child.renameTo(moved)
        }
        folder.deleteRecursively()
      }
    }

    /** Camera files the draft names in a folder they were moved out of, found in [captures]. */
    internal fun relocated(draft: MediaDraft, captures: File): MediaDraft = draft.copy(
      items = draft.items.map { item ->
        if (!item.isCaptured || !item.uri.startsWith(FILE_SCHEME)) return@map item
        val path = item.uri.removePrefix(FILE_SCHEME)
        val moved = File(captures, path.substringAfterLast('/'))
        if (File(path).exists() || !moved.exists()) item else item.copy(uri = "$FILE_SCHEME${moved.absolutePath}")
      },
    )

    private const val FILE_SCHEME = "file://"

    /**
     * The draft's camera files sealed (#1388): a plain one (from before, or a recording a kill
     * interrupted the sealing of) is sealed beside and then removed, and the draft names the sealed
     * file. What the draft doesn't name (a recording a kill cut off, plain copies, unfinished seals)
     * goes. Each step leaves files the next load finishes: a sealed file is written beside and
     * renamed, and the plain one goes only after.
     */
    internal fun sealPlainCaptures(draft: MediaDraft, captures: File, key: ByteArray): MediaDraft {
      val sealed = draft.copy(
        items = draft.items.map { item ->
          if (!item.isCaptured || !item.uri.startsWith(FILE_SCHEME) || item.uri.endsWith(SealedCaptures.SUFFIX)) return@map item
          val plain = File(item.uri.removePrefix(FILE_SCHEME))
          if (plain.parentFile?.absolutePath != captures.absolutePath) return@map item
          val target = File(captures, "${plain.name}${SealedCaptures.SUFFIX}")
          val isSealed = runCatching {
            if (plain.exists()) SealedMedia.sealTo(target, key, plain)
            target.exists()
          }.getOrDefault(false)
          if (!isSealed) return@map item
          plain.delete()
          item.copy(uri = "$FILE_SCHEME${target.absolutePath}")
        },
      )
      val named = sealed.items.filter { it.isCaptured && it.uri.startsWith(FILE_SCHEME) }.map { File(it.uri.removePrefix(FILE_SCHEME)).name }.toSet()
      captures.listFiles().orEmpty().forEach { file -> if (file.name !in named) file.deleteRecursively() }
      return sealed
    }

    /** Plain copies and unfinished seals in camera folders that a kill left. */
    private fun dropLeftovers(filesDir: File, nowMillis: Long) {
      captureFolders(filesDir).flatMap { it.listFiles().orEmpty().toList() }
        .filter { it.name.startsWith(SealedCaptures.OPENED_PREFIX) || it.name.endsWith(".tmp") }
        .filter { nowMillis - it.lastModified() > LEFTOVER_MILLIS }
        .forEach { it.deleteRecursively() }
    }

    /** The core's sweep of its staging folder skips folders, so these are this store's to remove. */
    private fun dropOrphanedCaptures(filesDir: File) {
      val drafts = File(filesDir, DIRECTORY)
      captureFolders(filesDir)
        .filter { folder ->
          val name = folder.name.removePrefix("$CAPTURES-")
          // Before #1377 a draft was `draft-{name}.sealed`.
          val draft = if (name.startsWith(MediaDraftNames.PREFIX)) "$name.sealed" else "draft-$name.sealed"
          !File(drafts, draft).exists()
        }
        .forEach { it.deleteRecursively() }
    }

    /** `store/<server>/outgoing/<account>/media-flow-captures-<name>`: the core's staging folders. */
    private fun captureFolders(filesDir: File): List<File> =
      File(filesDir, STORES).listFiles().orEmpty()
        .flatMap { server -> File(server, OUTGOING).listFiles().orEmpty().toList() }
        .flatMap { account -> account.listFiles { file -> file.isDirectory && file.name.startsWith("$CAPTURES-") }.orEmpty().toList() }
  }
}

/**
 * The names of a draft's files: `d-{group}-{draft}`, HMAC-SHA256 under a random secret of the
 * device, so neither part shows a conversation or thread (#1377), the way the media cache names its
 * groups (e2ee.md §9.6). All drafts of a conversation share the group, so it is dropped as a whole.
 */
internal class MediaDraftNames(secret: ByteArray) {
  private val secretKey = SecretKeySpec(secret.copyOf(), ALGORITHM)

  fun name(key: MediaDraftKey): String =
    "${groupPrefix(key.conversation)}${mac("draft", key.conversation, key.maybeThread.orEmpty())}"

  /** The key of the camera files ([SealedCaptures]): a MAC of its own label, so independent of the names. */
  fun captureKey(): ByteArray = Mac.getInstance(ALGORITHM).run {
    init(secretKey)
    doFinal("teamcraft media-flow capture key v1".encodeToByteArray())
  }

  /** What the names of [conversation]'s drafts start with. */
  fun groupPrefix(conversation: String): String = "$PREFIX${mac("group", conversation)}-"

  private fun mac(vararg parts: String): String {
    val mac = Mac.getInstance(ALGORITHM)
    mac.init(secretKey)
    // Length-prefixed, so no two lists of parts give the same input.
    parts.forEach { part ->
      val bytes = part.encodeToByteArray()
      mac.update(bytes.size.toString().encodeToByteArray())
      mac.update(':'.code.toByte())
      mac.update(bytes)
    }
    return mac.doFinal().copyOf(ID_BYTES).joinToString("") { "%02x".format(it) }
  }

  companion object {
    const val PREFIX = "d-"
    private const val ALGORITHM = "HmacSHA256"
    private const val ID_BYTES = 16
  }
}
