package io.github.ronin11.underscore

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * What is on the phone, book by book: each book's own audio files in the
 * download cache, and the music its map plays, which other books may
 * share. This list is what lets a book go without taking music another
 * book still needs. Which books and files go when space runs short is
 * decided in Rust (`crates/library/src/keep.rs`); this keeps the list and
 * does the deleting.
 *
 * Each book's map is kept here too, by book, so a book plays with its
 * music offline and coming back to a book brings its map back. Maps and
 * transcripts are small and are never removed to make room.
 */
class Storage(context: Context) {
    private val file = File(context.filesDir, "storage.json")
    private val mapsDir = File(context.filesDir, "maps")

    /** One book on the phone. [audio], in order, and [music] are cache file names. */
    data class Book(
        val key: String,
        val title: String,
        val author: String = "",
        val series: String = "",
        val coverUrl: String? = null,
        val durationMs: Long = 0,
        val audio: List<String> = emptyList(),
        /** Every audio file is whole in the cache. */
        val complete: Boolean = false,
        val music: Set<String> = emptySet(),
        /** When it was last opened, in wall-clock milliseconds. */
        val played: Long = 0,
        /** Kept for offline listening: whole, with its map's music, never removed to make room. */
        val pinned: Boolean = false,
        /** Its transcript came from beside it on the server, so captions show without transcribing. */
        val serverTranscript: Boolean = false,
        /** Where the listener was, for opening it again without the server. */
        val positionMs: Long = 0,
    ) {
        /** The server book's id, for a key such as `abs:<id>`. */
        val serverId: String? get() = key.removePrefix("abs:").takeIf { key.startsWith("abs:") }
    }

    @Synchronized
    fun books(): List<Book> = load().values.toList()

    @Synchronized
    fun book(key: String): Book? = load()[key]

    /** Change [key]'s entry, creating it from [title] if there is none. */
    @Synchronized
    fun update(key: String, title: String, change: (Book) -> Book) {
        val all = load()
        all[key] = change(all[key] ?: Book(key, title))
        save(all)
    }

    @Synchronized
    fun forget(keys: Collection<String>) {
        if (keys.isEmpty()) return
        val all = load()
        keys.forEach { all.remove(it) }
        save(all)
    }

    /** The map kept for book [key], with the name it was loaded as. */
    fun map(key: String): Pair<String, String>? {
        val f = mapFile(key)
        val text = runCatching { f.readText() }.getOrNull() ?: return null
        val name = runCatching { File(f.path + ".name").readText() }.getOrNull() ?: "map"
        return text to name
    }

    fun saveMap(key: String, toml: String, name: String) {
        mapsDir.mkdirs()
        runCatching {
            mapFile(key).writeText(toml)
            File(mapFile(key).path + ".name").writeText(name)
        }
    }

    fun forgetMap(key: String) {
        mapFile(key).delete()
        File(mapFile(key).path + ".name").delete()
    }

    /** Book keys hold any characters; the file name is a hash of the key. */
    private fun mapFile(key: String): File =
        File(mapsDir, "%016x.toml".format(key.fold(-0x340d631b7bdddcdbL) { h, c -> (h xor c.code.toLong()) * 0x100000001b3L }))

