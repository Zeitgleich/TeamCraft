package de.zeitgleich.teamcraft.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Decodes samples in the shape teamcraft-client-api serializes them. */
class WireTest {
  @Test
  fun snapshotAndMessagesDecode() {
    val snapshot = WireJson.decodeFromString(
      ChatSnapshot.serializer(),
      """
      {"me":{"id":"1","username":"alice","display_name":"Alice","presence":"Online","maybe_custom_status":null,
             "avatar_hue":10,"is_guest":false,"is_bot":false},
       "users":[],"spaces":[{"id":"5","name":"Company","hue":3,"unread_count":2,"mention_count":0}],
       "conversations":[{"id":"7","maybe_space":"5","kind":"Public","category":"Channels","name":"general","topic":"",
                         "member_ids":["1"],"unread_count":2,"mention_count":0,"is_favorite":false,"is_muted":false,
                         "can_post":true,"bookmarks":[]}],
       "message_counts":[["7",3]],"security":{"my_fingerprint":"ab","contacts":[]}}
      """,
    )
    assertEquals("Alice", snapshot.name("1"))
    assertEquals("general", snapshot.conversation("7")?.name)
    assertFalse(snapshot.conversation("7")!!.isDirect)

    val window = WireJson.decodeFromString(
      MessageWindow.serializer(),
      """
      {"conversation":"7","start":0,"total":2,"messages":[
        {"id":"10","conversation":"7","author":"1","kind":"User","sent_at":1700000000000,"body":"hi","is_edited":false,
         "reactions":[{"emoji":"👍","count":1,"reacted_by_me":true}],"reply_count":0,"maybe_thread":null},
        {"id":"4611686018427387905","conversation":"7","author":"1","kind":{"System":"Joined"},"sent_at":1,"body":"",
         "is_edited":false,"reactions":[],"reply_count":0,"maybe_thread":null}]}
      """,
    )
    assertFalse(window.messages[0].isPending)
    assertTrue(window.messages[1].isPending)
    assertNotNull(window.messages[1].maybeSystemEvent)
    assertEquals(null, snapshot.security.backup)
  }

  @Test
  fun theNotesFlagDecodesAndDefaultsToFalse() {
    val notes = WireJson.decodeFromString(
      Conversation.serializer(),
      """{"id":"9","kind":"Direct","name":"","is_notes_to_self":true}""",
    )
    assertTrue(notes.isNotesToSelf)
    val older = WireJson.decodeFromString(Conversation.serializer(), """{"id":"9","kind":"Direct","name":"Amy"}""")
    assertFalse(older.isNotesToSelf)
  }

  @Test
  fun theKeyBackupStateDecodesAndToleratesNewMethods() {
    val security = WireJson.decodeFromString(
      SecuritySnapshot.serializer(),
      """
      {"my_fingerprint":"ab","contacts":[],
       "backup":{"is_stored":true,"has_pending_upload":false,"recovery":["passphrase","organization","hardware_key"],
                 "passphrase_set_at":1700000000000}}
      """,
    )
    val backup = security.backup!!
    assertTrue(backup.isStored)
    assertTrue(backup.hasPassphrase)
    assertTrue(backup.hasOrganization)
    assertEquals(1_700_000_000_000L, backup.passphraseSetAt)
  }

  @Test
  fun formerAccountsNameOldMessagesAndDeletedOnesGetTheAppsName() {
    val snapshot = WireJson.decodeFromString(
      ChatSnapshot.serializer(),
      """
      {"me":{"id":"1","username":"alice","display_name":"Alice"},
       "users":[{"id":"2","username":"sam","display_name":"Sam Doe"}],
       "former_users":[{"id":"3","username":"bea","display_name":"Bea Former","is_deactivated":true},
                       {"id":"4","username":"","display_name":"Deleted user","is_deactivated":true,"is_deleted":true}]}
      """,
    ).namingDeletedUsers("Gelöschter Nutzer")
    assertEquals(listOf("2"), snapshot.users.map { it.id })
    assertEquals("Bea Former", snapshot.name("3"))
    assertEquals("Gelöschter Nutzer", snapshot.name("4"))
    assertEquals("", snapshot.user("4")?.username)
    assertFalse(snapshot.user("3")!!.isOffered)
    assertTrue(snapshot.user("2")!!.isOffered)
  }

