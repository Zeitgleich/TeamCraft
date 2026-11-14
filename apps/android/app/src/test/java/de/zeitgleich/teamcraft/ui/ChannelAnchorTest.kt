package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.teamcraft_ffi.ChannelAnchorForkItem
import uniffi.teamcraft_ffi.ChannelDeleteNameNote
import uniffi.teamcraft_ffi.ChannelNameProblem

class ChannelAnchorTest {
  @Test
  fun eachNameProblemHasItsTextAndAnEmptyNameTheForms() {
    assertNull(channelNameProblemText(ChannelNameProblem.EMPTY))
    assertEquals(R.string.channel_name_error_bidi, channelNameProblemText(ChannelNameProblem.BIDI_CONTROL))
    assertEquals(R.string.channel_name_error_control, channelNameProblemText(ChannelNameProblem.CONTROL_CHARACTER))
    assertEquals(R.string.channel_name_error_invisible, channelNameProblemText(ChannelNameProblem.INVISIBLE_CHARACTER))
  }

  @Test
  fun theDeleteNoteNamesWhoLastRenamed() {
    assertEquals(
      DeleteNoteText(R.string.channel_delete_renamed_by_member, takesName = true, maybeName = "Anna"),
      channelDeleteNoteText(ChannelDeleteNameNote.RenamedByMember("Anna")),
    )
    assertEquals(
      DeleteNoteText(R.string.channel_delete_named_by_founder, takesName = true, maybeName = null),
      channelDeleteNoteText(ChannelDeleteNameNote.NamedByFounder(null)),
    )
    assertEquals(
      DeleteNoteText(R.string.channel_delete_name_stale, takesName = false, maybeName = null),
      channelDeleteNoteText(ChannelDeleteNameNote.NameStale),
    )
  }

  @Test
  fun aForkIsNamedByItsSide() {
    assertEquals(R.string.channel_anchor_forked_founding, channelAnchorForkText(ChannelAnchorForkItem.FOUNDING))
    assertEquals(R.string.channel_anchor_forked_properties, channelAnchorForkText(ChannelAnchorForkItem.PROPERTIES))
    assertEquals(R.string.channel_anchor_forked, channelAnchorForkText(ChannelAnchorForkItem.UNKNOWN))
  }
}
