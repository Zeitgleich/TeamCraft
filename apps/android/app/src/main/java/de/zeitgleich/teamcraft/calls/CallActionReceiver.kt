package de.zeitgleich.teamcraft.calls

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import de.zeitgleich.teamcraft.TeamCraftApplication

/** "Decline" on the incoming-call notification: stops ringing on this device. */
class CallActionReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    if (intent.action == ACTION_DECLINE) TeamCraftApplication.graph(context).calls.decline()
  }

  companion object {
    const val ACTION_DECLINE = "de.zeitgleich.teamcraft.calls.DECLINE"
  }
}
