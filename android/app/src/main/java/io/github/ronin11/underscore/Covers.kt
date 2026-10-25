package io.github.ronin11.underscore

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.util.LruCache
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.Executors

/**
 * Book covers for every screen: from the Audiobookshelf server, which
 * serves them without a login, or from the art inside a book file. Kept in
 * memory and, for server covers, under the cache folder, so a cover is
 * fetched once.
 */
object Covers {
    private val memory = LruCache<String, Bitmap>(80)
    private val pending = HashSet<String>()
    private val pool = Executors.newFixedThreadPool(3)
    private val main = Handler(Looper.getMainLooper())

    /** The cover for [key] if it is already in memory. */
    fun cached(key: String): Bitmap? = memory.get(key)

    /**
     * Fetch the server cover at [url] for [key], from the cache folder or
     * the server, and hand it to [onLoaded] on the main thread. Does
     * nothing when it is in memory already or on its way.
     */
    fun load(context: Context, key: String, url: String, onLoaded: (Bitmap) -> Unit) {
        memory.get(key)?.let { onLoaded(it); return }
        synchronized(pending) { if (!pending.add(key)) return }
        val dir = File(context.cacheDir, "covers")
        pool.execute {
            val bitmap = runCatching { fromServer(dir, key, url) }.getOrNull()
            synchronized(pending) { pending.remove(key) }
            if (bitmap != null) {
                memory.put(key, bitmap)
                main.post { onLoaded(bitmap) }
            }
        }
    }

    /**
     * The art and the author inside the book file at [uri], read in the
     * background and handed to [onLoaded] on the main thread. Either may
     * be null. The art is remembered under [key].
     */
    fun loadEmbedded(context: Context, key: String, uri: Uri, onLoaded: (Bitmap?, String?) -> Unit) {
        val app = context.applicationContext
        pool.execute {
            var author: String? = null
            val bitmap = runCatching {
                val r = MediaMetadataRetriever()
                try {
                    r.setDataSource(app, uri)
                    author = r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_ARTIST)
                        ?: r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_ALBUMARTIST)
                    memory.get(key) ?: r.embeddedPicture?.let { decodeSmall(it) }
                } finally {
                    r.release()
                }
            }.getOrNull()
            bitmap?.let { memory.put(key, it) }
            main.post { onLoaded(bitmap, author) }
        }
    }

    private fun fromServer(dir: File, key: String, url: String): Bitmap? {
        dir.mkdirs()
        // The size asked for is in the address; a different size is a different file.
        val file = File(dir, key.replace(Regex("[^A-Za-z0-9_-]"), "_") + "-" + Integer.toHexString(url.hashCode()))
        if (!file.isFile) {
            val conn = URL(url).openConnection() as HttpURLConnection
            conn.connectTimeout = 10_000
            conn.readTimeout = 20_000
            try {
                if (conn.responseCode != 200) return null
                val bytes = conn.inputStream.use { it.readBytes() }
                val part = File(dir, "${file.name}.part")
                part.writeBytes(bytes)
                part.renameTo(file)
            } finally {
                conn.disconnect()
            }
        }
        return BitmapFactory.decodeFile(file.path)
    }

    /** Book art can be large; a few hundred pixels is plenty for a screen. */
    private fun decodeSmall(bytes: ByteArray): Bitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        var sample = 1
        while (bounds.outWidth / (sample * 2) >= 600) sample *= 2
        return BitmapFactory.decodeByteArray(bytes, 0, bytes.size, BitmapFactory.Options().apply { inSampleSize = sample })
    }
}
