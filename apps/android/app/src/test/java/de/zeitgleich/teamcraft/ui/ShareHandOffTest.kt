package de.zeitgleich.teamcraft.ui

import java.util.concurrent.ConcurrentLinkedQueue
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A share reaches a composer exactly once. A direct share opened the conversation several times,
 * and each copy of the screen appended the link to the draft the other one had saved.
 */
class ShareHandOffTest {
  @Test
  fun theFirstClaimTakesTheShareAndLaterOnesGetNothing() {
    val handOff = ShareHandOff()
    assertTrue(handOff.offer(CONVERSATION, SHARE))

    assertEquals(SHARE, handOff.claim(CONVERSATION))
    assertNull(handOff.claim(CONVERSATION))
    assertNull(handOff.current.value)
  }

  @Test
  fun anotherConversationCannotClaimIt() {
    val handOff = ShareHandOff()
    handOff.offer(CONVERSATION, SHARE)

    assertNull(handOff.claim(OTHER_CONVERSATION))
    assertEquals(SHARE, handOff.claim(CONVERSATION))
  }

  @Test
  fun theSameShareIsOfferedOnlyOnce() {
    val handOff = ShareHandOff()
    assertTrue(handOff.offer(CONVERSATION, SHARE))
    handOff.claim(CONVERSATION)

    // A repeated pick after the composer took it.
    assertFalse(handOff.offer(CONVERSATION, SHARE))
    assertNull(handOff.claim(CONVERSATION))
  }

  @Test
  fun theFilesThatFailedComeBackAsANewOffer() {
    val handOff = ShareHandOff()
    handOff.offer(CONVERSATION, SHARE)
    handOff.claim(CONVERSATION)
    val retry = SHARE.copy(text = "", files = listOf(FILE))

    assertTrue(handOff.offer(CONVERSATION, retry))
    assertEquals(retry, handOff.claim(CONVERSATION))
  }

  @Test
  fun concurrentClaimsHandItOutOnce() {
    val handOff = ShareHandOff()
    handOff.offer(CONVERSATION, SHARE)
    val taken = ConcurrentLinkedQueue<StagedShare>()

    val threads = (1..8).map { Thread { handOff.claim(CONVERSATION)?.let(taken::add) } }
    threads.forEach(Thread::start)
    threads.forEach(Thread::join)

    assertEquals(listOf(SHARE), taken.toList())
  }

  @Test
  fun clearingForgetsTheWaitingShareAndTheOffers() {
    val handOff = ShareHandOff()
    handOff.offer(CONVERSATION, SHARE)

    handOff.clear()

    assertNull(handOff.claim(CONVERSATION))
    assertTrue(handOff.offer(CONVERSATION, SHARE))
  }

  private companion object {
    const val CONVERSATION = "100000000000000001"
    const val OTHER_CONVERSATION = "100000000000000002"
    val FILE = StagedShareFile(name = "photo.jpg", mediaType = "image/jpeg", storedAs = "0")
    val SHARE = StagedShare(id = "0000000000000001-a", text = "Example\nhttps://example.com/", files = emptyList(), leftOut = 0)
  }
}
