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
    private lateinit var shake: SwitchCompat
    private lateinit var musicLevel: SeekBar
    private lateinit var musicLevelValue: TextView
    private lateinit var musicFolder: Row
    private lateinit var server: Row
    private lateinit var musicServer: Row
    private lateinit var storage: Row
    private lateinit var budget: SeekBar
    private lateinit var budgetValue: TextView
    private lateinit var wifiOnly: SwitchCompat
    /** What the download cache holds, measured off the main thread. */
    @Volatile private var storageSummary: String? = null

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
        if (!shake.isPressed) shake.isChecked = settings.sleepShake
        if (!musicLevel.isPressed) musicLevel.progress = settings.musicLevel
        musicLevelValue.text = "${settings.musicLevel}%"
        musicFolder.summary = settings.musicDir ?: "Not set. Used when no music server is signed in."
        val s = service
        musicServer.summary = when {
            s == null -> "Starting…"
            s.musicServerBusy != null -> s.musicServerBusy
            s.engine.hasMusicServer -> "Signed in to ${s.musicServerUrl}. Maps play from it."
            s.musicServerUrl != null -> "${s.musicServerUrl}, not signed in"
            else -> "Not signed in. Maps need this or a music folder."
        }
        server.summary = when {
            s == null -> "Starting…"
            s.serverBusy != null -> s.serverBusy
            s.engine.hasServer -> "Signed in to ${s.serverUrl}"
            s.serverUrl != null -> "${s.serverUrl}, not signed in"
            else -> "Not signed in"
        }
        if (!budget.isPressed) budget.progress = settings.storageStep
        budgetValue.text = budgetLabel(budget.progress)
        if (!wifiOnly.isPressed) wifiOnly.isChecked = settings.wifiOnly
        storage.summary = listOfNotNull(s?.keepBusy, storageSummary ?: "Measuring…").joinToString(". ")
    }

    override fun onServiceReady() = measure()

    private fun size(bytes: Long): String = android.text.format.Formatter.formatShortFileSize(this, bytes)

    /** Count up the downloads in the background and show the total. */
    private fun measure() {
        val s = service ?: return
        Thread({
            val sizes = s.storage.sizes(s.libraryCache)
            val books = s.storage.books().filter { (sizes[it.key]?.first ?: 0) + (sizes[it.key]?.second ?: 0) > 0 }
            val total = (s.libraryCache.listFiles() ?: emptyArray()).sumOf { it.length() }
            val kept = books.count { it.pinned }
            val parts = ArrayList<String>()
            parts.add("${if (books.size == 1) "1 book" else "${books.size} books"}, ${size(total)} in all")
            if (kept > 0) parts.add("$kept kept offline")
            val covers = s.coverBytes()
            if (covers > 0) parts.add("covers ${size(covers)}")
            storageSummary = "${parts.joinToString(", ")}. Tap to see or remove them."
            ui.post { refresh() }
        }, "underscore-storage").start()
    }

    /** The slider's stop as words. */
    private fun budgetLabel(step: Int): String = when (val gb = Settings.STORAGE_STEPS_GB[step]) {
        0L -> "Only the book I'm on"
        -1L -> "No limit"
        else -> "$gb GB"
    }

    /**
     * The books on the phone, biggest first. Each says what it takes on
     * its own and what it shares with other books, which stays when it
     * goes. Tap one to keep it offline or remove it.
     */
    private fun storageDialog() {
        val s = service ?: return
        Thread({
            val sizes = s.storage.sizes(s.libraryCache)
            val unlisted = s.storage.unlisted(s.libraryCache)
            val covers = s.coverBytes()
            ui.post { showStorage(s, sizes, unlisted, covers) }
        }, "underscore-storage").start()
    }

    private fun showStorage(s: PlaybackService, sizes: Map<String, Pair<Long, Long>>, unlisted: List<java.io.File>, covers: Long) {
        val open = s.openKey
        val books = s.storage.books()
            .filter { (sizes[it.key]?.first ?: 0) + (sizes[it.key]?.second ?: 0) > 0 || it.pinned }
            .sortedByDescending { sizes[it.key]?.first ?: 0 }
        val labels = ArrayList<String>()
        for (b in books) {
            val (own, shared) = sizes[b.key] ?: (0L to 0L)
            val notes = listOfNotNull(
                size(own),
                if (shared > 0) "+${size(shared)} shared with other books" else null,
                if (b.pinned) (if (b.complete) "kept offline" else "kept offline, downloading") else null,
                if (b.key == open) "open now" else null,
            )
            labels.add("${b.title}\n${notes.joinToString("  ·  ")}")
        }
        if (unlisted.isNotEmpty()) labels.add("Files no book uses\n${unlisted.size} files, ${size(unlisted.sumOf { it.length() })}")
        if (covers > 0) labels.add("Book covers\n${size(covers)}")
        if (labels.isEmpty()) {
            toast("Nothing is downloaded")
            return
        }
        AlertDialog.Builder(this)
            .setTitle("On this phone")
            .setItems(labels.toTypedArray()) { _, which ->
                when {
                    which < books.size -> bookDialog(s, books[which], books[which].key == open)
                    which == books.size && unlisted.isNotEmpty() -> confirm("Remove the files no book uses?") {
                        s.removeUnlisted { measure() }
                    }
                    else -> confirm("Remove the book covers?") {
                        Thread({ s.removeCovers(); ui.post { measure() } }, "underscore-storage").start()
                    }
                }
            }
            .setNegativeButton("Close", null)
            .show()
    }

    /** Keep one book offline, or remove it from the phone. */
    private fun bookDialog(s: PlaybackService, b: Storage.Book, open: Boolean) {
        val actions = ArrayList<Pair<String, () -> Unit>>()
        val id = b.serverId
        if (id != null) {
            actions.add(
                (if (b.pinned) "Stop keeping offline" else "Keep offline, with its map's music") to {
                    s.keepOffline(
                        PlaybackService.ServerBook(
                            id, b.title, b.author, b.series, b.durationMs, 0, b.audio.size, b.coverUrl,
                            -1.0, false, b.played, open,
                        ),
                        !b.pinned,
                    )
                    measure()
                },
            )
        }
        if (!open) {
            actions.add("Remove from this phone" to {
                confirm("Remove ${b.title}?", "Music another book uses stays. Its map and transcript stay too. It downloads again the next time it is opened.") {
                    s.removeBook(b.key) { measure() }
                }
            })
        }
        AlertDialog.Builder(this)
            .setTitle(b.title)
            .setItems(actions.map { it.first }.toTypedArray()) { _, which -> actions[which].second() }
            .setNegativeButton("Close", null)
            .show()
    }

    private fun confirm(title: String, message: String? = null, onYes: () -> Unit) {
        AlertDialog.Builder(this)
            .setTitle(title)
            .apply { message?.let { setMessage(it) } }
            .setPositiveButton("Remove") { _, _ -> onYes() }
            .setNegativeButton("Cancel", null)
            .show()
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
        val (shakeRow, shakeSwitch) = switchRow(
            "Shake to undo sleep",
            "Shake the phone when Auto winds the book down while you are awake, or within 5 minutes of it stopping. During a timer, a shake starts it over.",
        ) { settings.sleepShake = it }
        shake = shakeSwitch
        list.addView(shakeRow)

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
        list.addView(heading("Music server"))
        musicServer = row("Navidrome", null) { musicServerDialog() }
        list.addView(musicServer.view)

        list.addView(divider())
        list.addView(heading("Audiobookshelf"))
        server = row("Server", null) { serverDialog() }
        list.addView(server.view)

        list.addView(divider())
        list.addView(heading("Storage"))
        list.addView(
            row(
                "Keep up to",
                "Books played longest ago go first when there is no room, with the music only they use. Books kept offline and the open book stay.",
                null,
            ).view,
        )
        budgetValue = label(14f, android.view.Gravity.END)
        budget = SeekBar(this).apply {
            max = Settings.STORAGE_STEPS_GB.lastIndex
            setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(sb: SeekBar, p: Int, fromUser: Boolean) {
                    budgetValue.text = budgetLabel(p)
                }
                override fun onStartTrackingTouch(sb: SeekBar) {}
                override fun onStopTrackingTouch(sb: SeekBar) {
                    settings.storageStep = sb.progress
                    service?.keepInBudget()
                    ui.postDelayed({ measure() }, 500)
                }
            })
        }
        list.addView(
            LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = android.view.Gravity.CENTER_VERTICAL
                setPadding(dp(16), 0, dp(16), dp(8))
                addView(budget, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                addView(budgetValue, LinearLayout.LayoutParams(dp(140), ViewGroup.LayoutParams.WRAP_CONTENT))
            },
        )
        val (wifiRow, wifiSwitch) = switchRow(
            "Download on Wi-Fi only",
            "For books kept offline and their music. The book you are playing still streams on mobile data.",
        ) {
            settings.wifiOnly = it
            service?.fetchKept()
        }
        wifiOnly = wifiSwitch
        list.addView(wifiRow)
        storage = row("On this phone", null) { storageDialog() }
        list.addView(storage.view)

        list.addView(divider())
        list.addView(heading("About"))
        list.addView(row("underscore", "Build ${BuildConfig.BUILD}, commit ${BuildConfig.COMMIT}", null).view)

        root.addView(ScrollView(this).apply { addView(list) })
        return root
    }
}
