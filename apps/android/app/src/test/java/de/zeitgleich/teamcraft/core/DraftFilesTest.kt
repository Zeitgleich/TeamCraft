package de.zeitgleich.teamcraft.core

import java.io.File
import java.nio.file.Files
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Picked files travel with the composer's draft (#1220). */
class DraftFilesTest {
  private val copy = DraftCopy("0123456789abcdef0123456789abcdef", "/staging/draft-0123")

  @Test
  fun aFreshIdIsAClientMessageId() {
    val id = DraftCopy.newId()
    assertTrue(id.matches(Regex("[0-9a-f]{32}")))
    assertTrue(id != DraftCopy.newId())
  }

  @Test
  fun attachingNamesEachFileWithItsDraftIdAndTheCoresCopy() {
    val file = PickedFile("a.png", "image/png", "/staging/1-a.png", maybeDraft = copy)
    val json = Json.parseToJsonElement(ChatActions.attachDraftFiles("7", "9", listOf(file))).jsonObject
    assertEquals("attach_draft_files", json["action"]!!.jsonPrimitive.content)
    assertEquals("7", json["conversation"]!!.jsonPrimitive.content)
    assertEquals("9", json["maybe_thread"]!!.jsonPrimitive.content)
    val sent = json["files"]!!.jsonArray[0].jsonObject
    assertEquals(copy.id, sent["draft_id"]!!.jsonPrimitive.content)
    assertEquals("/staging/draft-0123", sent["source"]!!.jsonObject["path"]!!.jsonPrimitive.content)
  }

  @Test
  fun aSendTakesTheDraftsFilesByIdAndItsOwnPicksWithTheirDraftId() {
    val picked = PickedFile("a.png", "image/png", "/staging/1-a.png", maybeDraft = copy)
    val fromDraft = DraftFileItem(id = "fedcba9876543210fedcba9876543210", fileName = "b.pdf", mediaType = "application/pdf")
    val message = Json.parseToJsonElement(
      ChatActions.send("7", "", SendOptions(files = listOf(picked), draftFiles = listOf(fromDraft))),
    ).jsonObject["message"]!!.jsonObject
    val files = message["files"]!!.jsonArray.map { it.jsonObject }
    assertEquals(copy.id, files[0]["draft_id"]!!.jsonPrimitive.content)
    assertEquals("/staging/draft-0123", files[0]["source"]!!.jsonObject["path"]!!.jsonPrimitive.content)
    val source = files[1]["source"]!!.jsonObject
    assertEquals("draft", source["kind"]!!.jsonPrimitive.content)
    assertEquals(fromDraft.id, source["id"]!!.jsonPrimitive.content)
    assertEquals("b.pdf", files[1]["file_name"]!!.jsonPrimitive.content)
    assertNull(files[1]["draft_id"])
  }

  @Test
  fun aFileOutsideTheDraftHasNoDraftId() {
    val message = Json.parseToJsonElement(
      ChatActions.send("7", "", SendOptions(files = listOf(PickedFile("a.png", "image/png", "/cache/a.png")))),
    ).jsonObject["message"]!!.jsonObject
    assertNull(message["files"]!!.jsonArray[0].jsonObject["draft_id"])
  }

  @Test
  fun removingAndDiscardingNameThePlace() {
    val remove = Json.parseToJsonElement(ChatActions.removeDraftFile("7", null, copy.id)).jsonObject
    assertEquals("remove_draft_file", remove["action"]!!.jsonPrimitive.content)
    assertEquals(JsonNull, remove["maybe_thread"])
    assertEquals(copy.id, remove["id"]!!.jsonPrimitive.content)
    val discard = Json.parseToJsonElement(ChatActions.discardDraft("7", "9")).jsonObject
    assertEquals("discard_draft", discard["action"]!!.jsonPrimitive.content)
    assertEquals("9", discard["maybe_thread"]!!.jsonPrimitive.content)
    val query = Json.parseToJsonElement(ChatQueries.draftFiles("7", null)).jsonObject
    assertEquals("draft_files", query["kind"]!!.jsonPrimitive.content)
  }

  @Test
  fun theDraftsFilesDecodeWithTheirState() {
    val files = decodeDraftFiles(
      """
      {"answer":"draft_files","files":[
        {"id":"a","file_name":"a.png","media_type":"image/png","size_bytes":3,"thumbnail":[1,255],"state":{"state":"ready"}},
        {"id":"b","file_name":"b.pdf","media_type":"application/pdf","size_bytes":9,"state":{"state":"failed"}},
        {"id":"c","file_name":"c.pdf","media_type":"application/pdf","size_bytes":9,"state":{"state":"expired"}},
        {"id":"d","file_name":"d.pdf","media_type":"application/pdf","size_bytes":9,"state":{"state":"on_this_device"}},
        {"id":"e","file_name":"e.pdf","media_type":"application/pdf","size_bytes":9,"state":{"state":"from_the_future"}}
      ]}
      """.trimIndent(),
    )
    assertEquals(listOf(DraftFileState.READY, DraftFileState.FAILED, DraftFileState.EXPIRED, DraftFileState.ON_THIS_DEVICE, DraftFileState.UNKNOWN), files.map { it.state })
    assertEquals(listOf(true, false, false, true, false), files.map { it.isSendable })
    assertEquals(listOf<Byte>(1, -1), files[0].maybeThumbnail!!.bytes.toList())
  }

