package io.github.ronin11.underscore

import org.json.JSONObject

/**
 * Maps and transcripts as lines to read: a time over a line of text. For
 * showing only. The engine parses maps for real (`crates/map`); this reads
 * just enough of the TOML to list the cues, and skips what it does not
 * know.
 */
object MapText {
    private val chapterAt = Regex("""chapter\s*=\s*(\d+)\s*,\s*offset\s*=\s*"([^"]*)"""")
    private val bareAt = Regex("""^\s*"([^"]*)"""")

    private fun str(v: String, field: String): String? =
        Regex("""$field\s*=\s*"((?:[^"\\]|\\.)*)"""").find(v)?.groupValues?.get(1)
            ?.replace("\\\"", "\"")?.replace("\\\\", "\\")

    private fun where(v: String): String {
        chapterAt.find(v)?.let { return "ch ${it.groupValues[1]}  ${it.groupValues[2]}" }
        bareAt.find(v)?.let { return it.groupValues[1] }
        return v.trim()
    }

    /** One line per cue: where it plays, and what. A header line first. */
    fun lines(toml: String): List<Pair<String, String>> {
        val out = mutableListOf<Pair<String, String>>()
        var head = mutableMapOf<String, String>()
        var cue: MutableMap<String, String>? = null
        fun flush() {
            val c = cue ?: return
            val span = where(c["at"] ?: "") + (c["until"]?.let { " → " + where(it) } ?: "")
            val what = when {
                c["silence"] == "true" -> "Silence"
                c["track"] != null -> {
                    val t = c["track"]!!
                    "${str(t, "title") ?: "?"} · ${str(t, "artist") ?: ""}"
                }
                c["vibe"] != null -> "Mood: " + c["vibe"]!!.trim('[', ']', ' ').replace("\"", "")
                else -> "?"
            }
            val extras = listOfNotNull(
                c["level_db"]?.let { "$it dB" },
                c["region"]?.let { if ("repeat = true" in it) "loops" else "part" },
            ).joinToString(", ")
            val why = c["why"]?.let { str("why = $it", "why") }
            val body = what + (if (extras.isNotEmpty()) "  ($extras)" else "") + (why?.let { "\n$it" } ?: "")
            out += span to body
        }
        for (raw in toml.lines()) {
            val line = raw.trim()
            if (line.startsWith("#") || line.isEmpty()) continue
            if (line == "[[cue]]") {
                flush()
                cue = mutableMapOf()
                continue
            }
            if (line.startsWith("[")) {
                flush()
                cue = null
                continue
            }
            val eq = line.indexOf('=')
            if (eq < 0) continue
            val k = line.substring(0, eq).trim()
            val v = line.substring(eq + 1).trim()
            if (cue != null) cue!![k] = v else head[k] = v
        }
        flush()
        val cues = out.size
        val music = out.count { !it.second.startsWith("Silence") }
        val name = head["name"]?.let { str("name = $it", "name") } ?: "Map"
        val state = if (head["complete"] == "false") ", still being made" else ""
        out.add(0, name to "$cues cues, $music with music$state" + (head["id"]?.let { "\nid ${it.trim('"')}" } ?: ""))
        return out
    }

    private fun clock(ms: Long): String {
        val s = ms / 1000
        return if (s >= 3600) "%d:%02d:%02d".format(s / 3600, s / 60 % 60, s % 60) else "%d:%02d".format(s / 60, s % 60)
    }

    /** The phone's transcript: one JSON object per line, `startMs`, `endMs`, `text`. */
    fun phoneTranscript(lines: List<String>): List<Pair<String, String>> = lines.mapNotNull { l ->
        val o = runCatching { JSONObject(l) }.getOrNull() ?: return@mapNotNull null
        val text = o.optString("text").trim()
        if (!o.has("startMs") || text.isEmpty()) null else clock(o.getLong("startMs")) to text
    }

    /** The map server's transcript: chapters of segments, times in seconds from the start. */
    fun serverTranscript(json: String): List<Pair<String, String>> {
        val out = mutableListOf<Pair<String, String>>()
        val chapters = JSONObject(json).optJSONArray("chapters") ?: return out
        for (i in 0 until chapters.length()) {
            val c = chapters.getJSONObject(i)
            out += (c.optString("title").ifEmpty { "Part ${i + 1}" }) to ""
            val segs = c.optJSONArray("segments") ?: continue
            for (j in 0 until segs.length()) {
                val g = segs.getJSONObject(j)
                out += clock((g.optDouble("start") * 1000).toLong()) to g.optString("text").trim()
            }
        }
        return out
    }
}
