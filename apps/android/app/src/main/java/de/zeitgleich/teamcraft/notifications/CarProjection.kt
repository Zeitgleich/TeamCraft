package de.zeitgleich.teamcraft.notifications

import android.content.Context
import android.util.Log
import androidx.car.app.connection.CarConnection

/**
 * Whether the phone projects to a car (Android Auto) right now, for how a placeholder alerts
 * ([PushRetry.releasesHeldAlert], [MessageNotificationSpecs.alerting]). Follows the Car App
 * Library's connection state, which Android Auto reports; nothing is polled.
 */
class CarProjection(private val context: Context) {
  @Volatile
  var isActive: Boolean = false
    private set

  /**
   * Starts following; on the main thread (the library's LiveData asks for that). `onEnded`: the
   * phone stopped projecting.
   */
  fun start(onEnded: () -> Unit) {
    runCatching {
      CarConnection(context).type.observeForever { type ->
        val wasActive = isActive
        isActive = type == CarConnection.CONNECTION_TYPE_PROJECTION
        if (wasActive && !isActive) onEnded()
      }
    }.onFailure {
      // Only the car's extra rule is lost: placeholders then alert as off the road.
      Log.w(TAG, "car connection not followed: ${it.javaClass.simpleName}")
    }
  }

  private companion object {
    const val TAG = "TeamCraftNotify"
  }
}
