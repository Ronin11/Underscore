package io.github.ronin11.underscore

import android.content.Intent
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.BaseAdapter
import android.widget.Button
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.ScrollView
import android.widget.TextView
import androidx.appcompat.app.AlertDialog
import org.json.JSONObject
import java.io.File
import java.text.DateFormat
import java.util.Date

/**
 * One server book, everything about it: what is on the phone and where,
 * downloading and removing it, its maps on the map server with a reader
 * for each, its transcript, and its details. Opened by holding a book in
 * the library.
 */
class BookActivity : ServiceActivity() {

    override val tab = Tab.LIBRARY

    private val id by lazy { intent.getStringExtra(EXTRA_ID) ?: "" }
    private val key get() = "abs:$id"
    private lateinit var cover: ImageView
    private lateinit var titleView: TextView
    private lateinit var byView: TextView
    private lateinit var phone: Row
    private lateinit var files: Row
    private lateinit var keepButton: Button
    private lateinit var removeButton: Button
    private lateinit var mapsList: LinearLayout
    private lateinit var mapsNote: TextView
    private lateinit var transcriptRow: Row
    private lateinit var voicesRow: Row
    private lateinit var details: Row

    /** The map server's answer for this book, once asked. */
    @Volatile private var maps: PlaybackService.BookMaps? = null
    @Volatile private var mapsProblem: String? = null
    private var asked = false
    private var mapsShown: String? = null

    private fun book(s: PlaybackService): PlaybackService.ServerBook? =
        s.serverBookList?.firstOrNull { it.id == id } ?: s.keptBooks().firstOrNull { it.id == id }

    override fun onServiceReady() = askMaps()

    /** Ask the map server for this book's maps, off the main thread. */
    private fun askMaps() {
        val s = service ?: return
        if (s.mapServerUrl == null) {
            mapsProblem = "Add a map server under Connections to see maps here."
            return
        }
        asked = true
        Thread({
            val v = runCatching { JSONObject(s.engine.mapServerBook(id)) }.getOrNull()
            if (v == null || v.has("error")) {
                mapsProblem = v?.optString("error") ?: "The map server did not answer."
            } else {
                mapsProblem = null
                maps = PlaybackService.parseBookMaps(id, v)
            }
            runOnUiThread { refresh() }
        }, "underscore-book-maps").start()
    }

