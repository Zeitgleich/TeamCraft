package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.Reaction

/**
 * Who is behind a reaction (issue #2176), as far as this device knows them: whether the viewer is
 * among them, the others it can name in order, and the total. People it can't name (departed,
 * unknown) only count towards the total; an id is never shown.
 */
data class ReactionPeople(val hasMe: Boolean, val others: List<String>, val total: Int) {
  /** How many people have a name to show. */
  val named: Int get() = (if (hasMe) 1 else 0) + others.size

  /** People without a name on this device. */
  val unnamed: Int get() = (total - named).coerceAtLeast(0)
}

/** The people of [reaction]; the core lists the viewer first, which `reactedByMe` confirms. */
fun reactionPeople(reaction: Reaction, nameOf: (String) -> String?): ReactionPeople {
  val hasMe = reaction.reactedByMe && reaction.reactors.isNotEmpty()
  val others = reaction.reactors.drop(if (hasMe) 1 else 0).mapNotNull(nameOf)
  return ReactionPeople(hasMe, others, reaction.count)
}