    /**
     * Ask Rust what to drop and drop it: books least recently played,
     * with the files only they used, until the cache fits [budget] bytes
     * (null for no limit), and the books in [remove] whatever the budget.
     * [open] and [busy] stay. Returns the bytes left. Reads and deletes
     * files: not on the main thread.
     */
    fun trim(engine: Engine, cache: File, budget: Long?, open: String?, busy: Set<String>, remove: Collection<String> = emptyList()): Long {
        val files = (cache.listFiles() ?: emptyArray()).filter { it.isFile && !it.name.endsWith(".part") }
        val ask = JSONObject()
            .put("books", JSONArray(books().map { b ->
                JSONObject().put("key", b.key).put("files", JSONArray(b.audio + b.music))
                    .put("played", b.played).put("pinned", b.pinned)
            }))
            .put("files", JSONArray(files.map { f ->
                JSONObject().put("name", f.name).put("bytes", f.length()).put("modified", f.lastModified())
            }))
            .put("budget", budget ?: JSONObject.NULL)
            .put("open", JSONArray(listOfNotNull(open)))
            .put("busy", JSONArray(busy.toList()))
            .put("remove", JSONArray(remove.toList()))
        val plan = JSONObject(engine.storagePlan(ask.toString()))
        if (plan.has("error")) {
            android.util.Log.w("underscore", "storage plan: ${plan.getString("error")}")
            return files.sumOf { it.length() }
        }
        val names = plan.getJSONArray("files")
        for (i in 0 until names.length()) File(cache, names.getString(i)).delete()
        val gone = plan.getJSONArray("books")
        forget((0 until gone.length()).map { gone.getString(it) })
        return plan.optLong("bytes")
    }

    /**
     * Bytes each book takes: its own files, which go with it, and files it
     * shares with other books, which stay. Reads the folder.
     */
    fun sizes(cache: File): Map<String, Pair<Long, Long>> {
        val bytes = (cache.listFiles() ?: emptyArray()).associate { it.name to it.length() }
        val all = books()
        val users = HashMap<String, Int>()
        for (b in all) for (f in b.audio + b.music) users[f] = (users[f] ?: 0) + 1
        return all.associate { b ->
            val files = b.audio + b.music
            val own = files.filter { users[it] == 1 }.sumOf { bytes[it] ?: 0 }
            val shared = files.filter { (users[it] ?: 0) > 1 }.sumOf { bytes[it] ?: 0 }
            b.key to (own to shared)
        }
    }

    /** Cache files no book lists, such as music from a map since changed. */
    fun unlisted(cache: File): List<File> {
        val listed = books().flatMap { it.audio + it.music }.toSet()
        return (cache.listFiles() ?: emptyArray()).filter { it.isFile && !it.name.endsWith(".part") && it.name !in listed }
    }

    private fun load(): LinkedHashMap<String, Book> {
        val out = LinkedHashMap<String, Book>()
        val text = runCatching { file.readText() }.getOrNull() ?: return out
        val arr = runCatching { JSONObject(text).getJSONArray("books") }.getOrNull() ?: return out
        for (i in 0 until arr.length()) {
            val o = arr.getJSONObject(i)
            fun names(k: String) = o.optJSONArray(k)?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList()
            val b = Book(
                key = o.getString("key"),
                title = o.optString("title"),
                author = o.optString("author"),
                series = o.optString("series"),
                coverUrl = o.optString("cover").ifEmpty { null },
                durationMs = o.optLong("durationMs"),
                audio = names("audio"),
                complete = o.optBoolean("complete"),
                music = names("music").toSet(),
                played = o.optLong("played"),
                pinned = o.optBoolean("pinned"),
                serverTranscript = o.optBoolean("serverTranscript"),
                positionMs = o.optLong("positionMs"),
            )
            out[b.key] = b
        }
        return out
    }

    private fun save(all: Map<String, Book>) {
        val arr = JSONArray()
        for (b in all.values) {
            arr.put(
                JSONObject().put("key", b.key).put("title", b.title).put("author", b.author)
                    .put("series", b.series).put("cover", b.coverUrl ?: "").put("durationMs", b.durationMs)
                    .put("audio", JSONArray(b.audio.toList())).put("complete", b.complete)
                    .put("music", JSONArray(b.music.toList()))
                    .put("played", b.played).put("pinned", b.pinned).put("serverTranscript", b.serverTranscript)
                    .put("positionMs", b.positionMs),
            )
        }
        val tmp = File(file.path + ".tmp")
        tmp.writeText(JSONObject().put("books", arr).toString())
        tmp.renameTo(file)
    }
}
