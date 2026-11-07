package de.zeitgleich.teamcraft.ui.media

import java.io.File
import java.nio.file.Files
import de.zeitgleich.teamcraft.security.SealedMedia
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class MediaDraftStoreTest {
  private val names = MediaDraftNames(ByteArray(32) { it.toByte() })

  @Test
  fun theEndOfASessionTakesTheCameraFilesOfEveryAccount() {
    val files = files()
    val first = capture(files, "server-a", "account-1", nameOf("111", null))
    val second = capture(files, "server-b", "account-2", nameOf("222", null))
    val upload = File(first.parentFile, "123-photo.jpg").apply { writeText("staged") }
    MediaDraftStore.deleteAll(files)
    assertFalse(first.exists())
    assertFalse(second.exists())
    assertFalse(File(files, "media-flow").exists())
    // Files the core staged for sending are the core's: its own sweep and sign-out handle them.
    assertTrue(upload.exists())
  }

  @Test
  fun theSweepRemovesCameraFilesWhoseDraftIsGone() {
    val files = files()
    val keptName = nameOf("111", null)
    val kept = capture(files, "server-a", "account-1", keptName)
    val orphan = capture(files, "server-a", "account-1", nameOf("222", null))
    draft(files, "$keptName.sealed")
    MediaDraftStore.sweep(files, nowMillis = System.currentTimeMillis(), names)
    assertTrue(kept.exists())
    assertFalse(orphan.exists())
  }

  @Test
  fun aWeekOldDraftGoesWithItsCameraFiles() {
    val files = files()
    val name = nameOf("111", null)
    val captures = capture(files, "server-a", "account-1", name)
    val sealed = draft(files, "$name.sealed")
    MediaDraftStore.sweep(files, nowMillis = System.currentTimeMillis() + 8L * 24 * 60 * 60 * 1000, names)
    assertFalse(sealed.exists())
    assertFalse(captures.exists())
  }

  @Test
  fun aDraftSavedLatelyKeepsItsOldKeyAndTheNamingSecretStays() {
    val files = files()
    val name = nameOf("111", null)
    val week = 8L * 24 * 60 * 60 * 1000
    val now = System.currentTimeMillis() + week
    val key = draft(files, "$name.key").apply { setLastModified(now - week) }
    val sealed = draft(files, "$name.sealed").apply { setLastModified(now) }
    val secret = draft(files, "names.key").apply { setLastModified(now - week) }
    MediaDraftStore.sweep(files, nowMillis = now, names)
    assertTrue(key.exists())
    assertTrue(sealed.exists())
    assertTrue(secret.exists())
  }

  @Test
  fun aDroppedConversationsDraftsGoWithTheirCameraFilesAndOthersStay() {
    val files = files()
    val dropped = listOf("${nameOf("111", null)}.sealed", "${nameOf("111", null)}.key", "${nameOf("111", "222")}.sealed")
      .map { draft(files, it) }
    // Another conversation whose ID starts with the same digits.
    val other = draft(files, "${nameOf("1112", null)}.sealed")
    val droppedCaptures = capture(files, "server-a", "account-1", nameOf("111", "222"))
    val otherCaptures = capture(files, "server-a", "account-1", nameOf("1112", null))
    assertTrue(MediaDraftStore.dropConversation(files, "111", names))
    dropped.forEach { assertFalse(it.name, it.exists()) }
    assertFalse(droppedCaptures.exists())
    assertTrue(other.exists())
    assertTrue(otherCaptures.exists())
  }

  @Test
  fun aConversationWithoutDraftsIsDroppedAtOnce() {
    assertTrue(MediaDraftStore.dropConversation(files(), "111", names))
    assertTrue(MediaDraftStore.dropConversation(files(), "not-an-id", names))
  }

  @Test
  fun withoutTheNamingDraftsOfUnknownConversationsAreKeptForLater() {
    val files = files()
    val sealed = draft(files, "${nameOf("111", null)}.sealed")
    assertFalse(MediaDraftStore.dropConversation(files, "111", maybeNames = null))
    assertTrue(sealed.exists())
  }

  @Test
  fun namesShowNoConversationOrThreadAndGroupByConversation() {
    val name = nameOf("1234567", "7654321")
    assertFalse(name.contains("1234567"))
    assertFalse(name.contains("7654321"))
    assertTrue(name.matches(Regex("d-[0-9a-f]{32}-[0-9a-f]{32}")))
    assertTrue(name.startsWith(names.groupPrefix("1234567")))
    assertTrue(nameOf("1234567", null).startsWith(names.groupPrefix("1234567")))
    assertNotEquals(name, nameOf("1234567", null))
    assertNotEquals(names.groupPrefix("1234567"), names.groupPrefix("123456"))
    // Stable for a secret, different under another.
    assertEquals(name, MediaDraftNames(ByteArray(32) { it.toByte() }).name(MediaDraftKey("1234567", "7654321")))
    assertNotEquals(name, MediaDraftNames(ByteArray(32)).name(MediaDraftKey("1234567", "7654321")))
    // The parts can't be shifted into each other.
    assertNotEquals(nameOf("12", "3"), nameOf("1", "23"))
  }

  @Test
  fun draftsAndCameraFoldersOfOlderVersionsGetTheirNewNames() {
    val files = files()
    val sealed = draft(files, "draft-111-222.sealed").apply { writeText("state") }
    val key = draft(files, "draft-111-.key").apply { writeText("key") }
    val unfinished = draft(files, "draft-111-.sealed.tmp")
    val captures = capture(files, "server-a", "account-1", "111-222")
    MediaDraftStore.migrate(files, names)
    assertFalse(sealed.exists())
    assertFalse(key.exists())
    assertFalse(unfinished.exists())
    assertFalse(captures.exists())
    assertEquals("state", File(files, "media-flow/${nameOf("111", "222")}.sealed").readText())
    assertEquals("key", File(files, "media-flow/${nameOf("111", null)}.key").readText())
    val moved = File(files, "store/server-a/outgoing/account-1/media-flow-captures-${nameOf("111", "222")}")
    assertEquals("picture", File(moved, "photo.jpg").readText())
    // Nothing left that names the conversation; a second run changes nothing.
    MediaDraftStore.migrate(files, names)
    val left = File(files, "media-flow").list().orEmpty().toList() + moved.parentFile!!.list().orEmpty()
    assertTrue(left.toString(), left.all { it.startsWith("d-") || it.startsWith("media-flow-captures-d-") })
    assertTrue(File(moved, "photo.jpg").exists())
  }

  @Test
  fun aMigrationCutShortIsFinishedByTheNextOne() {
    val files = files()
    val name = nameOf("111", null)
    // The key was renamed, then the app died before the state and the camera folder followed.
    draft(files, "$name.key").writeText("key")
    draft(files, "draft-111-.sealed").writeText("state")
    val target = capture(files, "server-a", "account-1", name)
    capture(files, "server-a", "account-1", "111-").also { File(it, "video.mp4").writeText("film") }
    MediaDraftStore.migrate(files, names)
    assertEquals("state", File(files, "media-flow/$name.sealed").readText())
    assertEquals("key", File(files, "media-flow/$name.key").readText())
    assertEquals("film", File(target, "video.mp4").readText())
    assertFalse(File(target.parentFile, "media-flow-captures-111-").exists())
  }

  @Test
  fun theSweepMigratesBeforeItLooksForOrphans() {
    val files = files()
    draft(files, "draft-111-.sealed")
    capture(files, "server-a", "account-1", "111-")
    MediaDraftStore.sweep(files, nowMillis = System.currentTimeMillis(), names)
    assertTrue(File(files, "store/server-a/outgoing/account-1/media-flow-captures-${nameOf("111", null)}/photo.jpg").exists())
  }

  @Test
  fun aDroppedConversationsOldNamesGoEvenWithoutTheNaming() {
    val files = files()
    val sealed = draft(files, "draft-111-.sealed")
    val captures = capture(files, "server-a", "account-1", "111-")
    assertTrue(MediaDraftStore.dropConversation(files, "111", maybeNames = null))
    assertFalse(sealed.exists())
    assertFalse(captures.exists())
  }

  @Test
  fun aLoadedDraftFindsItsCameraFilesInTheirNewFolder() {
    val files = files()
    val moved = capture(files, "server-a", "account-1", nameOf("111", null))
    val old = File(moved.parentFile, "media-flow-captures-111-")
    val gallery = MediaDraftItem(id = "a", uri = "content://media/1", isVideo = false)
    val shot = MediaDraftItem(id = "b", uri = "file://${File(old, "photo.jpg").absolutePath}", isVideo = false, isCaptured = true)
    val lost = MediaDraftItem(id = "c", uri = "file://${File(old, "gone.jpg").absolutePath}", isVideo = false, isCaptured = true)
    val draft = MediaDraftStore.relocated(MediaDraft(items = listOf(gallery, shot, lost)), moved)
    assertEquals(gallery, draft.items[0])
    assertEquals("file://${File(moved, "photo.jpg").absolutePath}", draft.items[1].uri)
    assertEquals(lost, draft.items[2])
  }

  @Test
  fun aLoadedDraftsPlainCameraFilesAreSealedAndWhatItDoesNotNameGoes() {
    val files = files()
    val captures = capture(files, "server-a", "account-1", nameOf("111", null))
    val key = names.captureKey()
    val plain = File(captures, "photo.jpg")
    val sealedAlready = File(captures, "video-1.mp4.sealed").apply { SealedMedia.sealTo(this, key, "video".encodeToByteArray()) }
    val leftover = File(captures, "video-2.mp4").apply { writeText("a recording a kill cut off") }
    val opened = File(captures, "open-1.mp4").apply { writeText("plain copy") }
    val gallery = MediaDraftItem(id = "a", uri = "content://media/1", isVideo = false)
    val shot = MediaDraftItem(id = "b", uri = "file://${plain.absolutePath}", isVideo = false, isCaptured = true)
    val video = MediaDraftItem(id = "c", uri = "file://${sealedAlready.absolutePath}", isVideo = true, isCaptured = true)
    val draft = MediaDraftStore.sealPlainCaptures(MediaDraft(items = listOf(gallery, shot, video)), captures, key)
    val sealedShot = File(captures, "photo.jpg.sealed")
    assertEquals(listOf(gallery.uri, "file://${sealedShot.absolutePath}", video.uri), draft.items.map { it.uri })
    assertFalse(plain.exists())
    assertArrayEquals("picture".encodeToByteArray(), SealedMedia.readAll(sealedShot, key))
    assertTrue(sealedAlready.exists())
    assertFalse(leftover.exists())
    assertFalse(opened.exists())
    // Loading again changes nothing.
    assertEquals(draft, MediaDraftStore.sealPlainCaptures(draft, captures, key))
    assertTrue(sealedShot.exists())
  }

  @Test
  fun aSealKilledBeforeThePlainFileWentIsFinished() {
    val files = files()
    val captures = capture(files, "server-a", "account-1", nameOf("111", null))
    val key = names.captureKey()
    val plain = File(captures, "photo.jpg")
    // Sealed and renamed, then killed: the draft still names the plain file, which is gone.
    SealedMedia.sealTo(File(captures, "photo.jpg.sealed"), key, plain)
    plain.delete()
    val shot = MediaDraftItem(id = "b", uri = "file://${plain.absolutePath}", isVideo = false, isCaptured = true)
    val draft = MediaDraftStore.sealPlainCaptures(MediaDraft(items = listOf(shot)), captures, key)
    assertEquals("file://${File(captures, "photo.jpg.sealed").absolutePath}", draft.items.single().uri)
  }

  @Test
  fun theCameraKeyIsTheDevicesAndNotAName() {
    val key = names.captureKey()
    assertEquals(32, key.size)
    assertArrayEquals(key, MediaDraftNames(ByteArray(32) { it.toByte() }).captureKey())
    assertNotEquals(key.toList(), MediaDraftNames(ByteArray(32) { 1 }).captureKey().toList())
  }

  @Test
  fun theSweepTakesPlainCopiesAndUnfinishedSealsAKillLeft() {
    val files = files()
    val name = nameOf("111", null)
    val captures = capture(files, "server-a", "account-1", name)
    draft(files, "$name.sealed")
    val now = System.currentTimeMillis()
    val old = File(captures, "open-1.mp4").apply { writeText("plain"); setLastModified(now - 2 * 60 * 60 * 1000) }
    val unfinished = File(captures, "photo-1.jpg.sealed.tmp").apply { writeText("half"); setLastModified(now - 2 * 60 * 60 * 1000) }
    val inUse = File(captures, "open-2.mp4").apply { writeText("being sent") }
    MediaDraftStore.sweep(files, nowMillis = now, names)
    assertFalse(old.exists())
    assertFalse(unfinished.exists())
    assertTrue(inUse.exists())
    assertTrue(File(captures, "photo.jpg").exists())
  }

  private fun nameOf(conversation: String, maybeThread: String?): String = names.name(MediaDraftKey(conversation, maybeThread))

  private fun files(): File = Files.createTempDirectory("media-draft").toFile().apply { deleteOnExit() }

  private fun draft(files: File, name: String): File =
    File(files, "media-flow/$name").apply {
      parentFile!!.mkdirs()
      writeText("x")
    }

  private fun capture(files: File, server: String, account: String, name: String): File =
    File(files, "store/$server/outgoing/$account/media-flow-captures-$name").apply {
      mkdirs()
      File(this, "photo.jpg").writeText("picture")
    }
}
