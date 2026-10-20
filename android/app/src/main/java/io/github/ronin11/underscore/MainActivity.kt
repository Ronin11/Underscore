package io.github.ronin11.underscore

import android.Manifest
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.provider.OpenableColumns
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.SeekBar
import android.widget.TextView
import android.widget.Toast
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import java.util.Locale

/**
 * A deliberately plain control surface: enough to try the engine on a phone.
 * The real UI comes later; this one is built in code so there is nothing to
 * keep in sync.
 */
class MainActivity : AppCompatActivity() {

    private var service: PlaybackService? = null
    private val ui = Handler(Looper.getMainLooper())

    private lateinit var status: TextView
    private lateinit var files: TextView
    private lateinit var playButton: Button
    private lateinit var musicButton: Button
    private lateinit var speedLabel: TextView
    private lateinit var sleepLabel: TextView
    private lateinit var chapterLabel: TextView
    private lateinit var chaptersButton: Button
    private lateinit var progress: SeekBar
    private var scrubbing = false

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            service = (binder as PlaybackService.LocalBinder).service
            refresh()
        }

        override fun onServiceDisconnected(name: ComponentName) {
            service = null
        }
    }

    private val pickSpeech = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let { load(it, speech = true) }
    }
    private val pickMusic = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let { load(it, speech = false) }
    }
    private val askNotifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) { }

    private val tick = object : Runnable {
        override fun run() {
            refresh()
            ui.postDelayed(this, 500)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildUi())
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            askNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        val intent = Intent(this, PlaybackService::class.java)
        ContextCompat.startForegroundService(this, intent)
        bindService(intent, connection, Context.BIND_AUTO_CREATE)
    }

    override fun onStart() {
        super.onStart()
        ui.post(tick)
    }

    override fun onStop() {
        ui.removeCallbacks(tick)
        super.onStop()
    }

    override fun onDestroy() {
        unbindService(connection)
        super.onDestroy()
    }

    private fun load(uri: Uri, speech: Boolean) {
        val s = service ?: return
        val name = displayName(uri)
        val ok = if (speech) s.loadSpeech(uri, name) else s.loadMusic(uri, name)
        if (!ok) Toast.makeText(this, "Could not open ${name ?: "file"}", Toast.LENGTH_LONG).show()
        refresh()
    }

    private fun displayName(uri: Uri): String? =
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }

    private fun refresh() {
        val s = service ?: run {
            status.text = "Starting…"
            return
        }
        val e = s.engine
        val pos = e.positionMs
        val dur = e.durationMs
        files.text = "Book: ${s.speechName ?: "none"}\nMusic: ${s.musicName ?: "none"}"
        status.text = "${fmt(pos)} / ${if (dur >= 0) fmt(dur) else "?"}"
        playButton.text = if (e.isPlaying) "Pause" else "Play"
        musicButton.text = if (e.musicEnabled) "Music: on" else "Music: off"
        speedLabel.text = String.format(Locale.US, "%.1fx", e.speed)
        val chapters = e.chapterCount
        val chapter = e.currentChapter
        chapterLabel.text = when {
            chapters == 0 -> "No chapter marks"
            chapter < 0 -> "$chapters chapters"
            else -> "Chapter ${chapter + 1} of $chapters" +
                (e.chapterTitle(chapter)?.let { ": $it" } ?: "")
        }
        chaptersButton.isEnabled = chapters > 0
        val left = e.sleepRemainingMs(s.now())
        sleepLabel.text = when {
            left >= 0 -> "Sleep in ${fmt(left)}"
            e.sleepAtChapterEnd -> "Sleep at the end of this chapter"
            else -> "Sleep: off"
        }
        if (!scrubbing && dur > 0) {
            progress.max = 1000
            progress.progress = ((pos * 1000) / dur).toInt().coerceIn(0, 1000)
        }
    }

    /** A list of the book's chapters; picking one jumps to it. */
    private fun showChapters() {
        val e = service?.engine ?: return
        val count = e.chapterCount
        if (count == 0) return
        val items = Array(count) { i ->
            val title = e.chapterTitle(i) ?: "Chapter ${i + 1}"
            "${i + 1}. $title   ${fmt(e.chapterStartMs(i))}"
        }
        AlertDialog.Builder(this)
            .setTitle("Chapters")
            .setSingleChoiceItems(items, e.currentChapter) { dialog, which ->
                e.seekChapter(which)
                dialog.dismiss()
                refresh()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun fmt(ms: Long): String {
        val total = ms / 1000
        val h = total / 3600
        val m = (total % 3600) / 60
        val sec = total % 60
        return if (h > 0) String.format(Locale.US, "%d:%02d:%02d", h, m, sec)
        else String.format(Locale.US, "%d:%02d", m, sec)
    }

    private fun buildUi(): View {
        val pad = (16 * resources.displayMetrics.density).toInt()
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad, pad, pad)
        }

        fun row(vararg views: View) {
            val r = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
            views.forEach { v ->
                r.addView(v, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            }
            root.addView(r)
        }

        fun button(label: String, onClick: () -> Unit) = Button(this).apply {
            text = label
            setOnClickListener { onClick(); refresh() }
        }

        root.addView(TextView(this).apply {
            text = "underscore"
            textSize = 24f
            gravity = Gravity.CENTER
        })
        files = TextView(this).apply { setPadding(0, pad / 2, 0, pad / 2) }
        root.addView(files)

        row(
            button("Pick book") { pickSpeech.launch(AUDIO_TYPES) },
            button("Pick music") { pickMusic.launch(AUDIO_TYPES) },
        )

        status = TextView(this).apply {
            textSize = 20f
            gravity = Gravity.CENTER
            setPadding(0, pad, 0, 0)
        }
        root.addView(status)
        progress = SeekBar(this).apply {
            setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(sb: SeekBar, p: Int, fromUser: Boolean) {}
                override fun onStartTrackingTouch(sb: SeekBar) { scrubbing = true }
                override fun onStopTrackingTouch(sb: SeekBar) {
                    scrubbing = false
                    val s = service ?: return
                    val dur = s.engine.durationMs
                    if (dur > 0) s.engine.seekMs(dur * sb.progress / 1000)
                }
            })
        }
        root.addView(progress)

        playButton = button("Play") { service?.let { it.engine.toggle(it.now()) } }
        row(
            button("−15 s") { service?.engine?.skipBack() },
            playButton,
            button("+30 s") { service?.engine?.skipForward() },
        )

        speedLabel = TextView(this).apply { gravity = Gravity.CENTER; textSize = 18f }
        row(
            button("Slower") { service?.engine?.let { it.setSpeed(it.speed - 0.1f) } },
            speedLabel,
            button("Faster") { service?.engine?.let { it.setSpeed(it.speed + 0.1f) } },
        )

        chapterLabel = TextView(this).apply { gravity = Gravity.CENTER; setPadding(0, pad, 0, 0) }
        root.addView(chapterLabel)
        chaptersButton = button("Chapters") { showChapters() }
        chaptersButton.isEnabled = false
        row(
            chaptersButton,
            button("Sleep at chapter end") { service?.let { it.engine.setSleepEndOfChapter(it.now()) } },
        )

        musicButton = button("Music: on") {
            service?.engine?.let { it.setMusicEnabled(!it.musicEnabled) }
        }
        root.addView(TextView(this).apply { text = "Music level"; setPadding(0, pad, 0, 0) })
        root.addView(SeekBar(this).apply {
            max = 100
            progress = 60
            setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(sb: SeekBar, p: Int, fromUser: Boolean) {
                    if (fromUser) service?.engine?.setMusicLevel(p / 100f)
                }
                override fun onStartTrackingTouch(sb: SeekBar) {}
                override fun onStopTrackingTouch(sb: SeekBar) {}
            })
        })
        root.addView(musicButton)

        sleepLabel = TextView(this).apply { gravity = Gravity.CENTER; setPadding(0, pad, 0, 0) }
        root.addView(sleepLabel)
        row(
            button("Sleep 15 m") { service?.let { it.engine.setSleepMs(15 * 60_000L, it.now()) } },
            button("Sleep 30 m") { service?.let { it.engine.setSleepMs(30 * 60_000L, it.now()) } },
            button("+10 m") { service?.let { it.engine.extendSleepMs(10 * 60_000L, it.now()) } },
            button("Off") { service?.let { it.engine.setSleepMs(0, it.now()) } },
        )

        return ScrollView(this).apply { addView(root) }
    }

    companion object {
        private val AUDIO_TYPES = arrayOf("audio/*", "application/ogg", "video/mp4")
    }
}
