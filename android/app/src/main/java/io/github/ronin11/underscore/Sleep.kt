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
 * One way of knowing the listener fell asleep. The listener sets each up
 * from the Sleep button's Auto list, which tests it, and checks the ones to
 * use. When one is sure, it calls [Sleep.report], and the player
 * winds down the same way whichever source it was. A new kind of sensor,
 * such as a watch or a bed, is a new source here, or another app that
 * sends [Sleep.ACTION_ASLEEP].
 */
interface SleepSource {
    /** The settings key for this source's switch. */
    val key: String
    val title: String
    val summary: String
    /** What setting it up involves, shown at the top of its setup screen. */
    val howItWorks: String
    /** A permission the source needs before it can start, if any. */
    val permission: String? get() = null
    /** False for a source that is planned but not built, shown greyed out. */
    val available: Boolean get() = true

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

    val sources: List<SleepSource> = listOf(PhoneSleep, WatchSleep, OtherAppsSleep)

    /**
     * While a setup screen tests a source, what any source hears goes here,
     * with a line saying what it was, and not to the player. Main thread.
     */
    @Volatile var testing: ((SleepSource, String) -> Unit)? = null

    /** Whether [source] may wind the book down: auto is on, and it is set up and checked. */
    fun listening(settings: Settings, source: SleepSource): Boolean =
        source.available && settings.sleepAuto && settings.sleepSetUp(source.key) && settings.sleepSource(source.key)

    /** Start the sources that are listening and stop the rest. */
    fun apply(context: Context, settings: Settings): Map<String, String> {
        val problems = HashMap<String, String>()
        for (source in sources) {
            if (!source.available) continue
            if (listening(settings, source)) {
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
    override val howItWorks =
        "Google's sleep API looks at how still the phone is and how dark the room is, and sends a " +
            "reading every 10 minutes. Lying still in the dark looks a lot like sleep, so it takes " +
            "two sure readings in a row, about 20 minutes. Leave the phone near you, on the bed or " +
            "the nightstand.\n\nIt needs the physical activity permission."
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

    /** The newest reading as one line, or null before the first. */
    fun latest(context: Context): String? = prefs(context).getString("readings", null)?.lines()?.lastOrNull()

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
    override val summary = "Tasker, Home Assistant, a smart bed: any app that can send a broadcast."
    override val howItWorks =
        "Have the other app send this broadcast when you fall asleep:\n\n" +
            "${Sleep.ACTION_ASLEEP}\n\n" +
            "to the package io.github.ronin11.underscore. Two extras, both optional: " +
            "\"${Sleep.EXTRA_SINCE}\", a long, the time you fell asleep in milliseconds since 1970, and " +
            "\"${Sleep.EXTRA_SOURCE}\", a name for it. In Tasker that is a Send Intent action with " +
            "Target set to Broadcast Receiver."

    override fun start(context: Context): String? = null

    override fun stop(context: Context) {}
}

/**
 * A Wear OS watch such as a Pixel Watch, through the watch app in
 * `android/wear`. The watch's own sleep detection decides; the app sends
 * the phone a message, which [WatchListener] hears. Nothing to start here.
 */
object WatchSleep : SleepSource {
    override val key = "watch"
    override val title = "Watch"
    override val summary = "A Wear OS watch such as a Pixel Watch, with the underscore sleep app on it."
    override val howItWorks =
        "The watch notices you fell asleep from your heart rate and movement, and its " +
            "underscore sleep app tells this phone. Install that app on the watch, open it, and tap " +
            "Watch for sleep. Then test it from here or with the watch's test button."

    /** The capability the watch app declares, to find it among connected watches. */
    const val CAPABILITY = "underscore_sleep"
    /** Paths and payloads must match `Watch` in the watch app. */
    const val PATH_ASLEEP = "/underscore/asleep"
    const val PATH_PING = "/underscore/ping"
    const val PATH_STATUS = "/underscore/status"

    override fun start(context: Context): String? = null

    override fun stop(context: Context) {}

    /** Ask each watch with the app how it stands. [done] hears the watch names, or none. */
    fun ping(context: Context, done: (List<String>) -> Unit) {
        com.google.android.gms.wearable.Wearable.getCapabilityClient(context)
            .getCapability(CAPABILITY, com.google.android.gms.wearable.CapabilityClient.FILTER_REACHABLE)
            .addOnSuccessListener { info ->
                for (node in info.nodes) {
                    com.google.android.gms.wearable.Wearable.getMessageClient(context).sendMessage(node.id, PATH_PING, ByteArray(0))
                }
                done(info.nodes.map { it.displayName })
            }
            .addOnFailureListener { done(emptyList()) }
    }
}

/** Messages from the watch app: it fell asleep, or its answer to a ping. */
class WatchListener : com.google.android.gms.wearable.WearableListenerService() {
    private val main = android.os.Handler(android.os.Looper.getMainLooper())

    override fun onMessageReceived(event: com.google.android.gms.wearable.MessageEvent) {
        val text = String(event.data)
        when (event.path) {
            WatchSleep.PATH_STATUS -> main.post { Sleep.testing?.invoke(WatchSleep, text) }
            WatchSleep.PATH_ASLEEP -> {
                val fields = text.lines().mapNotNull { line ->
                    line.split('=', limit = 2).takeIf { it.size == 2 }?.let { it[0] to it[1] }
                }.toMap()
                val since = fields["since"]?.toLongOrNull()?.takeIf { it > 0 }
                val from = fields["source"] ?: "the watch"
                val test = fields["test"] == "1"
                main.post {
                    val listener = Sleep.testing
                    if (listener != null) {
                        val at = since?.let { ", asleep since " + java.text.DateFormat.getTimeInstance().format(java.util.Date(it)) } ?: ""
                        listener(WatchSleep, (if (test) "Test from " else "Heard from ") + from + at)
                    } else if (!test && Sleep.listening(Settings(this), WatchSleep)) {
                        Sleep.report(this, since, from)
                    }
                }
            }
        }
    }
}

/** Where both the sleep API's readings and other apps' broadcasts arrive. */
class SleepReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val settings = Settings(context)
        val test = Sleep.testing
        when {
            intent.action == Sleep.ACTION_ASLEEP -> {
                val since = intent.getLongExtra(Sleep.EXTRA_SINCE, 0).takeIf { it > 0 }
                val from = intent.getStringExtra(Sleep.EXTRA_SOURCE) ?: "another app"
                if (test != null) {
                    val at = since?.let { ", asleep since " + java.text.DateFormat.getTimeInstance().format(java.util.Date(it)) } ?: ""
                    test(OtherAppsSleep, "Heard from $from$at")
                    return
                }
                if (!Sleep.listening(settings, OtherAppsSleep)) return
                Sleep.report(context, since, from)
            }
            SleepClassifyEvent.hasEvents(intent) -> {
                val asleep = PhoneSleep.heard(context, SleepClassifyEvent.extractEvents(intent))
                if (test != null) {
                    test(PhoneSleep, "Reading: ${PhoneSleep.latest(context)}")
                    return
                }
                if (!Sleep.listening(settings, PhoneSleep)) return
                asleep?.let { Sleep.report(context, it, "this phone") }
            }
        }
    }
}
