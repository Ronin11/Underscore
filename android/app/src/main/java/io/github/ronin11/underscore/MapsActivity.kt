package io.github.ronin11.underscore

import android.content.res.ColorStateList
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.view.Gravity
import android.view.View
import android.widget.ProgressBar
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import org.json.JSONObject

/**
 * The music under the open book: the cue before, the one playing and the
 * next, music on or off, the
 * book's maps on the map server, and a map from a file. The player's
 * Music tile opens it.
 */
class MapsActivity : ServiceActivity() {

    override val tab: Tab? = null

    private lateinit var bar: androidx.appcompat.widget.Toolbar
    private lateinit var nowView: TextView
    private lateinit var nowNote: TextView
    private lateinit var card: LinearLayout
    private lateinit var before: CueLine
    private lateinit var next: CueLine
    private lateinit var nowTitle: TextView
    private lateinit var nowArtist: TextView
    private lateinit var nowBar: ProgressBar
    private lateinit var nowElapsed: TextView
    private lateinit var nowLeft: TextView
    private lateinit var nowWhy: TextView
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
        val following = loaded && !s.mapLoading && e.musicEnabled && e.hasMap && status.startsWith("Map: ")
        nowView.text = when {
            !loaded -> "No book open"
            s.mapLoading -> "Loading the map…"
            !e.musicEnabled -> "Music is off"
            !e.hasMap -> if (s.musicName != null) s.musicName else "No map"
            else -> status.removePrefix("Map loaded, ")
        }
        nowView.visibility = if (following) View.GONE else View.VISIBLE
        card.visibility = if (following) View.VISIBLE else View.GONE
        if (following) showNearby(e.mapNearby, e.positionMs, e.durationMs, e.speed)
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

    /** A field of a cue from the engine, or null when absent, null or empty. */
    private fun JSONObject.str(key: String): String? = if (isNull(key)) null else optString(key).ifEmpty { null }

    /** What a cue plays, in words. */
    private fun what(c: JSONObject): String = when (c.optString("kind")) {
        "silence" -> "Silence"
        else -> c.str("title") ?: "Unknown"
    }

    /** The line under a cue's title: who, and how it was found. */
    private fun by(c: JSONObject): String? = when (c.optString("kind")) {
        "vibe" -> listOfNotNull(c.str("artist"), "picked for the mood").joinToString(" · ")
        "missing" -> listOfNotNull(c.str("artist"), "not in your music").joinToString(" · ")
        "silence" -> null
        else -> c.str("artist")
    }

    /**
     * The card: the cue before, the one playing, and the next. Times are
     * as you hear them, so at 1.5x a cue's minutes go by faster. In a gap
     * between cues, the middle says how long the quiet lasts.
     */
    private fun showNearby(json: String, at: Long, length: Long, speed: Float) {
        val v = runCatching { JSONObject(json) }.getOrNull() ?: JSONObject()
        val b = v.optJSONObject("before")
        val n = v.optJSONObject("now")
        val x = v.optJSONObject("next")
        fun heard(ms: Long) = fmt((ms.coerceAtLeast(0) / speed).toLong())
        fun end(c: JSONObject) = if (c.isNull("endMs")) length else c.optLong("endMs")

        before.view.visibility = if (b == null) View.GONE else View.VISIBLE
        if (b != null) {
            before.title.text = what(b)
            before.show(before.sub, by(b))
            before.side.text = "ended ${heard(at - end(b))} ago"
        }

        val from = n?.optLong("startMs") ?: b?.let { end(it) } ?: 0L
        val to = n?.let { end(it) } ?: x?.optLong("startMs") ?: length
        nowTitle.text = n?.let { what(it) } ?: "Quiet"
        val artist = n?.let { by(it) }
        nowArtist.text = artist ?: ""
        nowArtist.visibility = if (artist == null) View.GONE else View.VISIBLE
        nowBar.progress = if (to > from) (((at - from) * 1000) / (to - from)).toInt().coerceIn(0, 1000) else 0
        nowElapsed.text = heard(at - from)
        nowLeft.text = when {
            n != null -> "${heard(to - at)} left"
            x != null -> "${heard(to - at)} until the next cue"
            else -> "to the end of the book"
        }
        val why = n?.str("why")
        nowWhy.text = why?.let { "“$it”" } ?: ""
        nowWhy.visibility = if (why == null) View.GONE else View.VISIBLE

        next.view.visibility = if (x == null) View.GONE else View.VISIBLE
        if (x != null) {
            val start = x.optLong("startMs")
            // A quiet stretch between this cue and the next is said, so it does not seem broken.
            val quiet = if (n != null) start - end(n) else 0L
            next.show(next.note, if (quiet >= 1000) "Quiet for ${heard(quiet)}, then" else null)
            next.title.text = what(x)
            next.show(next.sub, by(x))
            next.side.text = "in ${heard(start - at)}"
        }
    }

