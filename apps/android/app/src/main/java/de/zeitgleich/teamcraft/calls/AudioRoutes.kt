package de.zeitgleich.teamcraft.calls

import androidx.core.telecom.CallEndpointCompat

/** What kind of place a call's sound can go to. */
enum class AudioRouteKind { EARPIECE, SPEAKER, BLUETOOTH, WIRED, OTHER }

/** One place the call's sound can go to now (issue #2060), as Telecom lists it. */
data class AudioRoute(val id: String, val kind: AudioRouteKind, val name: String)

internal fun routeKindOf(endpointType: Int): AudioRouteKind = when (endpointType) {
  CallEndpointCompat.TYPE_EARPIECE -> AudioRouteKind.EARPIECE
  CallEndpointCompat.TYPE_SPEAKER -> AudioRouteKind.SPEAKER
  CallEndpointCompat.TYPE_BLUETOOTH -> AudioRouteKind.BLUETOOTH
  CallEndpointCompat.TYPE_WIRED_HEADSET -> AudioRouteKind.WIRED
  else -> AudioRouteKind.OTHER
}

/**
 * Whether the call screen offers a list to choose from instead of the speaker switch: with only
 * the earpiece and the loudspeaker, one switch says it all; with a headset or more, the user picks.
 */
fun needsRoutePicker(routes: List<AudioRoute>): Boolean = routes.size > 2

/** The first route of [kind], for the switch's two states. */
internal fun List<AudioRoute>.firstOfKind(kind: AudioRouteKind): AudioRoute? = firstOrNull { it.kind == kind }
