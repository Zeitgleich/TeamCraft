package de.zeitgleich.teamcraft.ui

import kotlin.reflect.KProperty

/** One entry of a (state) map as a delegated variable: `var files by map.entry(key, emptyList())`. */
class MapEntry<K, V>(private val map: MutableMap<K, V>, private val key: K, private val default: V) {
  operator fun getValue(thisRef: Any?, property: KProperty<*>): V = map[key] ?: default

  operator fun setValue(thisRef: Any?, property: KProperty<*>, value: V) {
    map[key] = value
  }
}

fun <K, V> MutableMap<K, V>.entry(key: K, default: V) = MapEntry(this, key, default)