  @Test
  fun answersDecodeWithTransfersAndUnknownFields() {
    val answer = decodeAnswer(
      """
      {"answer":"message_extras","extras":[{"message":"10","priority":"urgent","is_pinned":true,"future_field":1,
        "files":[{"conversation":"7","message":"10","file_id":"f","file_name":"a.pdf","media_type":"application/pdf",
                  "size_bytes":200,"transfer":{"state":"uploading","done_bytes":50,"total_bytes":200}}]}]}
      """,
    )
    val extras = answer.extras.single()
    assertTrue(extras.isPinned)
    assertEquals(0.25f, extras.files.single().transfer.maybeProgress!!, 0.001f)
    assertTrue(!extras.isSendFailed)
  }

  @Test
  fun aRejectedMessageDecodesAsFailed() {
    val answer = decodeAnswer("""{"answer":"message_extras","extras":[{"message":"10","is_send_failed":true}]}""")
    assertTrue(answer.extras.single().isSendFailed)
  }

  @Test
  fun thumbnailBytesDecodeFromTheirNumbersAndCompareByContent() {
    val file = """
      {"conversation":"7","message":"10","file_id":"3","file_name":"a.jpg","media_type":"image/jpeg","size_bytes":9,
       "maybe_thumbnail_png":[255,216,0,127,128],"transfer":{"state":"remote"}}
    """
    val decoded = WireJson.decodeFromString(FileInfo.serializer(), file)
    val bytes = decoded.maybeThumbnail!!.bytes
    assertEquals(listOf(-1, -40, 0, 127, -128), bytes.map { it.toInt() })
    // A re-read of the same message is equal, so its row doesn't recompose.
    assertEquals(decoded, WireJson.decodeFromString(FileInfo.serializer(), file))
    assertEquals(
      "[255,216,0,127,128]",
      WireJson.encodeToString(ImageBytesSerializer, decoded.maybeThumbnail),
    )
  }

  @Test
  fun aFileNamesItsSenderAndTimeForTheViewer() {
    val decoded = WireJson.decodeFromString(
      FileInfo.serializer(),
      """{"conversation":"7","message":"10","file_id":"3","file_name":"a.jpg","media_type":"image/jpeg",
          "size_bytes":9,"transfer":{"state":"remote"},"maybe_sender":"42","maybe_sent_at":1700000000000}""",
    )
    assertEquals("42", decoded.maybeSender)
    assertEquals(1_700_000_000_000L, decoded.maybeSentAt)
    val older = WireJson.decodeFromString(
      FileInfo.serializer(),
      """{"conversation":"7","message":"10","file_id":"3","file_name":"a.jpg","media_type":"image/jpeg",
          "size_bytes":9,"transfer":{"state":"remote"}}""",
    )
    assertEquals(null, older.maybeSender)
    assertEquals(null, older.maybeSentAt)
  }

  @Test
  fun aFileWithoutThumbnailHasNone() {
    val decoded = WireJson.decodeFromString(
      FileInfo.serializer(),
      """{"conversation":"7","message":"10","file_id":"3","file_name":"a.pdf","media_type":"application/pdf",
          "size_bytes":9,"maybe_thumbnail_png":null,"transfer":{"state":"remote"}}""",
    )
    assertEquals(null, decoded.maybeThumbnail)
  }

  @Test
  fun uploadRetriesAndStallsSayWhy() {
    fun transfer(json: String) = WireJson.decodeFromString(FileTransfer.serializer(), json)
    assertTrue(transfer("""{"state":"uploading","done_bytes":0,"total_bytes":9,"failed_attempts":2}""").isRetrying)
    assertFalse(transfer("""{"state":"uploading","done_bytes":3,"total_bytes":9}""").isRetrying)
    val down = transfer("""{"state":"upload_stalled","reason":"storage_unavailable"}""")
    assertTrue(down.isStorageUnavailable)
    assertTrue(down.isCancellable)
    assertFalse(transfer("""{"state":"upload_stalled","reason":"network"}""").isStorageUnavailable)
  }

  @Test
  fun theOwnProfileKeepsItsFieldsWhenOnlyTheNameChanges() {
    val profile = OwnProfile.parse(
      """{"user_id":"1","title":"Lead","phone":"+49 30 1","has_avatar":true,"avatar_version":"7",
          "display_name_override":"Ada","can_override_display_name":true}""",
    )
    assertTrue(profile.canOverrideDisplayName)
    assertEquals("Ada", profile.maybeDisplayNameOverride)
    assertEquals("""{"title":"Lead","phone":"+49 30 1","display_name":"Grace"}""", profile.updateBody("Grace"))
    assertEquals("""{"title":"Lead","phone":"+49 30 1"}""", profile.updateBody(null))
  }

  @Test
  fun anOlderServerOffersNoNameChange() {
    val profile = OwnProfile.parse("""{"user_id":"1","has_avatar":false}""")
    assertFalse(profile.canOverrideDisplayName)
    assertEquals(null, profile.maybeDisplayNameOverride)
  }
}
