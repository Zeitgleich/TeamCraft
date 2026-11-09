package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.core.FileTransfer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

private const val MB = 1_000_000L

class UploadPillTest {
  @Test
  fun nothingGoingUpHasNoControl() {
    assertNull(UploadPill.of(emptyList()))
    assertNull(UploadPill.of(listOf(file(FileTransfer("remote"), MB))))
    assertNull(UploadPill.of(listOf(file(FileTransfer("upload_failed", reason = "too_large"), MB))))
  }

  @Test
  fun beforeTheFirstByteItIsProcessing() {
    val files = listOf(file(uploading(0, 5 * MB), 5 * MB), file(uploading(0, 3 * MB), 3 * MB))
    assertEquals(UploadPill.Processing, UploadPill.of(files))
    assertNull(UploadPill.Processing.maybeFraction)
  }

  @Test
  fun aFailedAttemptBeforeTheFirstByteIsRetrying() {
    assertEquals(UploadPill.Retrying, UploadPill.of(listOf(file(uploading(0, 5 * MB, failedAttempts = 2), 5 * MB))))
  }

  @Test
  fun bytesAddUpOverAllFiles() {
    val pill = UploadPill.of(listOf(file(uploading(2 * MB, 4 * MB), 4 * MB), file(uploading(0, 6 * MB), 6 * MB)))
    assertEquals(UploadPill.Sending(2 * MB, 10 * MB), pill)
    assertEquals(0.2f, pill?.maybeFraction)
  }

  @Test
  fun finishedFilesCountWholeSoTheRingNeverGoesBack() {
    val files = listOf(file(FileTransfer("remote"), 4 * MB), file(uploading(0, 6 * MB), 6 * MB))
    assertEquals(UploadPill.Sending(4 * MB, 10 * MB), UploadPill.of(files))
  }

  @Test
  fun filesThatFailedForGoodDoNotCount() {
    val files = listOf(file(FileTransfer("upload_failed", reason = "too_large"), 90 * MB), file(uploading(MB, 2 * MB), 2 * MB))
    assertEquals(UploadPill.Sending(MB, 2 * MB), UploadPill.of(files))
  }

  @Test
  fun aStalledFileMakesTheMessageFailed() {
    val files = listOf(file(uploading(MB, 2 * MB), 2 * MB), file(FileTransfer("upload_stalled", reason = "storage_unavailable"), MB))
    assertEquals(UploadPill.Failed(isStorageUnavailable = true), UploadPill.of(files))
  }

  @Test
  fun reportedBytesBeyondTheTotalDoNotOverfill() {
    assertEquals(1f, UploadPill.of(listOf(file(uploading(9 * MB, 4 * MB), 4 * MB)))?.maybeFraction)
  }

  @Test
  fun cancelStopsTheLastFilesFirst() {
    val files = listOf(
      file(uploading(0, MB), MB, id = "pending-0"),
      file(FileTransfer("remote"), MB, id = "17"),
      file(uploading(0, MB), MB, id = "pending-2"),
      file(FileTransfer("upload_stalled", reason = "network"), MB, id = "pending-1"),
    )
    assertEquals(listOf("pending-2", "pending-1", "pending-0"), UploadPill.cancellable(files).map { it.fileId })
  }

  private fun uploading(done: Long, total: Long, failedAttempts: Int = 0) =
    FileTransfer("uploading", doneBytes = done, totalBytes = total, failedAttempts = failedAttempts)

  private fun file(transfer: FileTransfer, size: Long, id: String = "pending-0") = FileInfo(
    conversation = "1",
    message = "2",
    fileId = id,
    fileName = "photo.jpg",
    mediaType = "image/jpeg",
    sizeBytes = size,
    transfer = transfer,
  )
}
