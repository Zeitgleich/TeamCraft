package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.FileInfo

/**
 * What the one upload control of a message shows (Signal-style): a pill in the corner of its
 * pictures, or a row under documents, with one ring for all its files instead of a ring and a
 * "Cancel" per file. The same rule as desktop and web (`teamcraft_client_api::upload_pill`) and
 * iOS (`UploadPill.swift`), with the same test cases.
 */
sealed interface UploadPill {
  /** Going up, but no byte has left yet: "Processing…" and a turning ring. */
  data object Processing : UploadPill

  /** No byte has left yet and an attempt failed; the core tries again by itself (#742). */
  data object Retrying : UploadPill

  /** Bytes are moving: "3.2 MB / 70 MB" and a ring filled to the share of all files together. */
  data class Sending(val doneBytes: Long, val totalBytes: Long) : UploadPill

  /** A file stopped retrying at the usual pace: "Try again" restarts the message's upload. */
  data class Failed(val isStorageUnavailable: Boolean) : UploadPill

  /** How far the files got, 0..1; null while the ring turns without a value. */
  val maybeFraction: Float?
    get() = (this as? Sending)?.takeIf { it.totalBytes > 0 }?.let { (it.doneBytes.toFloat() / it.totalBytes).coerceIn(0f, 1f) }

  companion object {
    /**
     * The control for a message's files; null when nothing is going up (done, or never was).
     * Files that finished count with their whole size, so the ring doesn't jump back when one is
     * done; a file that failed for good (the message went out without it) doesn't count.
     */
    fun of(files: List<FileInfo>): UploadPill? {
      files.firstOrNull { it.transfer.state == STALLED }?.let { return Failed(it.transfer.isStorageUnavailable) }
      if (files.none { it.transfer.state == UPLOADING }) return null
      var doneBytes = 0L
      var totalBytes = 0L
      var hasFailedAttempts = false
      for (file in files) {
        val transfer = file.transfer
        when (transfer.state) {
          UPLOADING -> {
            doneBytes += transfer.doneBytes.coerceAtMost(transfer.totalBytes)
            totalBytes += transfer.totalBytes
            hasFailedAttempts = hasFailedAttempts || transfer.failedAttempts > 0
          }
          FAILED_FOR_GOOD -> {}
          else -> {
            doneBytes += file.sizeBytes
            totalBytes += file.sizeBytes
          }
        }
      }
      return when {
        doneBytes > 0 -> Sending(doneBytes, totalBytes)
        hasFailedAttempts -> Retrying
        else -> Processing
      }
    }

    /**
     * The files a "Cancel" stops, last first: the core finds a pending file by its place, so
     * cancelling the last ones first keeps the places of the others.
     */
    fun cancellable(files: List<FileInfo>): List<FileInfo> =
      files.filter { it.transfer.isCancellable }.sortedByDescending { it.fileId.removePrefix("pending-").toIntOrNull() ?: -1 }

    private const val UPLOADING = "uploading"
    private const val STALLED = "upload_stalled"
    private const val FAILED_FOR_GOOD = "upload_failed"
  }
}
