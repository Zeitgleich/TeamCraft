package de.zeitgleich.teamcraft.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.core.WireJson
import uniffi.teamcraft_ffi.MediaScope
import uniffi.teamcraft_ffi.MediaViewer
import uniffi.teamcraft_ffi.MediaViewerItem
import uniffi.teamcraft_ffi.TeamCraftException
import uniffi.teamcraft_ffi.mediaViewerLimits

/** The picture the image viewer opens on. */
data class PictureTarget(val conversation: String, val message: String, val fileId: String) {
  val key: String get() = "$message:$fileId"

  /** For a saved screen state: the three IDs. */
  fun toList(): List<String> = listOf(conversation, message, fileId)

  companion object {
    fun of(file: FileInfo) = PictureTarget(file.conversation, file.message, file.fileId)

    fun fromList(ids: List<String>): PictureTarget? =
      if (ids.size == 3) PictureTarget(ids[0], ids[1], ids[2]) else null
  }
}

/** Where a page's original stands. */
sealed interface OriginalState {
  /** Coming: downloading (the share of the bytes once known) or being decoded. */
  data class Loading(val maybeProgress: Float?) : OriginalState

  /** Waiting for "Load original" (the setting, or a metered network with "Wi-Fi only"). */
  data object OnRequest : OriginalState

  /** The download failed: "Try again". */
  data object Failed : OriginalState

  /** Downloaded: the page decodes and shows it. */
  data class Here(val path: String) : OriginalState

  /** Nothing to fetch (an own picture still going up): the thumbnail is all there is. */
  data object ThumbnailOnly : OriginalState
}

/**
 * The state of an open image viewer (#240). Which pictures it pages through (the message's album or
 * the whole conversation, ends not wrapping), where it stands and which page of the conversation it
 * reads next come from the core (`MediaViewer`, shared with iOS, desktop and web); this keeps what
 * the screen needs: the files of the picture on screen and its neighbours. Pages further away have
 * no file here, so nothing of them is decoded.
 */
class PictureViewerState(val target: PictureTarget, private val chat: ChatClient) {
  private val maybeViewer: MediaViewer? =
    try {
      MediaViewer(target.conversation, target.message, target.fileId)
    } catch (failure: TeamCraftException) {
      null
    }
  private val limits = mediaViewerLimits()

  var items by mutableStateOf<List<MediaViewerItem>>(emptyList())
    private set

  /** The opened message has been read: an empty list now means its pictures are gone. */
  var isLoaded by mutableStateOf(false)
    private set
  var scope by mutableStateOf(MediaScope.CONVERSATION)
    private set
  var canSwitchScope by mutableStateOf(false)
    private set

  /** Where the core has the user; the pager follows it when the list changes. */
  var currentIndex by mutableStateOf(0)
    private set

  /** The files of the picture on screen and its neighbours, by item key. */
  var files by mutableStateOf<Map<String, FileInfo>>(emptyMap())
    private set

  /** Originals the user asked for although the setting waits for a tap. */
  private val requested = mutableSetOf<String>()

  /** Downloads asked for, so a picture isn't asked for again while the core starts it. */
  private val fetching = mutableSetOf<String>()

  val maxZoom: Float = limits.maxZoom.toFloat()
  val doubleTapZoom: Float = limits.doubleTapZoom.toFloat()

  /** The opened picture's message or all its pictures are gone: the viewer closes. */
  val isGone: Boolean get() = maybeViewer == null || (isLoaded && items.isEmpty())

  /** A download in the window is under way: its progress is read again while it runs. */
  val isDownloading: Boolean get() = files.values.any { it.transfer.state == "downloading" }

  val currentFile: FileInfo? get() = items.getOrNull(currentIndex)?.let { files[it.key] }

  /**
   * Reads what the viewer needs (the opened message, then pages of the conversation near the
   * picture on screen) until it needs nothing more. A read that fails isn't asked again.
   */
  suspend fun load() {
    val viewer = maybeViewer ?: return
    // Off the main thread: the files on screen carry their thumbnails as JSON.
    withContext(Dispatchers.Default) {
      while (true) {
        val query = viewer.nextQuery() ?: break
        val answer = runRead { chat.rawQuery(query) } ?: break
        runRead { viewer.addAnswer(query, answer) }
        apply()
      }
      apply()
    }
  }

