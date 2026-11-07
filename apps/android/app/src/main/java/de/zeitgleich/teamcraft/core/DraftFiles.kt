package de.zeitgleich.teamcraft.core

import java.io.File
import java.nio.file.Files

/**
 * How the composer's picked files travel with its draft (#1220). A file goes up as it is picked,
 * so the draft keeps it after the app was killed and the user's other devices show and send it.
 * Pure rules, so they are tested without a core; the view model and the composer apply them.
 */
object DraftFiles {
  /**
   * The draft's copy of a staged file. The core deletes a staged copy once it is up, while the
   * composer still shows it (and the picture editor reads it): the core gets a second name of the
   * same file (a hard link in the staging folder, no second copy of the bytes). A file outside the
   * staging folder isn't the core's to delete, and one that can't be linked goes as it is: the send
   * then takes the draft's upload, only the preview may go.
   */
  fun copyFor(file: PickedFile, stagingDir: File, id: String = DraftCopy.newId()): DraftCopy {
    val source = File(file.path)
    if (source.absoluteFile.parentFile?.canonicalFile != stagingDir.canonicalFile) return DraftCopy(id, file.path)
    val link = File(stagingDir, "draft-$id")
    return runCatching {
      Files.createLink(link.toPath(), source.toPath())
      DraftCopy(id, link.absolutePath)
    }.getOrElse { DraftCopy(id, file.path) }
  }

  /**
   * The composer's files once [prepared] files have their draft copies: those still waiting take
   * them, in place. [attached] are the ones that go to the draft now, [dropped] the copies of files
   * that were sent or removed meanwhile (the draft doesn't take them; their copy goes).
   */
  fun merge(current: List<PickedFile>, prepared: List<Pair<PickedFile, DraftCopy>>): Merged {
    val (waiting, gone) = prepared.partition { (file, _) -> current.any { it.path == file.path && it.maybeDraft == null } }
    val copies = waiting.associate { (file, copy) -> file.path to copy }
    val files = current.map { file -> copies[file.path]?.takeIf { file.maybeDraft == null }?.let { file.copy(maybeDraft = it) } ?: file }
    return Merged(
      files = files,
      attached = files.filter { file -> file.maybeDraft != null && file.maybeDraft == copies[file.path] },
      // A copy that is the file itself (not linked) went with its message or was deleted with it.
      dropped = gone.filter { (file, copy) -> copy.path != file.path }.map { (_, copy) -> copy },
    )
  }

  data class Merged(val files: List<PickedFile>, val attached: List<PickedFile>, val dropped: List<DraftCopy>)

  /**
   * The draft's files the composer shows besides its own picks: those attached on another device,
   * or before the app was last started. Not the composer's own, nor those it removed or sent while
   * the core catches up ([gone]).
   */
  fun others(items: List<DraftFileItem>, local: List<PickedFile>, gone: Collection<String>): List<DraftFileItem> {
    val localIds = local.mapNotNull { it.maybeDraft?.id }.toSet()
    return items.filter { it.id !in localIds && it.id !in gone }
  }

  /**
   * The composer's own copies to delete once their message is queued: a file of the draft goes from
   * the core's copy, so the composer's second name of it is left over. A file not (yet) in the
   * draft goes from the composer's copy, which the core deletes after the upload.
   */
  fun leftAfterSend(sent: List<PickedFile>): List<String> =
    sent.filter { file -> file.maybeDraft?.let { it.path != file.path } == true }.map { it.path }

  /** The staged paths that go with a file taken out of the composer: its own and the draft's copy. */
  fun pathsOfRemoved(file: PickedFile): List<String> = listOfNotNull(file.path, file.maybeDraft?.path).distinct()
}
