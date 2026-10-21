package io.github.ronin11.underscore

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Build
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import java.io.File
import java.io.PrintWriter
import java.io.StringWriter
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * Gathers everything useful about the app's state into one text file and
 * opens the share sheet with it. Nothing leaves the phone until the user
 * picks where to send it. The report holds no audio and no file contents:
 * build and device details, the service's view of the session and the
 * notification, the engine's transport state, the last crash if there was
 * one, and this app's own lines from logcat.
 */
object BugReport {

    private const val LOG_LINES = 600
    private const val CRASH_FILE = "last-crash.txt"

    /** Build the report, save it under the cache dir, and open the share sheet. */
    fun share(context: Context, service: PlaybackService?, note: String): File {
        val stamp = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(Date())
        val dir = File(context.cacheDir, "reports").apply { mkdirs() }
        val file = File(dir, "underscore-report-$stamp.txt")
        file.writeText(build(context, service, note))
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
        val send = Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_SUBJECT, "underscore bug report $stamp")
            putExtra(Intent.EXTRA_STREAM, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        context.startActivity(Intent.createChooser(send, "Send bug report"))
        return file
    }

    /** Called from the uncaught-exception handler; keeps the last crash only. */
    fun recordCrash(context: Context, thread: Thread, error: Throwable) {
        val trace = StringWriter().also { error.printStackTrace(PrintWriter(it)) }.toString()
        val at = SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.US).format(Date())
        File(context.filesDir, CRASH_FILE).writeText("$at on thread ${thread.name}\n$trace")
    }

    fun build(context: Context, service: PlaybackService?, note: String): String {
        val out = StringBuilder()
        fun line(s: String = "") = out.append(s).append('\n')
        fun section(title: String) { line(); line("== $title ==") }

        line("underscore bug report")
        line(SimpleDateFormat("yyyy-MM-dd HH:mm:ss Z", Locale.US).format(Date()))
        if (note.isNotBlank()) {
            section("What happened")
            line(note.trim())
        }

        section("Build")
        line("version ${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})")
        line("build ${BuildConfig.BUILD}, commit ${BuildConfig.COMMIT}")
        line("package ${context.packageName}, target sdk ${context.applicationInfo.targetSdkVersion}")

        section("Device")
        line("${Build.MANUFACTURER} ${Build.MODEL} (${Build.DEVICE})")
        line("Android ${Build.VERSION.RELEASE}, api ${Build.VERSION.SDK_INT}, patch ${Build.VERSION.SECURITY_PATCH}")
        line("abis ${Build.SUPPORTED_ABIS.joinToString()}")

        section("Notifications")
        val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        line("enabled for app: ${NotificationManagerCompat.from(context).areNotificationsEnabled()}")
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            val granted = ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
                PackageManager.PERMISSION_GRANTED
            line("POST_NOTIFICATIONS granted: $granted")
        }
        for (channel in nm.notificationChannels) {
            line("channel ${channel.id}: importance ${channel.importance}")
        }
        val active = nm.activeNotifications
        line("active notifications: ${active.size}")
        for (sbn in active) {
            val extras = sbn.notification.extras
            val template = extras.getString(Notification.EXTRA_TEMPLATE)?.substringAfterLast('.') ?: "none"
            val hasSession = extras.containsKey(Notification.EXTRA_MEDIA_SESSION)
            val flags = sbn.notification.flags
            line(
                "  id ${sbn.id}: template $template, media session ${if (hasSession) "yes" else "no"}, " +
                    "channel ${sbn.notification.channelId}, " +
                    "foreground ${flags and Notification.FLAG_FOREGROUND_SERVICE != 0}, " +
                    "ongoing ${flags and Notification.FLAG_ONGOING_EVENT != 0}, " +
                    "actions ${sbn.notification.actions?.size ?: 0}",
            )
        }

        section("Audio")
        val am = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        line("music active (any app): ${am.isMusicActive}")
        line("media volume ${am.getStreamVolume(AudioManager.STREAM_MUSIC)} of ${am.getStreamMaxVolume(AudioManager.STREAM_MUSIC)}")
        val outputs = am.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
            .filter { it.type != AudioDeviceInfo.TYPE_TELEPHONY }
            .joinToString { "${it.productName} (type ${it.type})" }
        line("outputs: $outputs")

        section("Service")
        if (service == null) {
            line("not bound")
        } else {
            line(service.describe())
        }

        val crash = File(context.filesDir, CRASH_FILE)
        if (crash.exists()) {
            section("Last crash")
            line(crash.readText().trimEnd())
        }

        section("Log (this app's lines)")
        line(logcat())
        return out.toString()
    }

    /** This app's recent logcat lines. An app can read its own without any permission. */
    private fun logcat(): String = try {
        val process = ProcessBuilder(
            "logcat", "-d", "-v", "time", "-b", "main,crash", "-t", LOG_LINES.toString(),
        ).redirectErrorStream(true).start()
        val text = process.inputStream.bufferedReader().use { it.readText() }
        process.waitFor()
        text.ifBlank { "(empty)" }
    } catch (e: Exception) {
        "could not read logcat: $e"
    }
}
