package de.zeitgleich.teamcraft.calls

/**
 * The title of a car list row: the name the phone's lists show ([de.zeitgleich.teamcraft.ui.listName]),
 * or [unnamedTitle] when that is blank. The car library throws on an empty title (#1961), and a
 * conversation can have no name (not decrypted yet, or named after members that are not known).
 */
fun carRowTitle(listName: String, unnamedTitle: String): String = listName.ifBlank { unnamedTitle }
