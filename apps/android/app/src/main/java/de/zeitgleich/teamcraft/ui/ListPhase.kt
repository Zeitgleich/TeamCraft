package de.zeitgleich.teamcraft.ui

import androidx.annotation.StringRes
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ListLoad
import de.zeitgleich.teamcraft.core.LoadFailure

/**
 * What a list shows instead of, or as, its rows (issue #297): the empty state is a statement about
 * the list, so it only comes once the list is known to be empty.
 */
sealed interface ListPhase {
  /** The rows it has: cached rows also while they are refreshed, or when that failed. */
  data object Rows : ListPhase

  /** Nothing read yet, or the first sync is still bringing the data: placeholders. */
  data object Skeleton : ListPhase

  data object Empty : ListPhase

  data class Failed(val failure: LoadFailure) : ListPhase
}

/** A read of the store: not answered yet, answered, or could not be answered. */
sealed interface Read<out T> {
  data object Loading : Read<Nothing>

  data class Loaded<T>(val value: T) : Read<T>

  data class Failed(val failure: LoadFailure) : Read<Nothing>
}

/**
 * The state after a read ended with [result] (`null` when it failed). Rows already read stay when a
 * refresh fails: an offline device keeps what it had instead of swapping it for an error.
 */
fun <T> Read<T>.after(result: T?): Read<T> = when {
  result != null -> Read.Loaded(result)
  this is Read.Loaded -> this
  else -> Read.Failed(LoadFailure.OTHER)
}

fun <T, R> Read<T>.map(transform: (T) -> R): Read<R> = when (this) {
  is Read.Loaded -> Read.Loaded(transform(value))
  is Read.Loading -> this
  is Read.Failed -> this
}

/**
 * The phase of a list read from the store. [isSyncing] is the first sync still bringing the newest
 * messages: an empty list is not known to be empty before that is done.
 */
fun <T> listPhase(read: Read<List<T>>, isSyncing: Boolean = false): ListPhase = when (read) {
  is Read.Loading -> ListPhase.Skeleton
  is Read.Failed -> ListPhase.Failed(read.failure)
  is Read.Loaded -> when {
    read.value.isNotEmpty() -> ListPhase.Rows
    isSyncing -> ListPhase.Skeleton
    else -> ListPhase.Empty
  }
}

/** The phase of a list whose core reports its status ([ListLoad]), and whether it has rows. */
fun listPhase(load: ListLoad, isEmpty: Boolean): ListPhase = when {
  !isEmpty -> ListPhase.Rows
  load is ListLoad.Loading -> ListPhase.Skeleton
  load is ListLoad.Failed -> ListPhase.Failed(load.failure)
  else -> ListPhase.Empty
}

@StringRes
fun failureText(failure: LoadFailure): Int = when (failure) {
  LoadFailure.NETWORK -> R.string.list_failed_network
  LoadFailure.SERVER -> R.string.list_failed_server
  LoadFailure.TIMED_OUT -> R.string.list_failed_timeout
  LoadFailure.OTHER -> R.string.list_failed_other
}
