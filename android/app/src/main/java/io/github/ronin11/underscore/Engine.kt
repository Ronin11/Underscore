package io.github.ronin11.underscore

/**
 * Thin Kotlin face of the Rust engine. One instance owns one native player.
 * Call [close] when done. All times are milliseconds on
 * [android.os.SystemClock.elapsedRealtime].
 */
class Engine(sampleRate: Int, val channels: Int) : AutoCloseable {
    private var handle: Long = nativeCreate(sampleRate, channels)

    private fun h(): Long = handle.also { check(it != 0L) { "engine closed" } }

    fun loadSpeech(fd: Int, mime: String?): Boolean = nativeLoadSpeech(h(), fd, mime)
    fun loadMusic(fd: Int, mime: String?): Boolean = nativeLoadMusic(h(), fd, mime)
    fun play(nowMs: Long) = nativePlay(h(), nowMs)
    fun pause(nowMs: Long) = nativePause(h(), nowMs)
    fun toggle(nowMs: Long) = nativeToggle(h(), nowMs)
    val isPlaying: Boolean get() = nativeIsPlaying(h())
    fun setSpeed(speed: Float): Float = nativeSetSpeed(h(), speed)
    val speed: Float get() = nativeSpeed(h())
    fun setMusicEnabled(enabled: Boolean) = nativeSetMusicEnabled(h(), enabled)
    val musicEnabled: Boolean get() = nativeMusicEnabled(h())
    fun setMusicLevel(level: Float) = nativeSetMusicLevel(h(), level)
    fun skipBack() = nativeSkipBack(h())
    fun skipForward() = nativeSkipForward(h())
    fun seekMs(toMs: Long) = nativeSeekMs(h(), toMs)
    fun setSleepMs(afterMs: Long, nowMs: Long) = nativeSetSleepMs(h(), afterMs, nowMs)
    fun extendSleepMs(byMs: Long, nowMs: Long) = nativeExtendSleepMs(h(), byMs, nowMs)
    fun sleepRemainingMs(nowMs: Long): Long = nativeSleepRemainingMs(h(), nowMs)
    fun setNoiseDb(db: Float) = nativeSetNoiseDb(h(), db)
    /** How far the skip buttons jump. */
    fun setSkipMs(backMs: Long, forwardMs: Long) = nativeSetSkipMs(h(), backMs, forwardMs)
    /** Rewind a little on resume, scaled by the length of the pause. */
    fun setRewind(enabled: Boolean) = nativeSetRewind(h(), enabled)
    val positionMs: Long get() = nativePositionMs(h())
    val durationMs: Long get() = nativeDurationMs(h())

    /** Chapter marks of the loaded book; zero when it has none. */
    val chapterCount: Int get() = nativeChapterCount(h())
    /** A chapter's title, or null when it has none. */
    fun chapterTitle(index: Int): String? = nativeChapterTitle(h(), index)
    /** Where a chapter starts, or -1 when out of range. */
    fun chapterStartMs(index: Int): Long = nativeChapterStartMs(h(), index)
    /** The chapter the book is in, or -1. */
    val currentChapter: Int get() = nativeCurrentChapter(h())
    /** Jump to the start of a chapter. */
    fun seekChapter(index: Int): Boolean = nativeSeekChapter(h(), index)
    /** Pause when the current chapter ends. */
    fun setSleepEndOfChapter(nowMs: Long) = nativeSetSleepEndOfChapter(h(), nowMs)
    val sleepAtChapterEnd: Boolean get() = nativeSleepAtChapterEnd(h())

    /**
     * Follow a music map, playing from [musicDir]. Scans the folder, so it
     * is slow: call it from a background thread. Returns null when the map
     * is loaded, otherwise what was wrong.
     */
    fun loadMap(toml: String, musicDir: String, cacheDir: String): String? =
        nativeLoadMap(h(), toml, musicDir, cacheDir)
    /**
     * Follow a music map that plays from the music server, which must be
     * signed in. Reads the server's whole song list, so it is slow: call it
     * from a background thread. Returns null when the map is loaded,
     * otherwise what was wrong.
     */
    fun loadMapFromServer(toml: String, cacheDir: String): String? =
        nativeLoadMapFromServer(h(), toml, cacheDir)
    /** Stop following the map and fade its music out. */
    fun clearMap() = nativeClearMap(h())
    val hasMap: Boolean get() = nativeHasMap(h())
    /** One line on the map for the UI. */
    val mapStatus: String get() = nativeMapStatus(h())

