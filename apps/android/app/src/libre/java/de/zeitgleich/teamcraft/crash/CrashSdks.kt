package de.zeitgleich.teamcraft.crash

/** Builds without Google services (F-Droid style) carry no crash SDK. */
object CrashSdks {
  fun platform(): CrashSdk = NoCrashSdk
}
