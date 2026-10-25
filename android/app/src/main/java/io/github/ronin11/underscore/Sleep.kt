package io.github.ronin11.underscore

import android.Manifest
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.ContextCompat
import com.google.android.gms.location.ActivityRecognition
import com.google.android.gms.location.SleepClassifyEvent
import com.google.android.gms.location.SleepSegmentRequest

/**
 * One way of knowing the listener fell asleep. Each source is a switch in
 * settings. When one is sure, it calls [Sleep.report], and the player
 * winds down the same way whichever source it was. A new kind of sensor,
 * such as a watch or a bed, is a new source here, or another app that
 * sends [Sleep.ACTION_ASLEEP].
 */
interface SleepSource {
    /** The settings key for this source's switch. */
    val key: String
    val title: String
    val summary: String
    /** A permission the source needs before it can start, if any. */
    val permission: String? get() = null

    /** Begin listening. Returns why it could not, or null. Safe to repeat. */
    fun start(context: Context): String?

    /** Stop listening. Safe to call when not started. */
    fun stop(context: Context)
}

object Sleep {
    /**
     * The broadcast another app sends to say the listener is asleep. Any
     * app can send it: Tasker, Home Assistant's companion app, a smart
     * bed's app. Extras, all optional: [EXTRA_SINCE], the wall-clock time
     * in milliseconds they fell asleep, and [EXTRA_SOURCE], a name for the
     * bug report.
     */
    const val ACTION_ASLEEP = "io.github.ronin11.underscore.ASLEEP"
    const val EXTRA_SINCE = "since"
    const val EXTRA_SOURCE = "source"

    val sources: List<SleepSource> = listOf(PhoneSleep, OtherAppsSleep)

    /** Start the sources that are switched on and stop the rest. */
    fun apply(context: Context, settings: Settings): Map<String, String> {
        val problems = HashMap<String, String>()
        for (source in sources) {
            if (settings.sleepSource(source.key)) {
                source.start(context)?.let { problems[source.key] = it }
            } else {
                source.stop(context)
            }
        }
        return problems
    }

    /**
     * Tell the player the listener is asleep, and since when, if known.
     * The player ignores it unless a book is playing, and then the service
     * is in the foreground, so starting it is allowed.
     */
    fun report(context: Context, sinceWallMs: Long?, source: String) {
        val intent = Intent(context, PlaybackService::class.java)
            .setAction(PlaybackService.ACTION_ASLEEP)
            .putExtra(EXTRA_SOURCE, source)
        if (sinceWallMs != null) intent.putExtra(EXTRA_SINCE, sinceWallMs)
        // Nothing is playing when the service is not running; nothing to do.
        runCatching { context.startService(intent) }
    }
}

/**
 * The phone's own guess, from Google's sleep API: how still the phone is
 * and how dark the room is, every ten minutes. One sleepy reading is easy
 * to get lying still in the dark while listening, so it takes two in a row.
 */
object PhoneSleep : SleepSource {
    override val key = "phone"
    override val title = "This phone"
    override val summary = "Google's guess from the phone's stillness and the room's light. Checks every 10 minutes."
    override val permission: String =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) Manifest.permission.ACTIVITY_RECOGNITION
        else "com.google.android.gms.permission.ACTIVITY_RECOGNITION"

    /** A reading at or above this, out of 100, counts as asleep. */
    private const val SURE = 85
    private const val READINGS_IN_A_ROW = 2
    /** Readings further apart than this are not in a row. */
    private const val GAP_MS = 25 * 60_000L
    private const val KEEP_READINGS = 12

    override fun start(context: Context): String? {
        if (ContextCompat.checkSelfPermission(context, permission) != PackageManager.PERMISSION_GRANTED) {
            return "Needs the physical activity permission"
        }
        return runCatching {
            ActivityRecognition.getClient(context).requestSleepSegmentUpdates(
                pending(context),
                SleepSegmentRequest(SleepSegmentRequest.CLASSIFY_EVENTS_ONLY),
            )
            null
        }.getOrElse { it.message ?: it.toString() }
    }

    override fun stop(context: Context) {
        runCatching { ActivityRecognition.getClient(context).removeSleepSegmentUpdates(pending(context)) }
    }

    /** Play services fills in the readings, so the intent must be mutable. */
    private fun pending(context: Context): PendingIntent = PendingIntent.getBroadcast(
        context, 0, Intent(context, SleepReceiver::class.java),
        PendingIntent.FLAG_UPDATE_CURRENT or
            (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0),
    )

    /** The latest readings, newest last, for the bug report and for tuning [SURE]. */
    fun readings(context: Context): String = prefs(context).getString("readings", null) ?: "none yet"

    /**
     * Take new readings. Returns when the listener fell asleep, the first
     * sure reading of the run, once a run is long enough. Each run is
     * reported once.
     */
    fun heard(context: Context, events: List<SleepClassifyEvent>): Long? {
        val p = prefs(context)
        var runStart = p.getLong("run_start", 0)
        var runLength = p.getInt("run_length", 0)
        var lastAt = p.getLong("last_at", 0)
        var reported = p.getBoolean("reported", false)
        val log = (p.getString("readings", null)?.lines() ?: emptyList()).toMutableList()
        var asleep: Long? = null
        for (e in events.sortedBy { it.timestampMillis }) {
            val at = e.timestampMillis
            log += "${java.text.DateFormat.getTimeInstance().format(java.util.Date(at))} " +
                "sure ${e.confidence} still ${e.motion} dark ${e.light}"
            if (e.confidence < SURE || at - lastAt > GAP_MS) {
                runStart = 0
                runLength = 0
                reported = false
            }
            if (e.confidence >= SURE) {
                if (runLength == 0) runStart = at
                runLength++
                if (runLength >= READINGS_IN_A_ROW && !reported) {
                    reported = true
                    asleep = runStart
                }
            }
            lastAt = at
        }
        p.edit()
            .putLong("run_start", runStart).putInt("run_length", runLength)
            .putLong("last_at", lastAt).putBoolean("reported", reported)
            .putString("readings", log.takeLast(KEEP_READINGS).joinToString("\n"))
            .apply()
        return asleep
    }

    private fun prefs(context: Context) = context.getSharedPreferences("sleep_phone", Context.MODE_PRIVATE)
}

/** Any other app that sends [Sleep.ACTION_ASLEEP]. Nothing to start; the switch is the gate. */
object OtherAppsSleep : SleepSource {
    override val key = "apps"
    override val title = "Other apps"
    override val summary = "Tasker, Home Assistant, a smart bed: any app that sends ${Sleep.ACTION_ASLEEP}."

    override fun start(context: Context): String? = null

    override fun stop(context: Context) {}
}

/** Where both the sleep API's readings and other apps' broadcasts arrive. */
class SleepReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val settings = Settings(context)
        when {
            intent.action == Sleep.ACTION_ASLEEP -> {
                if (!settings.sleepSource(OtherAppsSleep.key)) return
                val since = intent.getLongExtra(Sleep.EXTRA_SINCE, 0).takeIf { it > 0 }
                Sleep.report(context, since, intent.getStringExtra(Sleep.EXTRA_SOURCE) ?: "another app")
            }
            SleepClassifyEvent.hasEvents(intent) -> {
                if (!settings.sleepSource(PhoneSleep.key)) return
                PhoneSleep.heard(context, SleepClassifyEvent.extractEvents(intent))?.let {
                    Sleep.report(context, it, "this phone")
                }
            }
        }
    }
}