    /**
     * Audiobookshelf. All of these talk to the server, so call them from
     * a background thread. Errors come back as a message; null is success.
     */
    fun serverLogin(url: String, username: String, secret: String): String? =
        nativeServerLogin(h(), url, username, secret)
    fun serverResume(url: String, access: String, refresh: String?): String? =
        nativeServerResume(h(), url, access, refresh)
    /** Access token, newline, refresh token. Null without a server. */
    val serverCredentials: String? get() = nativeServerCredentials(h())
    fun serverForget() = nativeServerForget(h())
    val hasServer: Boolean get() = nativeHasServer(h())
    /** Begin a single sign-on. JSON: `{"url": ...}` to open in the browser, or `{"error": ...}`. */
    fun serverOpenIdStart(url: String): String = nativeServerOpenIdStart(h(), url)
    /** Finish it with the `code` and `state` the browser came back with. */
    fun serverOpenIdFinish(code: String, state: String): String? = nativeServerOpenIdFinish(h(), code, state)
    /** JSON: `{"base":...,"books":[{id,title,author,series,durationMs,progress,...}]}` or `{"error":...}`. */
    fun serverBooks(): String = nativeServerBooks(h())
    /** Download a book into [cacheDir] and load it. Slow. */
    fun serverOpen(id: String, cacheDir: String): String? = nativeServerOpen(h(), id, cacheDir)
    /** Report the position of the loaded server book. */
    fun serverSync(): Boolean = nativeServerSync(h())
    /**
     * The loaded server book's download: "received total" in bytes,
     * "error ..." when it stopped, or null when nothing is downloading.
     */
    val serverDownload: String? get() = nativeServerDownload(h())
    /** The cached file behind the loaded server book, or null while it downloads. */
    val serverBookFile: String? get() = nativeServerBookFile(h())
    /** Load a cached server book without asking the server. */
    fun serverReopen(id: String, path: String): Boolean = nativeServerReopen(h(), id, path)

    /**
     * Transcribe the book on the phone, keeping a lead on the listener.
     * [path] is the book's file, or empty when [fd] is an open descriptor,
     * which the engine takes over. Returns at once; the work runs on its
     * own low-priority thread.
     */
    fun transcribeStart(dir: String, model: String, key: String, path: String, fd: Int, lengthMs: Long) =
        nativeTranscribeStart(h(), dir, model, key, path, fd, lengthMs)
    /** Where the listener is. Cheap. */
    fun transcribePosition(atMs: Long) = nativeTranscribePosition(h(), atMs)
    /** JSON: phase, leadMs, speed, current line, nearby lines. */
    val transcribeStatus: String get() = nativeTranscribeStatus(h())
    fun transcribeStop() = nativeTranscribeStop(h())
    /** The file [name] beside server book [id], or null. Network I/O: not on the main thread. */
    fun serverCompanion(id: String, name: String): ByteArray? = nativeServerCompanion(h(), id, name)
    /** Take [data] as book [key]'s transcript under [dir]. Null, or why not. */
    fun transcribeImport(dir: String, key: String, data: ByteArray): String? = nativeTranscribeImport(h(), dir, key, data)

    /**
     * The music server (Navidrome or any Subsonic-compatible one). All of
     * these talk to the server, so call them from a background thread.
     * Errors come back as a message; null is success.
     */
    fun musicLogin(url: String, username: String, secret: String): String? =
        nativeMusicLogin(h(), url, username, secret)
    fun musicResume(url: String, user: String, salt: String, token: String): String? =
        nativeMusicResume(h(), url, user, salt, token)
    /** User, salt and token, one per line. Null without a music server. */
    val musicCredentials: String? get() = nativeMusicCredentials(h())
    fun musicForget() = nativeMusicForget(h())
    val hasMusicServer: Boolean get() = nativeHasMusicServer(h())

    /** Fill [out] with interleaved samples; returns frames written. */
    fun render(out: FloatArray, nowMs: Long): Int = nativeRender(h(), out, nowMs)

    override fun close() {
        if (handle != 0L) {
            nativeDestroy(handle)
            handle = 0L
        }
    }

