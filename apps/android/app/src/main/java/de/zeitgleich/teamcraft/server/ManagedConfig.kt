package de.zeitgleich.teamcraft.server

import android.content.Context
import android.content.RestrictionsManager
import de.zeitgleich.teamcraft.BuildConfig

/** Keys of `res/xml/app_restrictions.xml`; part of the contract with the organization's MDM. */
const val RESTRICTION_SERVER_URL = "server_url"
const val RESTRICTION_LOCK_SERVER = "lock_server"

/**
 * The server an organization set through its device management (Android Enterprise managed
 * configuration), or else the build's default server. Read on every start, so a changed
 * configuration applies the next time the app opens.
 */
class ManagedConfig(private val context: Context) {
  /** The preset server, if any. Does a binder call: not on the UI thread. */
  fun preset(): ServerPreset? {
    val manager = context.getSystemService(Context.RESTRICTIONS_SERVICE) as? RestrictionsManager
    val restrictions = manager?.applicationRestrictions
    return choosePreset(
      maybeManagedUrl = restrictions?.getString(RESTRICTION_SERVER_URL),
      isManagedLock = restrictions?.getBoolean(RESTRICTION_LOCK_SERVER, false) == true,
      buildDefault = BuildConfig.DEFAULT_SERVER,
    )
  }
}
