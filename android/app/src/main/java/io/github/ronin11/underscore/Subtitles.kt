package io.github.ronin11.underscore

import org.json.JSONObject
import java.io.File

/**
 * The open book's lines, for subtitles: from its voices file when the map
 * server has worked out who says what, otherwise from its transcript,
 * all the narrator's. Read from files the service keeps; loading reads the
 * disk, so it runs off the main thread, and lookups are a binary search.
 */
class Subtitles(private val filesDir: File) {
    data class Span(val who: String, val text: String)
    data class Line(val startMs: Long, val endMs: Long, val spans: List<Span>) {
        val text: String get() = spans.joinToString(" ") { it.text }
    }
    data class Person(val id: String, val name: String, val color: Int)

    /** The book these lines are for. */
    @Volatile var key: String? = null
        private set
    @Volatile var lines: List<Line> = emptyList()
        private set
    @Volatile var cast: Map<String, Person> = emptyMap()
        private set
    /** Whether the lines know who says what. */
    @Volatile var voiced = false
        private set
    /** When the voices file was saved, to know when the server has newer. */
    val voicesSaved: Long get() = key?.let { voicesFile(it).lastModified() } ?: 0L

    private fun voicesFile(key: String) = File(File(filesDir, "voices"), "%016x.json".format(fnv(key)))
    private fun transcriptFile(key: String) = File(File(filesDir, "whisper/transcripts"), "%016x.jsonl".format(fnv(key)))

    /** Keep the map server's voices for [key], and load them if it is the book shown. */
    fun saveVoices(key: String, json: String) {
        val f = voicesFile(key)
        f.parentFile?.mkdirs()
        val tmp = File(f.path + ".tmp")
        tmp.writeText(json)
        tmp.renameTo(f)
        if (this.key == key) load(key)
    }

    /** Load [key]'s lines: its voices if kept, else its transcript. Reads files. */
    fun load(key: String) {
        val voices = voicesFile(key).takeIf { it.isFile }?.let { runCatching { readVoices(it.readText()) }.getOrNull() }
        if (voices != null) {
            this.cast = voices.second
            this.lines = voices.first
            this.voiced = true
        } else {
            this.cast = emptyMap()
            this.lines = transcriptFile(key).takeIf { it.isFile }?.let { readTranscript(it) } ?: emptyList()
            this.voiced = false
        }
        this.key = key
    }

    /** Index of the line playing at [ms], or of the last one before it; -1 before the first. */
    fun at(ms: Long): Int {
        val l = lines
        var lo = 0
        var hi = l.size - 1
        var found = -1
        while (lo <= hi) {
            val mid = (lo + hi) ushr 1
            if (l[mid].startMs <= ms) {
                found = mid
                lo = mid + 1
            } else {
                hi = mid - 1
            }
        }
        return found
    }

    /** The line playing at [ms], if one is: not after it ended, give or take a moment. */
    fun current(ms: Long): Line? = lines.getOrNull(at(ms))?.takeIf { ms <= it.endMs + GRACE_MS }

    fun name(who: String): String? = cast[who]?.name

    fun color(who: String): Int = cast[who]?.color ?: NARRATOR_COLOR

    private fun readTranscript(f: File): List<Line> = f.readLines().mapNotNull { l ->
        val o = runCatching { JSONObject(l) }.getOrNull() ?: return@mapNotNull null
        val text = o.optString("text").trim()
        if (!o.has("startMs") || text.isEmpty()) null
        else Line(o.getLong("startMs"), o.optLong("endMs", o.getLong("startMs")), listOf(Span(NARRATOR, text)))
    }.sortedBy { it.startMs }

    private fun readVoices(json: String): Pair<List<Line>, Map<String, Person>> {
        val o = JSONObject(json)
        val castArr = o.optJSONArray("cast")
        val cast = LinkedHashMap<String, Person>()
        if (castArr != null) {
            for (i in 0 until castArr.length()) {
                val c = castArr.getJSONObject(i)
                val id = c.getString("id")
                cast[id] = Person(id, c.optString("name", id), PALETTE[cast.size % PALETTE.size])
            }
        }
        val out = ArrayList<Line>()
        val chapters = o.optJSONArray("chapters")
        if (chapters != null) {
            for (i in 0 until chapters.length()) {
                val lines = chapters.getJSONObject(i).optJSONArray("lines") ?: continue
                for (j in 0 until lines.length()) {
                    val l = lines.getJSONObject(j)
                    val spans = l.optJSONArray("spans") ?: continue
                    val list = (0 until spans.length()).map { k ->
                        val s = spans.getJSONObject(k)
                        Span(s.optString("who", NARRATOR), s.optString("text").trim())
                    }.filter { it.text.isNotEmpty() }
                    if (list.isEmpty()) continue
                    out += Line((l.optDouble("start") * 1000).toLong(), (l.optDouble("end") * 1000).toLong(), list)
                }
            }
        }
        out.sortBy { it.startMs }
        return out to cast
    }

    companion object {
        const val NARRATOR = "narrator"
        /** How long a line stays up after it ends, so short gaps do not flicker. */
        private const val GRACE_MS = 1500L
        const val NARRATOR_COLOR = 0xFFE6E6E6.toInt()
        /** Speaker colors: light enough to read on black, apart enough to tell apart. */
        private val PALETTE = intArrayOf(
            0xFF7FD4FF.toInt(), // sky
            0xFFFFC857.toInt(), // amber
            0xFF8EF0B0.toInt(), // mint
            0xFFFF9EC7.toInt(), // pink
            0xFFC3A6FF.toInt(), // lavender
            0xFFFF8A65.toInt(), // coral
            0xFFD4F06A.toInt(), // lime
        )

        /** The 64-bit FNV-1a the Rust side names transcripts with. */
        fun fnv(s: String): Long = s.toByteArray().fold(-0x340d631b7bdddcdbL) { h, b -> (h xor (b.toLong() and 0xff)) * 0x100000001b3L }
    }
}