    private external fun nativeCreate(sampleRate: Int, channels: Int): Long
    private external fun nativeDestroy(handle: Long)
    private external fun nativeLoadSpeech(handle: Long, fd: Int, mime: String?): Boolean
    private external fun nativeLoadMusic(handle: Long, fd: Int, mime: String?): Boolean
    private external fun nativePlay(handle: Long, nowMs: Long)
    private external fun nativePause(handle: Long, nowMs: Long)
    private external fun nativeToggle(handle: Long, nowMs: Long)
    private external fun nativeIsPlaying(handle: Long): Boolean
    private external fun nativeSetSpeed(handle: Long, speed: Float): Float
    private external fun nativeSpeed(handle: Long): Float
    private external fun nativeSetMusicEnabled(handle: Long, enabled: Boolean)
    private external fun nativeMusicEnabled(handle: Long): Boolean
    private external fun nativeSetMusicLevel(handle: Long, level: Float)
    private external fun nativeSkipBack(handle: Long)
    private external fun nativeSkipForward(handle: Long)
    private external fun nativeSeekMs(handle: Long, toMs: Long)
    private external fun nativeSetSleepMs(handle: Long, afterMs: Long, nowMs: Long)
    private external fun nativeExtendSleepMs(handle: Long, byMs: Long, nowMs: Long)
    private external fun nativeSleepRemainingMs(handle: Long, nowMs: Long): Long
    private external fun nativeSetNoiseDb(handle: Long, db: Float)
    private external fun nativeSetSkipMs(handle: Long, backMs: Long, forwardMs: Long)
    private external fun nativeSetRewind(handle: Long, enabled: Boolean)
    private external fun nativePositionMs(handle: Long): Long
    private external fun nativeDurationMs(handle: Long): Long
    private external fun nativeChapterCount(handle: Long): Int
    private external fun nativeChapterTitle(handle: Long, index: Int): String?
    private external fun nativeChapterStartMs(handle: Long, index: Int): Long
    private external fun nativeCurrentChapter(handle: Long): Int
    private external fun nativeSeekChapter(handle: Long, index: Int): Boolean
    private external fun nativeSetSleepEndOfChapter(handle: Long, nowMs: Long)
    private external fun nativeSleepAtChapterEnd(handle: Long): Boolean
    private external fun nativeLoadMap(handle: Long, toml: String, musicDir: String, cacheDir: String): String?
    private external fun nativeLoadMapFromServer(handle: Long, toml: String, cacheDir: String): String?
    private external fun nativeClearMap(handle: Long)
    private external fun nativeMusicLogin(handle: Long, url: String, username: String, secret: String): String?
    private external fun nativeMusicResume(handle: Long, url: String, user: String, salt: String, token: String): String?
    private external fun nativeMusicCredentials(handle: Long): String?
    private external fun nativeMusicForget(handle: Long)
    private external fun nativeHasMusicServer(handle: Long): Boolean
    private external fun nativeHasMap(handle: Long): Boolean
    private external fun nativeMapStatus(handle: Long): String
    private external fun nativeServerLogin(handle: Long, url: String, username: String, secret: String): String?
    private external fun nativeServerResume(handle: Long, url: String, access: String, refresh: String?): String?
    private external fun nativeServerCredentials(handle: Long): String?
    private external fun nativeServerForget(handle: Long)
    private external fun nativeHasServer(handle: Long): Boolean
    private external fun nativeServerOpenIdStart(handle: Long, url: String): String
    private external fun nativeServerOpenIdFinish(handle: Long, code: String, state: String): String?
    private external fun nativeServerBooks(handle: Long): String
    private external fun nativeServerOpen(handle: Long, id: String, cacheDir: String): String?
    private external fun nativeServerBookFile(handle: Long): String?
    private external fun nativeServerDownload(handle: Long): String?
    private external fun nativeServerReopen(handle: Long, id: String, path: String): Boolean
    private external fun nativeTranscribeStart(handle: Long, dir: String, model: String, key: String, path: String, fd: Int, lengthMs: Long)
    private external fun nativeTranscribePosition(handle: Long, atMs: Long)
    private external fun nativeTranscribeStatus(handle: Long): String
    private external fun nativeTranscribeStop(handle: Long)
    private external fun nativeServerSync(handle: Long): Boolean
    private external fun nativeServerCompanion(handle: Long, id: String, name: String): ByteArray?
    private external fun nativeTranscribeImport(handle: Long, dir: String, key: String, data: ByteArray): String?
    private external fun nativeRender(handle: Long, out: FloatArray, nowMs: Long): Int

    companion object {
        init {
            System.loadLibrary("underscore_android")
        }
    }
}
