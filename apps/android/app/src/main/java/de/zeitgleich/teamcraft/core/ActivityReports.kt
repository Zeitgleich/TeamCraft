package de.zeitgleich.teamcraft.core

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import uniffi.teamcraft_ffi.AppActivity

/**
 * Presence (issue #472): the app's way to the foreground and to the background reaches the core,
 * which tells the server. Coming back ends an away timeout the server applied meanwhile at once;
 * touches are reported by `MainActivity.onUserInteraction`.
 *
 * Registered after the sync observer: the process lifecycle calls observers in reverse order on
 * the way down, so the idle report is queued before the sync stops.
 */
class ActivityReports(private val core: CoreRepository) : DefaultLifecycleObserver {
  override fun onStart(owner: LifecycleOwner) = core.reportActivity(AppActivity.FOREGROUND)

  override fun onStop(owner: LifecycleOwner) = core.reportActivity(AppActivity.BACKGROUND)
}
