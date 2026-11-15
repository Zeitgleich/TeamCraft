package de.zeitgleich.teamcraft.ui

import android.annotation.SuppressLint
import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.PowerManager

private const val WAKE_LOCK_TAG = "teamcraft:voice-proximity"

/**
 * Holding the phone to the ear while a voice message plays (#2068): the proximity sensor is
 * listened to only while a message plays. Near, the sound moves to the earpiece ([onEarpiece]
 * true) and the screen goes off through a proximity wake lock; far, both come back. Headphones,
 * Bluetooth and other connected outputs keep the route they have.
 */
class VoiceProximity(context: Context, private val onEarpiece: (Boolean) -> Unit) {
  private val audio = context.getSystemService(AudioManager::class.java)
  private val sensors = context.getSystemService(SensorManager::class.java)
  private val maybeSensor: Sensor? = sensors?.getDefaultSensor(Sensor.TYPE_PROXIMITY)
  private val maybeLock: PowerManager.WakeLock? =
    context.getSystemService(PowerManager::class.java)?.let { power ->
      if (power.isWakeLockLevelSupported(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK)) {
        power.newWakeLock(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK, WAKE_LOCK_TAG).apply { setReferenceCounted(false) }
      } else {
        null
      }
    }
  private var isListening = false

  private val listener = object : SensorEventListener {
    override fun onSensorChanged(event: SensorEvent) {
      val sensor = maybeSensor ?: return
      val near = event.values.firstOrNull()?.let { isNearReading(it, sensor.maximumRange) } ?: return
      update(near)
    }

    override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) = Unit
  }

  /** A message plays: start following the sensor. */
  fun start() {
    val sensor = maybeSensor ?: return
    if (isListening) return
    isListening = true
    sensors?.registerListener(listener, sensor, SensorManager.SENSOR_DELAY_NORMAL)
  }

  /** Playback paused or ended: everything goes back to the speaker and the screen. */
  fun stop() {
    if (!isListening) return
    isListening = false
    sensors?.unregisterListener(listener)
    update(false)
  }

  @SuppressLint("WakelockTimeout") // Released when the phone moves away or playback stops.
  private fun update(near: Boolean) {
    val useEarpiece = shouldUseEarpiece(near, hasExternalOutput())
    onEarpiece(useEarpiece)
    val lock = maybeLock ?: return
    if (useEarpiece && !lock.isHeld) lock.acquire()
    if (!useEarpiece && lock.isHeld) lock.release()
  }

  private fun hasExternalOutput(): Boolean =
    audio?.getDevices(AudioManager.GET_DEVICES_OUTPUTS).orEmpty().any { isExternalOutput(it.type) }
}

/** The earpiece is used only while the phone is at the ear and nothing was connected to play on. */
fun shouldUseEarpiece(isNear: Boolean, hasExternalOutput: Boolean): Boolean = isNear && !hasExternalOutput

/** A reading is near when it is below the sensor's range (many sensors report only 0 and the maximum). */
fun isNearReading(distance: Float, maximumRange: Float): Boolean = distance < maximumRange && distance < 5f

/** Outputs the user connected: wired and USB headsets, Bluetooth, hearing aids. */
fun isExternalOutput(type: Int): Boolean = when (type) {
  AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
  AudioDeviceInfo.TYPE_WIRED_HEADSET,
  AudioDeviceInfo.TYPE_USB_HEADSET,
  AudioDeviceInfo.TYPE_USB_DEVICE,
  AudioDeviceInfo.TYPE_BLUETOOTH_A2DP,
  AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
  AudioDeviceInfo.TYPE_BLE_HEADSET,
  AudioDeviceInfo.TYPE_BLE_SPEAKER,
  AudioDeviceInfo.TYPE_HEARING_AID,
  -> true
  else -> false
}