    /** Before or next in the card: a caption, a note above the music, the music, who, and a time. */
    private class CueLine(val view: View, val note: TextView, val title: TextView, val sub: TextView, val side: TextView) {
        fun show(t: TextView, text: String?) {
            t.text = text ?: ""
            t.visibility = if (text == null) View.GONE else View.VISIBLE
        }
    }

    private fun cueLine(caption: String): CueLine {
        val note = label(12f, Gravity.START).apply { alpha = 0.7f }
        val title = label(15f, Gravity.START)
        val sub = label(12f, Gravity.START).apply { alpha = 0.7f }
        val side = label(12f, Gravity.END).apply { alpha = 0.7f }
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(16), dp(12), dp(16), dp(12))
            addView(
                label(11f, Gravity.START).apply { text = caption; alpha = 0.7f; letterSpacing = 0.08f },
                LinearLayout.LayoutParams(dp(52), LinearLayout.LayoutParams.WRAP_CONTENT),
            )
            addView(
                LinearLayout(context).apply {
                    orientation = LinearLayout.VERTICAL
                    addView(note); addView(title); addView(sub)
                },
                LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f),
            )
            addView(side)
        }
        return CueLine(box, note, title, sub, side)
    }

    /** A thin rule inside the card. */
    private fun rule(): View = divider().apply {
        (layoutParams as LinearLayout.LayoutParams).setMargins(dp(16), 0, dp(16), 0)
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

        val accent = themeColor(androidx.appcompat.R.attr.colorAccent)
        before = cueLine("BEFORE").also { it.view.alpha = 0.75f }
        next = cueLine("NEXT")
        nowTitle = label(22f, Gravity.START).apply { setTypeface(typeface, Typeface.BOLD) }
        nowArtist = label(14f, Gravity.START).apply { alpha = 0.7f }
        nowBar = ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply {
            max = 1000
            progressTintList = ColorStateList.valueOf(accent)
        }
        nowElapsed = label(12f, Gravity.START).apply { alpha = 0.7f }
        nowLeft = label(12f, Gravity.END).apply { alpha = 0.7f }
        nowWhy = label(14f, Gravity.START).apply {
            setTypeface(typeface, Typeface.ITALIC)
            setLineSpacing(0f, 1.2f)
            setPadding(0, dp(8), 0, 0)
        }
        val nowBox = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(16), dp(16), dp(16))
            addView(label(11f, Gravity.START).apply {
                text = "NOW"
                letterSpacing = 0.08f
                setTypeface(typeface, Typeface.BOLD)
                setTextColor(accent)
                setPadding(0, 0, 0, dp(6))
            })
            addView(nowTitle)
            addView(nowArtist)
            addView(nowBar, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, dp(12)).apply { topMargin = dp(10) })
            addView(LinearLayout(context).apply {
                orientation = LinearLayout.HORIZONTAL
                addView(nowElapsed, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
                addView(nowLeft, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
            })
            addView(nowWhy)
        }
        card = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = GradientDrawable().apply { cornerRadius = dp(16).toFloat(); setColor(0x1A808080) }
            addView(before.view)
            addView(rule())
            addView(nowBox)
            addView(rule())
            addView(next.view)
            visibility = View.GONE
        }
        list.addView(
            card,
            LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply {
                setMargins(dp(16), dp(4), dp(16), dp(4))
            },
        )
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
