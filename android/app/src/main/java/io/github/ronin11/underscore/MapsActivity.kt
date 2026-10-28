package io.github.ronin11.underscore

import android.net.Uri
import android.view.View
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog

/**
 * The music under the open book: what plays now, music on or off, the
 * book's maps on the map server, and a map from a file. The player's
 * Music tile opens it.
 */
class MapsActivity : ServiceActivity() {

    override val tab: Tab? = null

    private lateinit var bar: androidx.appcompat.widget.Toolbar
    private lateinit var nowView: TextView
    private lateinit var nowNote: TextView
    private lateinit var musicRow: Row
    private lateinit var mapsList: LinearLayout
    private lateinit var readRow: Row
    private lateinit var clearRow: Row
    private var mapsShown: String? = null

    private val pickMap = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let { loadMap(it) }
    }

    private fun loadMap(uri: Uri) {
        val s = service ?: return
        val name = displayName(uri)
        val text = try {
            contentResolver.openInputStream(uri)?.bufferedReader()?.use { it.readText() }
        } catch (e: Exception) {
            null
        }
        if (text == null) {
            toast("Could not read ${name ?: "the map"}")
            return
        }
        s.loadMap(text, name) { problem ->
            if (problem != null) toast(problem)
            refresh()
        }
    }

    override fun refresh() {
        val s = service ?: return
        val e = s.engine
        val loaded = s.speechName != null
        bar.subtitle = s.speechName

        // The engine's status: "Map: 210 of 220 cues found. Now: …", or why it is not following.
        val status = e.mapStatus
        val now = status.substringAfter(". Now: ", "").substringBefore(". Last error: ")
        nowView.text = when {
            !loaded -> "No book open"
            s.mapLoading -> "Loading the map…"
            !e.musicEnabled -> "Music is off"
            !e.hasMap -> if (s.musicName != null) s.musicName else "No map"
            now.isEmpty() || now == "nothing yet" -> "Nothing yet"
            else -> now
        }
        nowNote.text = buildList {
            if (e.hasMap) {
                s.mapName?.let { add(it) }
                val found = Regex("""\d+ of \d+ cues found""").find(status)?.value
                add(found ?: status)
                status.substringAfter(". Last error: ", "").takeIf { it.isNotEmpty() }?.let { add("Last error: $it") }
            } else if (loaded && !s.mapLoading) {
                add("Pick one of the book's maps below, or load one from a file.")
            }
        }.joinToString("\n")
        nowNote.visibility = if (nowNote.text.isEmpty()) View.GONE else View.VISIBLE

        musicRow.summary = if (e.musicEnabled) "On. Tap to turn it off." else "Off. Tap to turn it on."
        readRow.view.isEnabled = e.hasMap && s.openKey?.let { s.storage.map(it) } != null
        readRow.view.alpha = if (readRow.view.isEnabled) 1f else 0.4f
        clearRow.view.isEnabled = e.hasMap
        clearRow.view.alpha = if (e.hasMap) 1f else 0.4f
        showMaps(s)
    }

    /** The book's maps on the map server, rebuilt only when they change. */
    private fun showMaps(s: PlaybackService) {
        val bm = s.bookMaps?.takeIf { s.bookFromServer && s.openKey == "abs:${it.book}" }
        val job = bm?.job?.takeIf { it.active }
        val shape = listOf(
            s.mapServerUrl, s.bookFromServer, s.speechName, s.mapName, bm?.default,
            bm?.maps?.joinToString { "${it.id}${it.updated}${it.cues}" }, job?.done?.size, job?.message,
        ).joinToString("|")
        if (shape == mapsShown) return
        mapsShown = shape
        mapsList.removeAllViews()
        when {
            s.speechName == null -> {
                mapsList.addView(row("Open a book first", null, null).view)
                return
            }
            !s.bookFromServer -> {
                mapsList.addView(row("Maps from the map server are for Audiobookshelf books", "Load a map from a file below.", null).view)
                return
            }
            s.mapServerUrl == null -> {
                mapsList.addView(row("Add a map server", "Under Connections. It keeps maps for your books and makes new ones.") { go(Tab.CONNECTIONS) }.view)
                return
            }
            bm == null -> {
                mapsList.addView(row(s.mapServerProblem ?: "Asking the map server…", null, null).view)
                return
            }
        }
        if (job != null) {
            val of = if (job.total > 0) "${job.done.size} of ${job.total} chapters" else "starting"
            mapsList.addView(row("Being made", "$of. ${job.message}", null).view)
        }
        if (bm.maps.isEmpty() && job == null) mapsList.addView(row("No maps for this book yet", null, null).view)
        for (m in bm.maps) {
            val chosen = m.id == bm.default
            val state = if (m.complete) "${m.cues} cues" else "${m.cues} cues so far, still being made"
            val note = if (chosen) "Plays with this book.  $state" else state
            mapsList.addView(row((if (chosen) "● " else "") + m.name, note) { mapActions(s, m, chosen) }.view)
        }
        mapsList.addView(
            row("Make a new map", "The map server's worker writes one, a chapter at a time, from the chapter you are in.") {
                if (job != null) {
                    toast("A map is already being made for this book")
                } else {
                    s.makeNewMap()
                    toast("Asked the map server. Music starts as the first chapter is ready.")
                }
            }.view,
        )
    }

    private fun mapActions(s: PlaybackService, m: PlaybackService.ServerMap, chosen: Boolean) {
        val items = mutableListOf<String>()
        if (!chosen) items += "Play this book with it"
        items += "Read it"
        AlertDialog.Builder(this)
            .setTitle(m.name)
            .setItems(items.toTypedArray()) { _, which ->
                when (items[which]) {
                    "Read it" -> readServerMap(s, m)
                    else -> { s.chooseMap(m.id); refresh() }
                }
            }
            .show()
    }

    /** Chapter titles for the open book, as maps count marks: from 1. */
    private fun titles(s: PlaybackService): (Int) -> String? = { n -> s.engine.chapterTitle(n - 1) }

    private fun readServerMap(s: PlaybackService, m: PlaybackService.ServerMap) {
        toast("Fetching ${m.name}…", long = false)
        Thread({
            val toml = s.engine.mapServerMap(m.id)
            runOnUiThread {
                if (toml == null) toast("The map server did not send it") else lines(m.name, MapText.lines(toml, titles(s)))
            }
        }, "underscore-maps-read").start()
    }

    /** The map playing now, as the phone keeps it. */
    private fun readLoaded() {
        val s = service ?: return
        val (toml, name) = s.openKey?.let { s.storage.map(it) } ?: return
        lines(s.mapName ?: name, MapText.lines(toml, titles(s)))
    }

    override fun buildUi(): View {
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        bar = toolbar("Music", up = true)
        root.addView(bar)
        val list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }

        list.addView(heading("Playing now"))
        nowView = label(18f, android.view.Gravity.START).apply {
            setTypeface(typeface, android.graphics.Typeface.BOLD)
            setPadding(dp(16), 0, dp(16), 0)
        }
        nowNote = label(13f, android.view.Gravity.START).apply {
            alpha = 0.7f
            setPadding(dp(16), dp(4), dp(16), dp(8))
        }
        list.addView(nowView)
        list.addView(nowNote)
        musicRow = row("Music", null) { service?.engine?.let { it.setMusicEnabled(!it.musicEnabled) } }
        list.addView(musicRow.view)
        readRow = row("Read this map", "Every cue: where it plays, what, and why.") { readLoaded() }
        list.addView(readRow.view)

        list.addView(divider())
        list.addView(heading("Maps for this book"))
        mapsList = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        list.addView(mapsList)

        list.addView(divider())
        list.addView(heading("From a file"))
        list.addView(
            row("Load a map file…", "A map in TOML, played with the music server or the music folder.") {
                val s = service ?: return@row
                if (s.musicDir == null && !s.engine.hasMusicServer) {
                    toast("Sign in to the music server or set the music folder in Connections first")
                } else {
                    pickMap.launch(MAP_TYPES)
                }
            }.view,
        )
        clearRow = row("Clear the map", "Stop following it, and forget it for this book.") {
            service?.clearMap()
            mapsShown = null
        }
        list.addView(clearRow.view)

        root.addView(
            ScrollView(this).apply { addView(list) },
            LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f),
        )
        return root
    }

    companion object {
        // Maps are TOML; the picker rarely knows that type, so allow anything.
        private val MAP_TYPES = arrayOf("*/*")
    }
}
