package io.github.ronin11.underscore.wear

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.ContextCompat
import androidx.health.services.client.HealthServices
import androidx.health.services.client.data.PassiveListenerConfig
import androidx.health.services.client.data.UserActivityState
import com.google.android.gms.wearable.Wearable
import java.text.DateFormat
import java.util.Date
import java.util.concurrent.Executor

/**
 * What the watch app does: ask Health Services to say when the wearer
 * falls asleep, and pass that on to the phone app, which decides whether
 * a book is playing and should wind down. The watch keeps no audio and
 * plays nothing.
 *
 * The message paths and payloads must match the phone's WatchListener.
 */
object Watch {
    /** Watch to phone: the wearer fell asleep. Lines of key=value: since, source, test. */
    const val PATH_ASLEEP = "/underscore/asleep"
    /** Phone to watch: how do things stand? */
    const val PATH_PING = "/underscore/ping"
    /** Watch to phone: the answer, as lines of text. */
    const val PATH_STATUS = "/underscore/status"
    /** Watch to phone: the wearer's state changed. Lines of key=value: state, at, source. */
    const val PATH_STATE = "/underscore/state"

    private fun prefs(context: Context) = context.getSharedPreferences("watch", Context.MODE_PRIVATE)

    /** Whether the wearer turned sleep watching on in this app. */
    fun isOn(context: Context): Boolean = prefs(context).getBoolean("on", false)

    fun hasPermission(context: Context): Boolean =
        ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED

    val permission: String =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) Manifest.permission.ACTIVITY_RECOGNITION
        else "com.google.android.gms.permission.ACTIVITY_RECOGNITION"

    /** Remember a line about what happened, for the status the phone sees. */
    fun note(context: Context, line: String) {
        val stamped = DateFormat.getTimeInstance(DateFormat.SHORT).format(Date()) + " " + line
        val lines = (prefs(context).getString("log", null)?.lines() ?: emptyList()) + stamped
        prefs(context).edit().putString("log", lines.takeLast(KEEP).joinToString("\n")).apply()
    }

    /** The last [last] lines of what happened. */
    fun log(context: Context, last: Int = SHOW): String =
        prefs(context).getString("log", null)?.lines()?.takeLast(last)?.joinToString("\n") ?: "nothing yet"

    /** Lines kept: a few nights of state changes. Lines shown on screens. */
    private const val KEEP = 100
    private const val SHOW = 6

    /**
     * Start or stop listening for sleep. Health Services keeps the listener
     * across restarts of the app; it is set again whenever the app opens.
     * [done] hears null on success or what went wrong, on [executor].
     */
    fun setOn(context: Context, on: Boolean, executor: Executor, done: (String?) -> Unit) {
        prefs(context).edit().putBoolean("on", on).apply()
        val client = HealthServices.getClient(context).passiveMonitoringClient
        val future = if (on) {
            val config = PassiveListenerConfig.Builder().setShouldUserActivityInfoBeRequested(true).build()
            client.setPassiveListenerServiceAsync(AsleepService::class.java, config)
        } else {
            client.clearPassiveListenerServiceAsync()
        }
        future.addListener({
            val problem = runCatching { future.get() }.exceptionOrNull()
            note(context, if (problem == null) (if (on) "Watching for sleep" else "Stopped watching") else "Failed: ${problem.message}")
            done(problem?.let { it.cause?.message ?: it.message ?: it.toString() })
        }, executor)
    }

    /** Whether this watch can report the asleep state at all. Null if unknown. */
    fun supportsAsleep(context: Context, executor: Executor, done: (Boolean?) -> Unit) {
        val future = HealthServices.getClient(context).passiveMonitoringClient.getCapabilitiesAsync()
        future.addListener({
            done(runCatching { UserActivityState.USER_ACTIVITY_ASLEEP in future.get().supportedUserActivityStates }.getOrNull())
        }, executor)
    }

    /** Send [text] to every connected phone at [path]. */
    fun send(context: Context, path: String, text: String) {
        val app = context.applicationContext
        Wearable.getNodeClient(app).connectedNodes.addOnSuccessListener { nodes ->
            for (node in nodes) {
                Wearable.getMessageClient(app).sendMessage(node.id, path, text.toByteArray())
            }
            if (nodes.isEmpty()) note(app, "No phone connected")
        }
    }

    /** Tell the phone the wearer's state changed, asleep or awake, for its sleep log. */
    fun sendState(context: Context, state: String, atMs: Long) {
        send(context, PATH_STATE, "state=$state\nat=$atMs\nsource=${Build.MODEL}")
    }

    /** Tell the phone the wearer fell asleep at [sinceMs]. A test does not pause the book. */
    fun sendAsleep(context: Context, sinceMs: Long, test: Boolean) {
        send(context, PATH_ASLEEP, "since=$sinceMs\nsource=${Build.MODEL}\ntest=${if (test) 1 else 0}")
    }

    /** How things stand, as lines for the phone's setup screen. [done] runs on [executor]. */
    fun status(context: Context, executor: Executor, done: (String) -> Unit) {
        supportsAsleep(context, executor) { supported ->
            done(
                buildString {
                    appendLine("Watch app on ${Build.MODEL}")
                    appendLine("Watching for sleep: ${if (isOn(context)) "on" else "off"}")
                    appendLine("Permission: ${if (hasPermission(context)) "allowed" else "not allowed"}")
                    appendLine(
                        "Reports sleep: " + when (supported) {
                            true -> "yes"
                            false -> "no, this watch does not offer it"
                            null -> "unknown"
                        },
                    )
                    append("Latest:\n${log(context)}")
                },
            )
        }
    }
}
