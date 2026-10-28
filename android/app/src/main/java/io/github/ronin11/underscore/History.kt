package io.github.ronin11.underscore

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

/**
 * What happened to playback, and what did it: every play, pause, skip,
 * seek, chapter jump, speed change and book opened, newest last. A small
 * hand on the lock screen leaves a trail here, and any entry leads back
 * to where the book was before it.
 */
object History {
    private const val KEEP = 300

    /** Skips mashed in a row become one entry. */
    private const val MERGE_MS = 15_000L

    /** What happened. */
    object What {
        const val PLAY = "play"
        const val PAUSE = "pause"
        const val BACK = "back"
        const val FORWARD = "forward"
        const val SEEK = "seek"
        const val CHAPTER = "chapter"
        const val SPEED = "speed"
        const val OPEN = "open"
        const val RETURN = "return"
    }

    /** What did it. */
    object By {
        const val APP = "In the app"
        const val CONTROLS = "Lock screen, headset or watch"
        const val NOTIFICATION = "Notification"
        const val UNPLUGGED = "Headphones unplugged"
        const val OTHER_APP = "Another app's sound"
        const val SLEEP = "Sleep"
        const val SHAKE = "Shake"
        const val ENGINE = "The book itself"
    }

    /**
     * One change. Positions are in the book, [fromMs] before and [toMs]
     * after; they are the same for a play or a pause. [note] carries a
     * speed change, a chapter's name, or what was playing before a book.
     */
    data class Entry(
        val wallMs: Long,
        val book: String,
        val title: String,
        val what: String,
        val by: String,
        val fromMs: Long,
        val toMs: Long,
        val note: String? = null,
    )

    private var cache: MutableList<Entry>? = null

    @Synchronized
    fun add(context: Context, e: Entry) {
        val all = load(context)
        val prev = all.lastOrNull()
        val merges = prev != null && prev.book == e.book && prev.what == e.what && prev.by == e.by &&
            e.what in listOf(What.BACK, What.FORWARD, What.CHAPTER, What.SPEED) &&
            e.wallMs - prev.wallMs < MERGE_MS
        if (merges && prev != null) {
            all[all.size - 1] = e.copy(fromMs = prev.fromMs, note = merged(prev, e))
        } else {
            all.add(e)
        }
        while (all.size > KEEP) all.removeAt(0)
        prefs(context).edit().putString("entries", JSONArray(all.map { it.json() }).toString()).apply()
    }

    /** Newest first. */
    @Synchronized
    fun entries(context: Context): List<Entry> = load(context).reversed()

    /** A speed change keeps where it started; a chapter keeps where it ended. */
    private fun merged(prev: Entry, e: Entry): String? =
        if (e.what == What.SPEED) "${prev.note?.substringBefore(" ")} ${e.note?.substringAfter(" ")}" else e.note

    private fun load(context: Context): MutableList<Entry> {
        cache?.let { return it }
        val list = mutableListOf<Entry>()
        val text = prefs(context).getString("entries", null)
        if (text != null) {
            runCatching {
                val a = JSONArray(text)
                for (i in 0 until a.length()) {
                    val o = a.getJSONObject(i)
                    list.add(
                        Entry(
                            o.getLong("wall"), o.getString("book"), o.getString("title"), o.getString("what"),
                            o.getString("by"), o.getLong("from"), o.getLong("to"),
                            if (o.has("note")) o.getString("note") else null,
                        ),
                    )
                }
            }
        }
        cache = list
        return list
    }

    private fun Entry.json(): JSONObject = JSONObject().apply {
        put("wall", wallMs)
        put("book", book)
        put("title", title)
        put("what", what)
        put("by", by)
        put("from", fromMs)
        put("to", toMs)
        note?.let { put("note", it) }
    }

    private fun prefs(context: Context) = context.getSharedPreferences("history", Context.MODE_PRIVATE)
}