  @Test
  fun aDraftCountsItsFiles() {
    val answer = decodeAnswer("""{"answer":"drafts","drafts":[{"conversation":"7","conversation_name":"general","body":"","edited_at":1,"file_count":2}]}""")
    assertEquals(2, answer.drafts[0].fileCount)
    val older = decodeAnswer("""{"answer":"drafts","drafts":[{"conversation":"7","conversation_name":"general","body":"x","edited_at":1}]}""")
    assertEquals(0, older.drafts[0].fileCount)
  }

  @Test
  fun theCoreGetsASecondNameOfAStagedFile() {
    val staging = Files.createTempDirectory("staging").toFile()
    try {
      val staged = File(staging, "1-a.png").apply { writeText("picture") }
      val draft = DraftFiles.copyFor(PickedFile("a.png", "image/png", staged.path), staging, copy.id)
      assertEquals(copy.id, draft.id)
      assertTrue(draft.path != staged.path)
      // The core deletes its name once the file is up: the composer's stays.
      assertTrue(File(draft.path).delete())
      assertEquals("picture", staged.readText())
    } finally {
      staging.deleteRecursively()
    }
  }

  @Test
  fun aFileOutsideTheStagingFolderGoesAsItIs() {
    val staging = Files.createTempDirectory("staging").toFile()
    val elsewhere = Files.createTempFile("picked", ".pdf").toFile()
    try {
      val draft = DraftFiles.copyFor(PickedFile("a.pdf", "application/pdf", elsewhere.path), staging)
      assertEquals(elsewhere.path, draft.path)
      assertEquals(0, staging.listFiles()!!.size)
    } finally {
      staging.deleteRecursively()
      elsewhere.delete()
    }
  }

  @Test
  fun filesSentOrRemovedWhileTheirCopyWasMadeAreNotAttached() {
    val a = PickedFile("a.png", "image/png", "/staging/1-a.png")
    val b = PickedFile("b.png", "image/png", "/staging/2-b.png")
    val c = PickedFile("c.pdf", "application/pdf", "/elsewhere/c.pdf")
    val copyA = DraftCopy("a".repeat(32), "/staging/draft-a")
    val copyB = DraftCopy("b".repeat(32), "/staging/draft-b")
    val copyC = DraftCopy("c".repeat(32), c.path)
    // `b` and `c` were removed meanwhile; `a` waits.
    val merged = DraftFiles.merge(listOf(a), listOf(a to copyA, b to copyB, c to copyC))
    assertEquals(listOf(a.copy(maybeDraft = copyA)), merged.files)
    assertEquals(merged.files, merged.attached)
    // `c`'s copy is the file itself: nothing more to delete.
    assertEquals(listOf(copyB), merged.dropped)
  }

  @Test
  fun aFileInTheDraftAlreadyIsNotAttachedAgain() {
    val a = PickedFile("a.png", "image/png", "/staging/1-a.png", maybeDraft = copy)
    val merged = DraftFiles.merge(listOf(a), listOf(a.copy(maybeDraft = null) to DraftCopy("f".repeat(32), "/staging/draft-f")))
    assertEquals(listOf(a), merged.files)
    assertTrue(merged.attached.isEmpty())
    assertEquals(listOf("/staging/draft-f"), merged.dropped.map { it.path })
  }

  @Test
  fun theComposerShowsOnlyTheDraftsFilesItDidntPickNorLetGo() {
    val mine = PickedFile("a.png", "image/png", "/staging/1-a.png", maybeDraft = copy)
    val items = listOf(
      DraftFileItem(id = copy.id, fileName = "a.png", mediaType = "image/png"),
      DraftFileItem(id = "other", fileName = "b.pdf", mediaType = "application/pdf"),
      DraftFileItem(id = "removed", fileName = "c.pdf", mediaType = "application/pdf"),
    )
    assertEquals(listOf("other"), DraftFiles.others(items, listOf(mine), listOf("removed")).map { it.id })
  }

  @Test
  fun afterTheSendOnlyTheComposersSecondNamesGo() {
    val inDraft = PickedFile("a.png", "image/png", "/staging/1-a.png", maybeDraft = copy)
    val notLinked = PickedFile("b.pdf", "application/pdf", "/elsewhere/b.pdf", maybeDraft = DraftCopy("b".repeat(32), "/elsewhere/b.pdf"))
    val notYet = PickedFile("c.png", "image/png", "/staging/3-c.png")
    assertEquals(listOf("/staging/1-a.png"), DraftFiles.leftAfterSend(listOf(inDraft, notLinked, notYet)))
    assertEquals(listOf("/staging/1-a.png", "/staging/draft-0123"), DraftFiles.pathsOfRemoved(inDraft))
    assertFalse(DraftFiles.pathsOfRemoved(notYet).size > 1)
  }
}