    override fun refresh() {
        val s = service ?: return
        val b = book(s)
        val stored = s.storage.book(key)
        titleView.text = b?.title ?: stored?.title ?: id
        byView.text = listOfNotNull(
            (b?.author ?: stored?.author)?.takeIf { it.isNotEmpty() },
            (b?.series ?: stored?.series)?.takeIf { it.isNotEmpty() },
            (b?.durationMs ?: stored?.durationMs)?.takeIf { it > 0 }?.let { fmt(it) },
        ).joinToString("  ·  ")
        (b?.coverUrl ?: stored?.coverUrl)?.let { url ->
            cover.setImageBitmap(Covers.cached(id))
            Covers.load(this, id, url) { cover.setImageBitmap(it) }
        }

        // On this phone.
        val sizes = s.storage.sizes(s.libraryCache)[key]
        val size = { n: Long -> android.text.format.Formatter.formatShortFileSize(this, n) }
        phone.summary = when {
            stored == null -> "Not on this phone. It streams when you play it."
            else -> buildString {
                append(
                    when {
                        stored.pinned && stored.complete -> "Kept offline"
                        stored.pinned -> "Kept offline, still downloading"
                        stored.complete -> "Downloaded"
                        else -> "Partly downloaded while playing"
                    },
                )
                if (sizes != null) {
                    append(": ${size(sizes.first)} of its own")
                    if (sizes.second > 0) append(", and ${size(sizes.second)} of music shared with other books")
                }
                append(".")
                if (s.openKey == key) append(" It is open now, so it stays.")
                s.keepBusy?.let { append("\n$it") }
            }
        }
        val names = stored?.let { it.audio + it.music }.orEmpty()
        files.summary = if (names.isEmpty()) {
            s.libraryCache.absolutePath
        } else {
            names.joinToString("\n") { n ->
                val f = File(s.libraryCache, n)
                val note = if (f.isFile) size(f.length()) else "not downloaded yet"
                "${if (n in stored!!.music) "music" else "book"}: ${f.absolutePath} ($note)"
            }
        }
        keepButton.text = if (stored?.pinned == true) "Stop keeping offline" else "Download and keep offline"
        removeButton.isEnabled = stored != null && s.openKey != key
        removeButton.alpha = if (removeButton.isEnabled) 1f else 0.4f

        showMaps(s)
        transcriptRow.summary = transcriptSummary(s)
        voicesRow.summary = when {
            s.mapServerUrl == null -> "Add a map server under Connections."
            maps?.voicesJob?.active == true -> maps!!.voicesJob!!.let { j ->
                "Being worked out: ${j.done.size} of ${j.total.takeIf { it > 0 } ?: "?"} chapters. ${j.message}"
            }
            maps?.voices != null -> "Worked out. Speakers show in color in the subtitles. Tap to do it again."
            maps != null -> "Not yet. Tap to have the map server work out who says what. It takes a night for a long book."
            else -> "Asking the map server…"
        }

        details.summary = buildString {
            append("Audiobookshelf id: $id\n")
            append("Phone key: $key\n")
            b?.let {
                append("Files on the server: ${it.files}\n")
                if (it.addedMs > 0) append("Added: ${DateFormat.getDateInstance().format(Date(it.addedMs))}\n")
                if (it.progress >= 0) append("Progress: ${(it.progress * 100).toInt()}%${if (it.finished) ", finished" else ""}\n")
            }
            stored?.let {
                if (it.played > 0) append("Last played: ${DateFormat.getDateTimeInstance().format(Date(it.played))}\n")
                if (it.positionMs > 0) append("Position kept for offline: ${fmt(it.positionMs)}\n")
            }
            s.storage.map(key)?.let { (_, name) -> append("Map on this phone: $name\n") }
            append("Transcript file: ${transcriptFile(s).absolutePath}")
        }
    }

    private fun showMaps(s: PlaybackService) {
        val bm = maps
        val shape = "${bm?.default}|${bm?.maps?.joinToString { "${it.id}${it.updated}" }}|${bm?.job?.done?.size}|$mapsProblem"
        if (shape == mapsShown) return
        mapsShown = shape
        mapsList.removeAllViews()
        mapsNote.text = when {
            mapsProblem != null -> mapsProblem
            bm == null -> if (asked) "Asking the map server…" else ""
            bm.maps.isEmpty() -> "No maps for this book yet."
            else -> "Tap a map to read it or make it the one this book plays with."
        }
        bm?.job?.takeIf { it.active }?.let { job ->
            val of = if (job.total > 0) "${job.done.size} of ${job.total} chapters" else "starting"
            mapsList.addView(row("Being made", "$of. ${job.message}", null).view)
        }
        for (m in bm?.maps.orEmpty()) {
            val star = if (m.id == bm?.default) "  ★ plays with this book" else ""
            val state = if (m.complete) "${m.cues} cues" else "${m.cues} cues so far, still being made"
            mapsList.addView(row(m.name + star, state, { mapActions(s, m) }).view)
        }
        mapsList.addView(
            row("Make a new map", "The map server's worker writes one, a chapter at a time.") {
                Thread({
                    val v = runCatching { JSONObject(s.engine.mapServerBuild(id, 1)) }.getOrNull()
                    runOnUiThread {
                        toast(v?.optString("error")?.takeIf { it.isNotEmpty() } ?: "Asked. It appears here as it is made.")
                        askMaps()
                    }
                }, "underscore-book-build").start()
            }.view,
        )
    }

