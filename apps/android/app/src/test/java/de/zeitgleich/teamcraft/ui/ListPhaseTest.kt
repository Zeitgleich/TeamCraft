package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.ListLoad
import de.zeitgleich.teamcraft.core.LoadFailure
import org.junit.Assert.assertEquals
import org.junit.Test

/** A list says "nothing here" only once it is known to be empty (#297). */
class ListPhaseTest {
  @Test
  fun aListNotReadYetShowsPlaceholders() {
    assertEquals(ListPhase.Skeleton, listPhase(Read.Loading as Read<List<String>>))
  }

  @Test
  fun aKnownEmptyListIsEmpty() {
    assertEquals(ListPhase.Empty, listPhase(Read.Loaded(emptyList<String>())))
  }

  @Test
  fun anEmptyListDuringTheFirstSyncStillLoads() {
    assertEquals(ListPhase.Skeleton, listPhase(Read.Loaded(emptyList<String>()), isSyncing = true))
  }

  @Test
  fun rowsShowWhateverElseIsGoingOn() {
    assertEquals(ListPhase.Rows, listPhase(Read.Loaded(listOf("a")), isSyncing = true))
  }

  @Test
  fun aFailedReadIsNotEmpty() {
    assertEquals(
      ListPhase.Failed(LoadFailure.OTHER),
      listPhase(Read.Failed(LoadFailure.OTHER) as Read<List<String>>),
    )
  }

  @Test
  fun aFailedRefreshKeepsTheRowsAlreadyRead() {
    val read = Read.Loaded(listOf("a")).after(null)
    assertEquals(ListPhase.Rows, listPhase(read))
  }

  @Test
  fun aFailedFirstReadFails() {
    assertEquals(Read.Failed(LoadFailure.OTHER), Read.Loading.after<List<String>>(null))
  }

  @Test
  fun aFreshReadReplacesTheFailure() {
    val read = Read.Failed(LoadFailure.OTHER).after(emptyList<String>())
    assertEquals(Read.Loaded(emptyList<String>()), read)
  }

  @Test
  fun theCoresStatusDecidesAnEmptyListOnly() {
    assertEquals(ListPhase.Skeleton, listPhase(ListLoad.Loading, isEmpty = true))
    assertEquals(ListPhase.Empty, listPhase(ListLoad.Ready, isEmpty = true))
    assertEquals(
      ListPhase.Failed(LoadFailure.NETWORK),
      listPhase(ListLoad.Failed(LoadFailure.NETWORK), isEmpty = true),
    )
    for (load in listOf(ListLoad.Loading, ListLoad.Ready, ListLoad.Failed(LoadFailure.SERVER))) {
      assertEquals(ListPhase.Rows, listPhase(load, isEmpty = false))
    }
  }

  @Test
  fun mapKeepsLoadingAndFailed() {
    assertEquals(Read.Loading, (Read.Loading as Read<List<Int>>).map { it.size })
    assertEquals(Read.Loaded(2), Read.Loaded(listOf(1, 2)).map { it.size })
  }
}