  /** After a change in the core: the files on screen again (a download's progress or end, a deleted message). */
  suspend fun refresh() {
    val viewer = maybeViewer ?: return
    if (!isLoaded) return
    val query = viewer.refreshQuery() ?: return
    withContext(Dispatchers.Default) {
      val answer = runRead { chat.rawQuery(query) } ?: return@withContext
      if (runRead { viewer.addAnswer(query, answer) } == true) apply()
    }
  }

  /** The user paged to `index`; [load] then reads the files around it and further pages near an end. */
  fun show(index: Int) {
    val viewer = maybeViewer ?: return
    val item = items.getOrNull(index) ?: return
    if (viewer.show(item.messageId, item.fileId)) currentIndex = index
  }

  /** Back on a picture after the screen was recreated (a rotation): its key and the scope; then [load]. */
  fun resume(key: String, maybeScope: MediaScope?) {
    val viewer = maybeViewer ?: return
    maybeScope?.let { viewer.setScope(it) }
    items.firstOrNull { it.key == key }?.let { viewer.show(it.messageId, it.fileId) }
  }

  /** Switches between the album and the conversation; then [load]. */
  fun switchScope(scope: MediaScope) {
    maybeViewer?.setScope(scope)
    this.scope = scope
  }

  /** Where the original of `key` stands; `isAutomatic` is the setting on the current network. */
  fun originalState(key: String, isAutomatic: Boolean): OriginalState {
    val file = files[key] ?: return OriginalState.Loading(null)
    val path = file.transfer.path
    return when (file.transfer.state) {
      "local" -> if (path != null) OriginalState.Here(path) else OriginalState.Failed
      "downloading" -> OriginalState.Loading(file.transfer.maybeProgress)
      "failed" -> OriginalState.Failed
      "remote" -> if (isAutomatic || key in requested) OriginalState.Loading(null) else OriginalState.OnRequest
      else -> OriginalState.ThumbnailOnly
    }
  }

  /** "Load original" or "Try again" on a page. */
  fun request(key: String) {
    requested += key
    fetching -= key
  }

  /**
   * The download the picture on screen needs now, if any: when the setting allows it or the user
   * asked; a failed one waits for "Try again". Each is handed out once.
   */
  fun nextDownload(isAutomatic: Boolean): FileInfo? {
    val key = items.getOrNull(currentIndex)?.key ?: return null
    val file = files[key] ?: return null
    val isWanted = when (file.transfer.state) {
      "remote" -> isAutomatic || key in requested
      "failed" -> key in requested
      else -> false
    }
    if (!isWanted || key in fetching) return null
    fetching += key
    return file
  }

  /** Frees the core's side of the viewer. */
  fun close() {
    maybeViewer?.close()
  }

  /** Reads the list and the window's files from the core; files further away are let go. */
  private fun apply() {
    val viewer = maybeViewer ?: return
    val state = viewer.state()
    items = state.items
    isLoaded = state.isLoaded
    scope = state.scope
    canSwitchScope = state.canSwitchScope
    val index = state.index.toInt()
    currentIndex = index
    val radius = limits.decodedNeighbors.toInt()
    val window = if (items.isEmpty()) emptyList() else items.subList((index - radius).coerceAtLeast(0), (index + radius + 1).coerceAtMost(items.size))
    val fresh = window.mapNotNull { item ->
      val json = viewer.fileJson(item.messageId, item.fileId) ?: return@mapNotNull null
      val file = runCatching { WireJson.decodeFromString(FileInfo.serializer(), json) }.getOrNull() ?: return@mapNotNull null
      item.key to file
    }.toMap()
    if (fresh != files) files = fresh
  }

  /** A read or an answer the core refused: `null`, and the viewer keeps what it has. */
  private suspend fun <T> runRead(block: suspend () -> T): T? =
    try {
      block()
    } catch (failure: TeamCraftException) {
      null
    } catch (failure: CancellationException) {
      // A CancellationException is an IllegalStateException; the caller's cancellation goes on.
      throw failure
    } catch (failure: IllegalStateException) {
      // Signed out meanwhile: no account to read for.
      null
    }
}