    private fun mapActions(s: PlaybackService, m: PlaybackService.ServerMap) {
        val isDefault = m.id == maps?.default
        val items = mutableListOf("Read it")
        if (!isDefault) items += "Play this book with it"
        AlertDialog.Builder(this)
            .setTitle(m.name)
            .setItems(items.toTypedArray()) { _, which ->
                when (items[which]) {
                    "Read it" -> readMap(s, m)
                    else -> makeDefault(s, m)
                }
            }
            .show()
    }

    private fun makeDefault(s: PlaybackService, m: PlaybackService.ServerMap) {
        if (s.bookMaps?.book == id) {
            // The open book: switch the music now, too.
            s.chooseMap(m.id)
            askMaps()
            return
        }
        Thread({
            s.engine.mapServerDefault(id, m.id)
            runOnUiThread { askMaps() }
        }, "underscore-book-default").start()
    }

    private fun readMap(s: PlaybackService, m: PlaybackService.ServerMap) {
        toast("Fetching ${m.name}…", long = false)
        Thread({
            val toml = s.engine.mapServerMap(m.id)
            runOnUiThread {
                if (toml == null) toast("The map server did not send it") else lines(m.name, MapText.lines(toml))
            }
        }, "underscore-book-map").start()
    }

    // The transcript: the phone's own copy when there is one, else the map server's.

    private fun transcriptFile(s: PlaybackService): File =
        File(File(s.filesDir, "whisper/transcripts"), "%016x.jsonl".format(fnv(key)))

    private fun transcriptSummary(s: PlaybackService): String {
        val f = transcriptFile(s)
        val local = if (f.isFile) "On this phone: ${android.text.format.Formatter.formatShortFileSize(this, f.length())}." else "Not on this phone yet."
        val server = if (s.mapServerUrl != null) " Tap to read it, from the phone or the map server." else " Tap to read it."
        return local + server
    }

    private fun readTranscript(s: PlaybackService) {
        val f = transcriptFile(s)
        Thread({
            val shown = if (f.isFile) {
                runCatching { MapText.phoneTranscript(f.readLines()) }.getOrNull()
            } else {
                s.engine.mapServerTranscript(id)?.let { runCatching { MapText.serverTranscript(String(it)) }.getOrNull() }
            }
            runOnUiThread {
                if (shown.isNullOrEmpty()) toast("No transcript yet. The map server makes one with a map.") else lines("Transcript", shown)
            }
        }, "underscore-book-transcript").start()
    }

