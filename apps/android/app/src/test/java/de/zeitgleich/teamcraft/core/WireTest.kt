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
  }
}
