package io.github.ronin11.underscore

import android.graphics.Typeface
import android.net.Uri
import android.provider.OpenableColumns
import android.text.InputType
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.SeekBar
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog

/**
 * Knobs for trying the engine that have no place on the player or in the
 * settings: a music track played by hand under the book, a fake room-noise
 * level, the live engine state, and the bug report.
 */
class DebugActivity : ServiceActivity() {

    private lateinit var music: Row
    private lateinit var noiseValue: TextView
    private lateinit var noise: SeekBar
    private lateinit var state: TextView

    private val pickMusic = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let { loadMusic(it) }
    }

    override fun refresh() {
        val s = service ?: return
        music.summary = s.musicName ?: "None. Tap to pick a track to play under the book."
        state.text = s.describe()
    }

    private fun loadMusic(uri: Uri) {
        val s = service ?: return
        val name = contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }
        if (!s.loadMusic(uri, name)) toast("Could not open ${name ?: "file"}")
        refresh()
    }

    /** Ask what happened, then build a report and open the share sheet. */
    private fun reportBug() {
        val pad = dp(16)
        val note = EditText(this).apply {
            hint = "What happened? What did you expect?"
            minLines = 3
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE
        }
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad / 2, pad, 0)
            addView(note)
        }
        AlertDialog.Builder(this)
            .setTitle("Report a bug")
            .setMessage("Device, build, playback state, and this app's log lines go into a text file. Nothing is sent until you pick where.")
            .setView(box)
            .setPositiveButton("Share") { _, _ ->
                try {
                    val file = BugReport.share(this, service, note.text.toString())
                    toast("Report saved as ${file.name}", long = false)
                } catch (e: Exception) {
                    toast("Could not build the report: $e")
                }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    override fun buildUi(): View {
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        root.addView(toolbar("Debug", up = true))
        val list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }

        list.addView(heading("Music by hand"))
        music = row("Music track", null) { pickMusic.launch(AUDIO_TYPES) }
        list.addView(music.view)
        list.addView(
            row(
                "A track picked here plays under the book until a map cue replaces it.",
                "Use Settings for the level and the player's Music menu to turn it off.",
                null,
            ).view,
        )

        list.addView(divider())
        list.addView(heading("Room noise"))
        list.addView(
            row(
                "Pretend the room is this loud",
                "Stands in for the microphone feed that is not wired yet. Louder raises the narrator and lowers the music.",
                null,
            ).view,
        )
        noiseValue = label(14f, android.view.Gravity.END)
        noise = SeekBar(this).apply {
            max = 60
            progress = 0
            setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(sb: SeekBar, p: Int, fromUser: Boolean) {
                    val db = (p - 60).toFloat()
                    noiseValue.text = "${db.toInt()} dB"
                    if (fromUser) service?.engine?.setNoiseDb(db)
                }
                override fun onStartTrackingTouch(sb: SeekBar) {}
                override fun onStopTrackingTouch(sb: SeekBar) {}
            })
        }
        noiseValue.text = "-60 dB"
        list.addView(
            LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = android.view.Gravity.CENTER_VERTICAL
                setPadding(dp(16), 0, dp(16), dp(8))
                addView(noise, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                addView(noiseValue, LinearLayout.LayoutParams(dp(64), ViewGroup.LayoutParams.WRAP_CONTENT))
            },
        )

        list.addView(divider())
        list.addView(heading("Report"))
        list.addView(
            Button(this).apply {
                text = "Report a bug"
                setOnClickListener { reportBug() }
            },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                leftMargin = dp(16); rightMargin = dp(16)
            },
        )
        list.addView(row("Build", "${BuildConfig.BUILD}, commit ${BuildConfig.COMMIT}", null).view)

        list.addView(divider())
        list.addView(heading("Engine"))
        state = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            textSize = 11f
            setPadding(dp(16), 0, dp(16), dp(16))
            setTextIsSelectable(true)
        }
        list.addView(state)

        root.addView(ScrollView(this).apply { addView(list) })
        return root
    }

    private companion object {
        val AUDIO_TYPES = arrayOf("audio/*", "application/ogg", "video/mp4")
    }
}
