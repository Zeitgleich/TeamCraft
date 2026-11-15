package de.zeitgleich.teamcraft.ui

import androidx.compose.foundation.layout.BoxScope
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.pulltorefresh.PullToRefreshDefaults
import androidx.compose.material3.pulltorefresh.PullToRefreshState
import androidx.compose.material3.pulltorefresh.pullToRefresh
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import kotlinx.coroutines.launch

/**
 * Pull to refresh (#2006): the gesture asks the core to check its gateway connection and catch
 * up at once, instead of waiting for its backoff, and ends when that finished or failed. Null
 * [refresh] means the screen has none.
 */
@OptIn(ExperimentalMaterial3Api::class)
class PullRefresh(val state: PullToRefreshState, val isRefreshing: Boolean, val onRefresh: () -> Unit)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun rememberPullRefresh(refresh: (suspend () -> Unit)?): PullRefresh? {
  val state = rememberPullToRefreshState()
  val scope = rememberCoroutineScope()
  var isRefreshing by remember { mutableStateOf(false) }
  if (refresh == null) return null
  return PullRefresh(state, isRefreshing) {
    if (!isRefreshing) {
      isRefreshing = true
      scope.launch {
        try {
          refresh()
        } finally {
          isRefreshing = false
        }
      }
    }
  }
}

@OptIn(ExperimentalMaterial3Api::class)
fun Modifier.pullRefresh(maybeRefresh: PullRefresh?): Modifier =
  if (maybeRefresh == null) this else pullToRefresh(maybeRefresh.isRefreshing, maybeRefresh.state, onRefresh = maybeRefresh.onRefresh)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun BoxScope.PullRefreshIndicator(maybeRefresh: PullRefresh?) {
  if (maybeRefresh == null) return
  PullToRefreshDefaults.Indicator(
    state = maybeRefresh.state,
    isRefreshing = maybeRefresh.isRefreshing,
    modifier = Modifier.align(Alignment.TopCenter),
  )
}
