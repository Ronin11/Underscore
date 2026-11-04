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
 * Each book's maps are kept here too, as many as it has, with which one it
 * plays with, so a book plays with its music offline and coming back to a
 * book brings its map back. Maps and transcripts are small and are never
 * removed to make room.
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

    /** A map kept for a book. [chosen] is the one the book plays with. */
    data class KeptMap(val name: String, val chosen: Boolean)

    /** The maps kept for book [key], oldest first. */
    @Synchronized
    fun maps(key: String): List<KeptMap> {
        val shelf = shelf(key)
        return shelf.files.keys.map { KeptMap(it, it == shelf.chosen) }
    }

    /** The map book [key] plays with, and its name. */
    @Synchronized
    fun map(key: String): Pair<String, String>? {
        val name = shelf(key).chosen ?: return null
        return map(key, name)?.let { it to name }
    }

    /** The kept map called [name], or null. */
    @Synchronized
    fun map(key: String, name: String): String? {
        val file = shelf(key).files[name] ?: return null
        return runCatching { File(bookDir(key), file).readText() }.getOrNull()
    }

    /** Whether the listener chose the playing map, so nothing from a server takes its place. */
    @Synchronized
    fun mapPinned(key: String): Boolean = shelf(key).pinned

    /**
     * Keep [toml] as [name] and have the book play with it. A map of the
     * same name is replaced. [pinned] says whether the listener chose it;
     * null keeps what was known about this map.
     */
    @Synchronized
    fun saveMap(key: String, toml: String, name: String, pinned: Boolean? = null) {
        val shelf = shelf(key)
        runCatching {
            write(key, shelf, name, toml)
            shelf.pinned = pinned ?: (shelf.pinned && shelf.chosen == name)
            shelf.chosen = name
            writeShelf(key, shelf)
        }
    }

    /** Keep [toml] as [name] without playing it. */
    @Synchronized
    fun keepMap(key: String, toml: String, name: String) {
        val shelf = shelf(key)
        runCatching {
            write(key, shelf, name, toml)
            writeShelf(key, shelf)
        }
    }

    /** The listener chose a map the server keeps track of: the server's default rules again. */
    @Synchronized
    fun unpinMap(key: String) {
        val shelf = shelf(key)
        if (!shelf.pinned) return
        shelf.pinned = false
        runCatching { writeShelf(key, shelf) }
    }

    /** Forget map [name], by default the one the book plays with. */
    @Synchronized
    fun forgetMap(key: String, name: String? = null) {
        val shelf = shelf(key)
        val gone = name ?: shelf.chosen ?: return
        shelf.files.remove(gone)?.let { File(bookDir(key), it).delete() }
        if (shelf.chosen == gone) {
            shelf.chosen = null
            shelf.pinned = false
        }
        runCatching { writeShelf(key, shelf) }
    }

    /** What is kept for one book: map names to files, and which map plays. */
    private class Shelf(var chosen: String? = null, var pinned: Boolean = false, val files: LinkedHashMap<String, String> = LinkedHashMap())

    private fun shelf(key: String): Shelf {
        val shelf = Shelf()
        val text = runCatching { File(bookDir(key), "index.json").readText() }.getOrNull()
        if (text == null) return carryOverMap(key, shelf)
        val o = runCatching { JSONObject(text) }.getOrNull() ?: return shelf
        shelf.chosen = if (o.isNull("chosen")) null else o.optString("chosen").ifEmpty { null }
        shelf.pinned = o.optBoolean("pinned")
        val maps = o.optJSONArray("maps") ?: return shelf
        for (i in 0 until maps.length()) {
            val m = maps.getJSONObject(i)
            shelf.files[m.getString("name")] = m.getString("file")
        }
        return shelf
    }

    /**
     * Before a book could keep several maps, it had one file. A file's or
     * the phone's own map was the listener's choice; a server's was not.
     */
    private fun carryOverMap(key: String, shelf: Shelf): Shelf {
        val old = File(mapsDir, "${hash(key)}.toml")
        val oldName = File(old.path + ".name")
        val toml = runCatching { old.readText() }.getOrNull() ?: return shelf
        val name = runCatching { oldName.readText() }.getOrNull() ?: "map"
        runCatching {
            write(key, shelf, name, toml)
            shelf.chosen = name
            shelf.pinned = name == PlaybackService.HEARD_MAP || name.endsWith(".toml")
            writeShelf(key, shelf)
            old.delete()
            oldName.delete()
        }
        return shelf
    }

    private fun write(key: String, shelf: Shelf, name: String, toml: String) {
        val file = "${hash(name)}.toml"
        val dir = bookDir(key).apply { mkdirs() }
        File(dir, "$file.tmp").apply { writeText(toml); renameTo(File(dir, file)) }
        shelf.files[name] = file
    }

    private fun writeShelf(key: String, shelf: Shelf) {
        val maps = JSONArray()
        for ((name, file) in shelf.files) maps.put(JSONObject().put("name", name).put("file", file))
        val text = JSONObject().put("chosen", shelf.chosen ?: JSONObject.NULL).put("pinned", shelf.pinned).put("maps", maps).toString()
        val dir = bookDir(key).apply { mkdirs() }
        File(dir, "index.json.tmp").apply { writeText(text); renameTo(File(dir, "index.json")) }
    }

    private fun bookDir(key: String) = File(mapsDir, hash(key))

    /** Book keys and map names hold any characters; file names are a hash of them. */
    private fun hash(text: String): String =
        "%016x".format(text.fold(-0x340d631b7bdddcdbL) { h, c -> (h xor c.code.toLong()) * 0x100000001b3L })

    /** What a trim left: the bytes in the cache, and with ask first, the books that would make room. */
    data class Trimmed(val bytes: Long, val suggest: List<String>)

    /**
     * Ask Rust what to drop and drop it: books least recently played,
     * with the files only they used, until the cache fits [budget] bytes
     * (null for no limit), and the books in [remove] whatever the budget.
     * With [askFirst] no book goes to make room; the ones that would come
     * back as suggestions. [open] and [busy] stay. Reads and deletes
     * files: not on the main thread.
     */
    fun trim(
        engine: Engine,
        cache: File,
        budget: Long?,
        open: String?,
        busy: Set<String>,
        remove: Collection<String> = emptyList(),
        askFirst: Boolean = false,
    ): Trimmed {
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
            .put("ask_first", askFirst)
        val plan = JSONObject(engine.storagePlan(ask.toString()))
        if (plan.has("error")) {
            android.util.Log.w("underscore", "storage plan: ${plan.getString("error")}")
            return Trimmed(files.sumOf { it.length() }, emptyList())
        }
        val names = plan.getJSONArray("files")
        for (i in 0 until names.length()) File(cache, names.getString(i)).delete()
        val gone = plan.getJSONArray("books")
        forget((0 until gone.length()).map { gone.getString(it) })
        val suggest = plan.optJSONArray("suggest")?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList()
        return Trimmed(plan.optLong("bytes"), suggest)
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
