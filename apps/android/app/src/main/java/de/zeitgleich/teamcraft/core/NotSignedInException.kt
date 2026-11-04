package de.zeitgleich.teamcraft.core

/**
 * No account to open a core for. Its own type, because the destroyed-object error of a closed core
 * (UniFFI) is an [IllegalStateException] too, and that one passes when the core opens again.
 */
class NotSignedInException(message: String) : IllegalStateException(message)