    /** A long list of time and text, in a full-height dialog. */
    private fun lines(title: String, items: List<Pair<String, String>>) {
        val list = ListView(this).apply {
            adapter = object : BaseAdapter() {
                override fun getCount() = items.size
                override fun getItem(position: Int) = items[position]
                override fun getItemId(position: Int) = position.toLong()
                override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
                    val box = convertView as? LinearLayout ?: LinearLayout(this@BookActivity).apply {
                        orientation = LinearLayout.VERTICAL
                        setPadding(dp(20), dp(6), dp(20), dp(6))
                        addView(TextView(context).apply { textSize = 12f; alpha = 0.6f })
                        addView(TextView(context).apply { textSize = 15f })
                    }
                    val (head, body) = items[position]
                    (box.getChildAt(0) as TextView).text = head
                    (box.getChildAt(1) as TextView).apply {
                        text = body
                        visibility = if (body.isEmpty()) View.GONE else View.VISIBLE
                    }
                    return box
                }
            }
        }
        AlertDialog.Builder(this)
            .setTitle(title)
            .setView(list)
            .setPositiveButton("Close", null)
            .show()
            .window?.setLayout(ViewGroup.LayoutParams.MATCH_PARENT, (resources.displayMetrics.heightPixels * 0.85).toInt())
    }

    override fun buildUi(): View {
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        root.addView(toolbar("Book", up = true))
        val list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }

        cover = ImageView(this).apply {
            scaleType = ImageView.ScaleType.CENTER_CROP
            clipToOutline = true
            background = GradientDrawable().apply { cornerRadius = dp(8).toFloat(); setColor(0x22808080) }
        }
        titleView = label(20f, Gravity.START).apply { setTypeface(typeface, android.graphics.Typeface.BOLD) }
        byView = label(14f, Gravity.START).apply { alpha = 0.7f; setPadding(0, dp(4), 0, 0) }
        list.addView(
            LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                setPadding(dp(16), dp(16), dp(16), dp(8))
                addView(cover, LinearLayout.LayoutParams(dp(96), dp(96)).apply { rightMargin = dp(16) })
                addView(
                    LinearLayout(this@BookActivity).apply {
                        orientation = LinearLayout.VERTICAL
                        addView(titleView)
                        addView(byView)
                    },
                    LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
                )
            },
        )

        list.addView(heading("On this phone"))
        phone = row("Storage", null, null)
        list.addView(phone.view)
        keepButton = Button(this).apply {
            setOnClickListener {
                val s = service ?: return@setOnClickListener
                val b = book(s) ?: return@setOnClickListener toast("Open the library once so the book is known")
                s.keepOffline(b, s.storage.book(key)?.pinned != true)
                refresh()
            }
        }
        removeButton = Button(this).apply {
            text = "Remove from phone"
            setOnClickListener {
                val s = service ?: return@setOnClickListener
                AlertDialog.Builder(this@BookActivity)
                    .setTitle("Remove ${titleView.text}?")
                    .setMessage("Its audio, and music no other book uses, are deleted from this phone. It still streams from the server, and its map stays.")
                    .setPositiveButton("Remove") { _, _ -> s.removeBooks(listOf(key)) { runOnUiThread { refresh() } } }
                    .setNegativeButton("Cancel", null)
                    .show()
            }
        }
        list.addView(
            LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                setPadding(dp(12), 0, dp(12), 0)
                addView(keepButton, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                addView(removeButton, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            },
        )
        files = row("Files", null, null)
        files.view.setOnLongClickListener { copy(files); true }
        list.addView(files.view)

        list.addView(divider())
        list.addView(heading("Maps"))
        mapsNote = label(13f, Gravity.START).apply { alpha = 0.7f; setPadding(dp(16), 0, dp(16), dp(4)) }
        list.addView(mapsNote)
        mapsList = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        list.addView(mapsList)

        list.addView(divider())
        list.addView(heading("Transcript"))
        transcriptRow = row("Transcript", null) { service?.let { readTranscript(it) } }
        list.addView(transcriptRow.view)
        voicesRow = row("Who says what", null) {
            val s = service ?: return@row
            if (s.mapServerUrl == null) return@row
            s.makeVoices(id) { problem ->
                toast(problem ?: "Asked. Speakers appear in the subtitles as chapters are done.")
                askMaps()
            }
        }
        list.addView(voicesRow.view)

        list.addView(divider())
        list.addView(heading("Details"))
        details = row("Details", null, null)
        details.view.setOnLongClickListener { copy(details); true }
        list.addView(details.view)

        root.addView(ScrollView(this).apply { addView(list) })
        return root
    }

    /** Copy a row's text, for paths and ids. */
    private fun copy(r: Row) {
        val cm = getSystemService(android.content.ClipboardManager::class.java)
        cm.setPrimaryClip(android.content.ClipData.newPlainText("underscore", r.summary))
        toast("Copied", long = false)
    }

    companion object {
        const val EXTRA_ID = "id"

        fun open(from: android.app.Activity, id: String) {
            from.startActivity(Intent(from, BookActivity::class.java).putExtra(EXTRA_ID, id))
        }

        /** The same 64-bit FNV-1a the Rust side names transcripts with. */
        fun fnv(s: String): Long = s.toByteArray().fold(-0x340d631b7bdddcdbL) { h, b -> (h xor (b.toLong() and 0xff)) * 0x100000001b3L }
    }
}
