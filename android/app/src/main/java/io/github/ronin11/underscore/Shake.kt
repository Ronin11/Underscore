package io.github.ronin11.underscore

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import kotlin.math.sqrt

/**
 * Hears the phone being shaken: two firm jolts within a second, as in
 * Audible. Listens only while [listen] is on, so the accelerometer is not
 * kept running all night. Events arrive on the main thread.
 */
class Shake(private val context: Context, private val onShake: () -> Unit) : SensorEventListener {
    private val sensors = context.getSystemService(Context.SENSOR_SERVICE) as SensorManager
    private val accelerometer: Sensor? = sensors.getDefaultSensor(Sensor.TYPE_ACCELEROMETER)
    private var listening = false
    private var lastJoltNs = 0L
    private var lastShakeNs = 0L

    /** Start or stop listening. Cheap; safe to repeat. */
    fun listen(on: Boolean) {
        if (on == listening || accelerometer == null) return
        listening = on
        if (on) sensors.registerListener(this, accelerometer, SensorManager.SENSOR_DELAY_UI) else sensors.unregisterListener(this)
    }

    override fun onSensorChanged(event: SensorEvent) {
        val (x, y, z) = event.values
        val g = sqrt(x * x + y * y + z * z) / SensorManager.GRAVITY_EARTH
        if (g < JOLT_G) return
        val at = event.timestamp
        val sinceJolt = at - lastJoltNs
        lastJoltNs = at
        // One jolt is a bump; a second soon after, but not the same one, is a shake.
        if (sinceJolt !in MIN_APART_NS..MAX_APART_NS || at - lastShakeNs < QUIET_NS) return
        lastShakeNs = at
        buzz()
        onShake()
    }

    override fun onAccuracyChanged(sensor: Sensor, accuracy: Int) {}

    /** A short buzz, so a shake in the dark is known to have landed. */
    private fun buzz() {
        val vibrator = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            (context.getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as VibratorManager).defaultVibrator
        } else {
            @Suppress("DEPRECATION")
            context.getSystemService(Context.VIBRATOR_SERVICE) as Vibrator
        }
        runCatching { vibrator.vibrate(VibrationEffect.createOneShot(80, VibrationEffect.DEFAULT_AMPLITUDE)) }
    }

    private companion object {
        /** How hard a jolt is, in multiples of gravity. Walking stays well under. */
        const val JOLT_G = 2.5f
        const val MIN_APART_NS = 100_000_000L
        const val MAX_APART_NS = 1_000_000_000L
        /** One shake at a time: ignore more for this long. */
        const val QUIET_NS = 2_000_000_000L
    }
}
