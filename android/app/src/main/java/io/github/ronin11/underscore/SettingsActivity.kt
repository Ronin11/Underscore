package io.github.ronin11.underscore

import android.Manifest
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.DocumentsContract
import android.view.View
import android.view.ViewGroup
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.SeekBar
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.widget.SwitchCompat
import androidx.core.content.ContextCompat
import java.io.File

/**
 * The listener's choices. Each row writes to [Settings]; the service
 * watches that file and pushes changes into the engine, so nothing here
 * touches the engine directly except to read what to show.
 */
class SettingsActivity : ServiceActivity() {

    private val settings by lazy { Settings(this) }
    private lateinit var skipBack: Row
    private lateinit var skipForward: Row
    private lateinit var rewind: SwitchCompat
    private lateinit var keepScreen: SwitchCompat
    private lateinit var musicLevel: SeekBar
    private lateinit var musicLevelValue: TextView
    private lateinit var musicFolder: Row
    private lateinit var server: Row

    private val pickMusicFolder = registerForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let { chooseMusicFolder(it) }
    }
    private val askAudioRead = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (!granted) toast("Without audio access the music folder cannot be read")
    }

    override fun refresh() {
        skipBack.summary = "${settings.skipBackSec} seconds"
        skipForward.summary = "${settings.skipForwardSec} seconds"
        if (!rewind.isPressed) rewind.isChecked = settings.rewindOnResume
        if (!keepScreen.isPressed) keepScreen.isChecked = settings.keepScreenOn
        if (!musicLevel.isPressed) musicLevel.progress = settings.musicLevel
        musicLevelValue.text = "${settings.musicLevel}%"
        musicFolder.summary = settings.musicDir ?: "Not set. Maps need one to find their music."
        val s = service
        server.summary = when {
            s == null -> "Starting…"
            s.serverBusy != null -> s.serverBusy
            s.engine.hasServer -> "Signed in to ${s.serverUrl}"
            s.serverUrl != null -> "${s.serverUrl}, not signed in"
            else -> "Not signed in"
        }
    }

    /** A single-choice list of seconds for one of the skip buttons. */
    private fun pickSeconds(title: String, choices: IntArray, current: Int, onPick: (Int) -> Unit) {
        val items = Array(choices.size) { "${choices[it]} seconds" }
        AlertDialog.Builder(this)
            .setTitle(title)
            .setSingleChoiceItems(items, choices.indexOf(current)) { dialog, which ->
                onPick(choices[which])
                dialog.dismiss()
                refresh()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /**
     * The map's music comes from a folder the engine scans by path. The
     * tree picker gives a document URI; on device storage that maps to a
     * path the scan can walk, given the audio permission.
     */
    private fun chooseMusicFolder(uri: Uri) {
        val dir = folderPath(uri)
        if (dir == null) {
            toast("Pick a folder on this device's storage")
            return
        }
        settings.musicDir = dir.absolutePath
        val permission = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            Manifest.permission.READ_MEDIA_AUDIO
        } else {
            Manifest.permission.READ_EXTERNAL_STORAGE
        }
        if (ContextCompat.checkSelfPermission(this, permission) != PackageManager.PERMISSION_GRANTED) {
            askAudioRead.launch(permission)
        }
        refresh()
    }

    /** A document-tree URI as a path, when it is on device storage. */
    private fun folderPath(uri: Uri): File? {
        if (uri.authority != "com.android.externalstorage.documents") return null
        val docId = DocumentsContract.getTreeDocumentId(uri)
        val volume = docId.substringBefore(':')
        val rel = docId.substringAfter(':', "")
        val root = if (volume == "primary") {
            Environment.getExternalStorageDirectory()
        } else {
            File("/storage/$volume")
        }
        return File(root, rel).takeIf { it.isDirectory }
    }

    private fun switchRow(title: String, summary: String, onChange: (Boolean) -> Unit): Pair<View, SwitchCompat> {
        val toggle = SwitchCompat(this).apply {
            setOnCheckedChangeListener { v, checked -> if (v.isPressed) onChange(checked) }
        }
        val text = row(title, summary, null)
        val line = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = android.view.Gravity.CENTER_VERTICAL
            setPadding(0, 0, dp(16), 0)
            isClickable = true
            isFocusable = true
            setBackgroundResource(selectableBackground())
            addView(text.view, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            addView(toggle)
            setOnClickListener { toggle.isChecked = !toggle.isChecked; onChange(toggle.isChecked); refresh() }
        }
        return line to toggle
    }

    override fun buildUi(): View {
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        root.addView(toolbar("Settings", up = true))
        val list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }

        list.addView(heading("Listening"))
        skipBack = row("Skip back", null) {
            pickSeconds("Skip back", intArrayOf(5, 10, 15, 20, 30, 45, 60), settings.skipBackSec) { settings.skipBackSec = it }
        }
        skipForward = row("Skip forward", null) {
            pickSeconds("Skip forward", intArrayOf(10, 15, 20, 30, 45, 60, 90), settings.skipForwardSec) { settings.skipForwardSec = it }
        }
        list.addView(skipBack.view)
        list.addView(skipForward.view)
        val (rewindRow, rewindSwitch) = switchRow(
            "Rewind on resume",
            "A couple of seconds after a short pause, up to half a minute after a day.",
        ) { settings.rewindOnResume = it }
        rewind = rewindSwitch
        list.addView(rewindRow)
        val (keepRow, keepSwitch) = switchRow(
            "Keep the screen on",
            "While the book plays and this app is in front.",
        ) { settings.keepScreenOn = it }
        keepScreen = keepSwitch
        list.addView(keepRow)

        list.addView(divider())
        list.addView(heading("Music"))
        val levelRow = row("Background music level", "Before ducking under the narrator.", null)
        musicLevelValue = label(14f, android.view.Gravity.END)
        musicLevel = SeekBar(this).apply {
            max = 100
            setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(sb: SeekBar, p: Int, fromUser: Boolean) {
                    if (fromUser) { settings.musicLevel = p; musicLevelValue.text = "$p%" }
                }
                override fun onStartTrackingTouch(sb: SeekBar) {}
                override fun onStopTrackingTouch(sb: SeekBar) {}
            })
        }
        list.addView(levelRow.view)
        list.addView(
            LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = android.view.Gravity.CENTER_VERTICAL
                setPadding(dp(16), 0, dp(16), dp(8))
                addView(musicLevel, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                addView(musicLevelValue, LinearLayout.LayoutParams(dp(48), ViewGroup.LayoutParams.WRAP_CONTENT))
            },
        )
        musicFolder = row("Music folder", null) { pickMusicFolder.launch(null) }
        list.addView(musicFolder.view)

        list.addView(divider())
        list.addView(heading("Audiobookshelf"))
        server = row("Server", null) { serverDialog() }
        list.addView(server.view)

        list.addView(divider())
        list.addView(heading("About"))
        list.addView(row("underscore", "Build ${BuildConfig.BUILD}, commit ${BuildConfig.COMMIT}", null).view)

        root.addView(ScrollView(this).apply { addView(list) })
        return root
    }
}
