package io.github.ronin11.underscore

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.net.Uri
import android.os.Binder
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
import android.support.v4.media.MediaMetadataCompat
import android.support.v4.media.session.MediaSessionCompat
import android.support.v4.media.session.PlaybackStateCompat
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import androidx.media.app.NotificationCompat.MediaStyle
import java.io.File
import kotlin.math.abs

/**
 * Owns the engine and the audio device. Runs in the foreground so playback
 * survives the screen turning off. The activity binds to it for control.
 *
 * The render thread asks the engine for one block at a time and writes it
 * straight to the [AudioTrack]. Nothing is written anywhere else.
 *
 * Transport goes through [play], [pause], [toggle], [skipBack],
 * [skipForward] and [seekMs] here rather than the engine directly, so the
 * lock screen, the notification, headset buttons, and audio focus all see
 * the same state. The media session reports the book, not the music: that
 * is what the listener is listening to.
 */
class PlaybackService : Service() {

    inner class LocalBinder : Binder() {
        val service: PlaybackService get() = this@PlaybackService
    }

    private val binder = LocalBinder()
    lateinit var engine: Engine
        private set
    private var track: AudioTrack? = null
    private var renderThread: Thread? = null
    @Volatile private var running = false

    private lateinit var session: MediaSessionCompat
    private lateinit var audioManager: AudioManager
    private var focusRequest: AudioFocusRequest? = null
    /** Playback was paused by a transient focus loss and should resume. */
    private var resumeOnFocusGain = false
    /** What the last focus request and focus change said, for bug reports. */
    private var lastFocusResult: String = "never requested"
    private var lastFocusChange: String = "none"
    private val mainHandler = Handler(Looper.getMainLooper())

    var speechName: String? = null
        private set
    /** True when the loaded book came from the Audiobookshelf server. */
    var bookFromServer = false
        private set
    /** The open book's author and series, when known. */
    var speechAuthor: String? = null
        private set
    var speechSeries: String? = null
        private set
    /** The open book's cover, once loaded. */
    var speechCover: android.graphics.Bitmap? = null
        private set
    var musicName: String? = null
        private set
    var mapName: String? = null
        private set
    /** The listener's settings; applied to the engine here, edited by the settings screen. */
    lateinit var settings: Settings
        private set
    private var settingsListener: android.content.SharedPreferences.OnSharedPreferenceChangeListener? = null
    /** The folder a map plays from, from the settings, when it still exists. */
    val musicDir: File?
        get() = settings.musicDir?.let { File(it) }?.takeIf { it.isDirectory }
    /** True while a map is being loaded and its folder scanned. */
    @Volatile var mapLoading = false
        private set
    /** The Audiobookshelf address in use, or the last one tried. */
    var serverUrl: String? = null
        private set
    /** The music server address in use, or the last one tried. */
    var musicServerUrl: String? = null
        private set
    /** What the music server thread is doing right now, for the UI; null when idle. */
    @Volatile var musicServerBusy: String? = null
        private set
    /** What the server thread is doing right now, for the UI; null when idle. */
    @Volatile var serverBusy: String? = null
        private set
    private var lastSyncMs = 0L
    @Volatile private var syncing = false
    private val prefs by lazy { getSharedPreferences("server", Context.MODE_PRIVATE) }
    private val musicPrefs by lazy { getSharedPreferences("music_server", Context.MODE_PRIVATE) }
    /** The last book and map, and where the listener was, for the next start. */
    private val last by lazy { getSharedPreferences("last", Context.MODE_PRIVATE) }
    /** Which cached file is which server book, by file name, for the storage screen. */
    private val downloadNames by lazy { getSharedPreferences("downloads", Context.MODE_PRIVATE) }
    private val lastMapFile by lazy { File(filesDir, "last-map.toml") }
    /** Transcribing on the phone: on or off, and which model. */
    private val transcribePrefs by lazy { getSharedPreferences("transcribe", Context.MODE_PRIVATE) }
    val transcribing: Boolean get() = transcribePrefs.getBoolean("on", false)
    val transcribeModel: String get() = transcribePrefs.getString("model", null) ?: "base.en"
    /** Where server books and map music are cached. */
    val libraryCache: File get() = File(cacheDir, "library")
    /** Where book covers are cached. */
    val coverCache: File get() = File(cacheDir, "covers")
    /** The listener picked a book since this start, so the last one stays closed. */
    @Volatile private var bookChosen = false
    /** The open server book is still downloading; finish up when it is whole. */
    private var downloading = false
    private var downloadFailed = false
    private var lastSaveMs = 0L

    /**
     * One book on the server, as the book list shows it. [progress] runs
     * 0 to 1 and is negative when the book was never started; [files] is
     * 0 when the server did not say. Times are milliseconds since 1970.
     */
    data class ServerBook(
        val id: String,
        val title: String,
        val author: String,
        val series: String,
        val durationMs: Long,
        val addedMs: Long,
        val files: Int,
        val coverUrl: String?,
        val progress: Double,
        val finished: Boolean,
        val playedMs: Long,
        val open: Boolean,
    )

    /** The last book list, so the book screen opens on something. */
    @Volatile var serverBookList: List<ServerBook>? = null
        private set

    // What the session and notification last showed, to publish only on change.
    private var shownPlaying = false
    private var shownPositionMs = 0L
    private var shownAtMs = 0L
    private var shownSpeed = 0f
    private var shownTitle: String? = null
    private var shownChapter: String? = null
    private var shownDurationMs = -1L
    private var shownCover: android.graphics.Bitmap? = null

    private val noisyReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            // Headphones unplugged: do not read the book out of the speaker.
            if (engine.isPlaying) pause()
        }
    }

    private val focusListener = AudioManager.OnAudioFocusChangeListener { change ->
        lastFocusChange = "$change at ${now()}"
        when (change) {
            AudioManager.AUDIOFOCUS_LOSS -> {
                resumeOnFocusGain = false
                if (engine.isPlaying) pause()
            }
            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT,
            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> {
                // Spoken word is not worth ducking; pause and come back.
                resumeOnFocusGain = engine.isPlaying
                if (resumeOnFocusGain) {
                    engine.pause(now())
                    publishState()
                }
            }
            AudioManager.AUDIOFOCUS_GAIN -> {
                if (resumeOnFocusGain) {
                    resumeOnFocusGain = false
                    engine.play(now())
                    publishState()
                }
            }
        }
    }

    /** Once a second: catch state the engine changed on its own, such as a sleep timer. */
    private val stateTick = object : Runnable {
        override fun run() {
            publishStateIfChanged()
            if (engine.isPlaying && now() - lastSyncMs > SYNC_EVERY_MS) syncServer()
            if (engine.isPlaying && now() - lastSaveMs > SAVE_EVERY_MS) savePosition()
            if (transcribing) engine.transcribePosition(engine.positionMs)
            if (downloading) checkDownload()
            mainHandler.postDelayed(this, 1000)
        }
    }

    override fun onCreate() {
        super.onCreate()
        engine = Engine(SAMPLE_RATE, CHANNELS)
        settings = Settings(this)
        settings.applyTo(engine)
        settingsListener = settings.listen { settings.applyTo(engine) }
        audioManager = getSystemService(Context.AUDIO_SERVICE) as AudioManager
        createSession()
        createChannel()
        startForeground()
        startAudio()
        ContextCompat.registerReceiver(
            this, noisyReceiver, IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY),
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        // Give the session a state and metadata before anything is loaded, so
        // the system's media controls have something to show from the start.
        publishState()
        mainHandler.post(stateTick)
        restore()
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_PLAY -> play()
            ACTION_PAUSE -> pause()
            ACTION_TOGGLE -> toggle()
            ACTION_BACK -> skipBack()
            ACTION_FORWARD -> skipForward()
            ACTION_OPENID_RETURN -> intent.data?.let { serverOpenIdFinish(it) }
        }
        return START_STICKY
    }

    override fun onDestroy() {
        savePosition()
        mainHandler.removeCallbacks(stateTick)
        settingsListener?.let { settings.unlisten(it) }
        unregisterReceiver(noisyReceiver)
        abandonFocus()
        session.isActive = false
        session.release()
        running = false
        renderThread?.join(1000)
        track?.stop()
        track?.release()
        engine.close()
        super.onDestroy()
    }

    fun now(): Long = SystemClock.elapsedRealtime()

    /** Start or resume. Does nothing if another app refuses to give up audio focus. */
    fun play() {
        if (!requestFocus()) return
        engine.play(now())
        session.isActive = true
        publishState()
    }

    fun pause() {
        engine.pause(now())
        resumeOnFocusGain = false
        publishState()
        syncServer()
        savePosition()
    }

    fun toggle() {
        if (engine.isPlaying) pause() else play()
    }

    fun skipBack() {
        engine.skipBack()
        publishState()
    }

    fun skipForward() {
        engine.skipForward()
        publishState()
    }

    fun seekMs(toMs: Long) {
        engine.seekMs(toMs)
        publishState()
    }

    /** Where chapter [index] ends: the next chapter's start, or the end of the book. */
    fun chapterEndMs(index: Int): Long =
        if (index + 1 < engine.chapterCount) engine.chapterStartMs(index + 1) else engine.durationMs

    /**
     * Back to the start of this chapter, or to the previous one when the
     * book is already within a few seconds of the start. The same rule as
     * a track button on a CD player.
     */
    fun previousChapter() {
        val chapter = engine.currentChapter
        if (chapter < 0) return
        val intoChapter = engine.positionMs - engine.chapterStartMs(chapter)
        val target = if (intoChapter > 3000 || chapter == 0) chapter else chapter - 1
        engine.seekChapter(target)
        publishState()
    }

    fun nextChapter() {
        val chapter = engine.currentChapter
        if (chapter < 0 || chapter + 1 >= engine.chapterCount) return
        engine.seekChapter(chapter + 1)
        publishState()
    }

    /** Author, series and cover for a server book; the cover loads in the background. */
    private fun showServerDetails(id: String, author: String?, series: String?, coverUrl: String?) {
        speechAuthor = author
        speechSeries = series
        speechCover = Covers.cached(id)
        coverUrl?.let { url ->
            Covers.load(this, id, url) { bitmap ->
                if (last.getString("id", null) == id) {
                    speechCover = bitmap
                    publishState()
                }
            }
        }
    }

    /** The author and art inside a book file, read in the background. */
    private fun showFileDetails(uri: Uri) {
        speechAuthor = null
        speechSeries = null
        speechCover = null
        val key = "file:$uri"
        Covers.loadEmbedded(this, key, uri) { bitmap, author ->
            if (last.getString("key", null) == key) {
                speechCover = bitmap
                speechAuthor = author
                publishState()
            }
        }
    }

    /** Open [uri] read-only and hand the descriptor to the engine. */
    fun loadSpeech(uri: Uri, name: String?): Boolean {
        bookChosen = true
        // Keep the right to read it after a restart, to open it again then.
        runCatching { contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION) }
        val ok = openSpeech(uri, name)
        if (ok) {
            rememberBook("file:$uri") {
                putString("kind", "file").putString("uri", uri.toString()).putString("name", name)
            }
            showFileDetails(uri)
            followWithTranscriber()
        }
        return ok
    }

    private fun openSpeech(uri: Uri, name: String?): Boolean {
        val ok = try {
            load(uri) { fd, mime -> engine.loadSpeech(fd, mime) }
        } catch (e: Exception) {
            false
        }
        if (ok) {
            speechName = name
            bookFromServer = false
        }
        publishState()
        return ok
    }

    fun loadMusic(uri: Uri, name: String?): Boolean {
        val ok = load(uri) { fd, mime -> engine.loadMusic(fd, mime) }
        if (ok) musicName = name
        return ok
    }

    /**
     * Follow a music map. The engine scans [musicDir] first, which takes a
     * while on a big folder, so this runs on its own thread and reports
     * through [onDone] on the main thread: null when the map is loaded,
     * otherwise what was wrong.
     */
    fun loadMap(toml: String, name: String?, onDone: (String?) -> Unit) {
        val dir = musicDir
        val fromServer = engine.hasMusicServer
        if (!fromServer && dir == null) {
            onDone("Sign in to the music server or set the music folder in Settings first")
            return
        }
        val cache = File(cacheDir, "library")
        mapLoading = true
        Thread({
            // The music server wins when both are set.
            val problem = when {
                fromServer -> engine.loadMapFromServer(toml, cache.absolutePath)
                dir != null -> engine.loadMap(toml, dir.absolutePath, cache.absolutePath)
                else -> "Set the music folder in Settings first"
            }
            if (problem == null) {
                mapName = name
                runCatching { lastMapFile.writeText(toml) }
                last.edit().putString("map", name ?: "map").apply()
            }
            mapLoading = false
            mainHandler.post { onDone(problem) }
        }, "underscore-map-load").start()
    }

    fun clearMap() {
        engine.clearMap()
        mapName = null
        last.edit().remove("map").apply()
        lastMapFile.delete()
    }

    // The music server. Same shape as Audiobookshelf below, but what is
    // kept is the user name and a salted token, never the password.

    /** Sign in to the music server; [onDone] gets null or what went wrong. */
    fun musicServerLogin(url: String, username: String, secret: String, onDone: (String?) -> Unit) {
        val base = url.trim().trimEnd('/')
        musicServerUrl = base
        musicServerBusy = "Signing in to $base…"
        Thread({
            val problem = engine.musicLogin(base, username, secret)
            if (problem == null) saveMusicServer(base)
            musicServerBusy = null
            mainHandler.post { onDone(problem) }
        }, "underscore-music-server").start()
    }

    /** Sign out and forget the saved login. */
    fun musicServerForget() {
        engine.musicForget()
        musicPrefs.edit().clear().apply()
    }

    /** Come back to the music server from the last run, if there was one. Blocks: network. */
    private fun resumeMusicServer() {
        val base = musicPrefs.getString("url", null) ?: return
        val user = musicPrefs.getString("user", null) ?: return
        val salt = musicPrefs.getString("salt", null) ?: return
        val token = musicPrefs.getString("token", null) ?: return
        musicServerUrl = base
        musicServerBusy = "Reconnecting to $base…"
        val problem = engine.musicResume(base, user, salt, token)
        musicServerBusy = null
        if (problem != null) android.util.Log.w("underscore", "music server: $problem")
    }

    private fun saveMusicServer(base: String) {
        val lines = (engine.musicCredentials ?: return).split('\n')
        if (lines.size < 3) return
        musicPrefs.edit()
            .putString("url", base)
            .putString("user", lines[0])
            .putString("salt", lines[1])
            .putString("token", lines[2])
            .apply()
    }

    // Audiobookshelf. Every call to the engine's server functions does
    // network I/O, so each runs on its own short-lived thread and reports
    // back on the main thread.

    /** Sign in; [onDone] gets null or what went wrong. */
    fun serverLogin(url: String, username: String, secret: String, onDone: (String?) -> Unit) {
        val base = url.trim().trimEnd('/')
        serverUrl = base
        serverBusy = "Signing in to $base…"
        Thread({
            val problem = engine.serverLogin(base, username, secret)
            if (problem == null) saveServer(base)
            serverBusy = null
            mainHandler.post { onDone(problem) }
        }, "underscore-server").start()
    }

    /**
     * Begin a single sign-on (Authentik or another OpenID provider behind
     * Audiobookshelf). [onDone] gets the address to open in the browser, or
     * null and what went wrong. The browser comes back through
     * [SignInReturnActivity], which hands the address to [serverOpenIdFinish].
     */
    fun serverOpenIdStart(url: String, onDone: (String?, String?) -> Unit) {
        val base = url.trim().trimEnd('/')
        serverUrl = base
        serverBusy = "Starting single sign-on with $base…"
        Thread({
            val json = engine.serverOpenIdStart(base)
            serverBusy = null
            val o = try { org.json.JSONObject(json) } catch (e: org.json.JSONException) { null }
            val authorize = o?.optString("url")?.takeIf { it.isNotEmpty() }
            val problem = if (authorize == null) o?.optString("error") ?: "bad answer from the engine" else null
            mainHandler.post { onDone(authorize, problem) }
        }, "underscore-server").start()
    }

    /** The browser came back from the single sign-on with [uri]. */
    private fun serverOpenIdFinish(uri: Uri) {
        val code = uri.getQueryParameter("code")
        val state = uri.getQueryParameter("state")
        if (code == null || state == null) {
            val why = uri.getQueryParameter("error_description") ?: uri.getQueryParameter("error") ?: "no code"
            toast("Sign-in did not finish: $why")
            return
        }
        val base = serverUrl
        serverBusy = "Finishing sign-in…"
        Thread({
            val problem = engine.serverOpenIdFinish(code, state)
            if (problem == null && base != null) saveServer(base)
            serverBusy = null
            mainHandler.post { toast(problem ?: "Signed in to Audiobookshelf") }
        }, "underscore-server").start()
    }

    private fun toast(text: String) {
        android.widget.Toast.makeText(applicationContext, text, android.widget.Toast.LENGTH_LONG).show()
    }

    /** Sign out and forget the saved tokens. */
    fun serverForget() {
        engine.serverForget()
        serverBookList = null
        prefs.edit().clear().apply()
    }

    /** Come back to the server from the last run, if there was one. Blocks: network. */
    private fun resumeServer() {
        val base = prefs.getString("url", null) ?: return
        val access = prefs.getString("access", null) ?: return
        val refresh = prefs.getString("refresh", null)
        serverUrl = base
        serverBusy = "Reconnecting to $base…"
        val problem = engine.serverResume(base, access, refresh)
        if (problem == null) saveServer(base)
        serverBusy = null
        if (problem != null) android.util.Log.w("underscore", "server: $problem")
    }

    /**
     * Pick up where the last run left off: sign back in to both servers,
     * then open the last book at the last position and follow the last
     * map. A server book comes from the cache, so this needs no download;
     * without the server it opens the cached file as it is. Runs on its
     * own thread because all of it can wait on the network.
     */
    private fun restore() {
        Thread({
            resumeServer()
            resumeMusicServer()
            val kind = last.getString("kind", null)
            val name = last.getString("name", null)
            val at = last.getLong("positionMs", -1)
            when (kind) {
                "server" -> restoreServerBook(last.getString("id", null), last.getString("file", null), name, at)
                "file" -> last.getString("uri", null)?.let { uri ->
                    mainHandler.post {
                        if (!bookChosen && openSpeech(Uri.parse(uri), name)) {
                            if (at > 0) seekMs(at)
                            showFileDetails(Uri.parse(uri))
                            followWithTranscriber()
                        }
                    }
                }
            }
            mainHandler.post { restoreMap() }
        }, "underscore-restore").start()
    }

    /** On the restore thread. */
    private fun restoreServerBook(id: String?, file: String?, name: String?, at: Long) {
        if (id == null || bookChosen) return
        serverBusy = "Reopening ${name ?: "the last book"}…"
        // The server knows the latest position, from any app; ask it first.
        var ok = engine.hasServer && engine.serverOpen(id, libraryCache.absolutePath) == null
        // The server had no position for it: use ours.
        if (ok && at > 0 && engine.positionMs < 1000) engine.seekMs(at)
        if (!ok && file != null && file.lines().all { File(it).isFile }) {
            ok = engine.serverReopen(id, file)
            if (ok && at > 0) engine.seekMs(at)
        }
        serverBusy = null
        mainHandler.post {
            if (ok && !bookChosen) {
                speechName = name
                bookFromServer = true
                showServerDetails(id, last.getString("author", null), last.getString("series", null), last.getString("cover", null))
                publishState()
                // A book whose download was cut short streams again.
                downloading = engine.serverDownload != null && engine.serverBookFile == null
                if (!downloading) followWithTranscriber()
            }
        }
    }

    private fun restoreMap() {
        val name = last.getString("map", null) ?: return
        if (mapName != null || mapLoading) return
        val text = runCatching { lastMapFile.readText() }.getOrNull() ?: return
        loadMap(text, name) { problem ->
            if (problem != null) toast("Could not reload the map $name: $problem")
        }
    }

    /**
     * Turn transcribing on the phone on or off, with [model]. On, it
     * follows the open book now and every book opened after.
     */
    fun setTranscribing(on: Boolean, model: String = transcribeModel) {
        transcribePrefs.edit().putBoolean("on", on).putString("model", model).apply()
        if (on) followWithTranscriber() else engine.transcribeStop()
    }

    /** What the transcriber is doing, or null when it is off. */
    fun transcribeStatus(): org.json.JSONObject? =
        if (!transcribing) null else runCatching { org.json.JSONObject(engine.transcribeStatus) }.getOrNull()

    /**
     * Point the transcriber at the open book. It gets a file of its own to
     * read: the cached file for a server book, a second descriptor for a
     * book from a file. Its transcript is kept per book, so coming back to
     * a book carries on where it stopped.
     */
    private fun followWithTranscriber() {
        if (!transcribing || speechName == null) return
        val dir = File(filesDir, "whisper").absolutePath
        val length = engine.durationMs
        engine.transcribePosition(engine.positionMs)
        when (last.getString("kind", null)) {
            "server" -> {
                val id = last.getString("id", null) ?: return
                // A book split across files is not transcribed yet: the
                // transcriber reads one file.
                val file = engine.serverBookFile?.takeIf { '\n' !in it } ?: return
                engine.transcribeStart(dir, transcribeModel, "abs:$id", file, -1, length)
            }
            "file" -> {
                val uri = last.getString("uri", null) ?: return
                val pfd = runCatching { contentResolver.openFileDescriptor(Uri.parse(uri), "r") }.getOrNull() ?: return
                engine.transcribeStart(dir, transcribeModel, "file:$uri", "", pfd.detachFd(), length)
            }
        }
    }

    /** Remember where the listener is in the current book. */
    private fun savePosition() {
        lastSaveMs = now()
        if (speechName == null || last.getString("kind", null) == null) return
        last.edit().putLong("positionMs", engine.positionMs).apply()
    }

    /**
     * Keep [key]'s book as the one to reopen. A map is written for one
     * book, so a different book lets the map go; the same book again
     * keeps it.
     */
    private fun rememberBook(key: String, fields: android.content.SharedPreferences.Editor.() -> Unit) {
        val same = last.getString("key", null) == key
        val map = last.getString("map", null)
        last.edit().clear().apply {
            fields()
            putString("key", key)
            if (same && map != null) putString("map", map)
        }.commit()
        if (!same && (mapName != null || engine.hasMap)) clearMap()
    }

    /** Keep the tokens, which change when the access token is renewed. */
    private fun saveServer(base: String) {
        val creds = engine.serverCredentials ?: return
        val access = creds.substringBefore('\n')
        val refresh = creds.substringAfter('\n', "")
        prefs.edit()
            .putString("url", base)
            .putString("access", access)
            .putString("refresh", refresh.ifEmpty { null })
            .apply()
    }

    /** The server's books; [onDone] gets the list, or null and a reason. */
    fun serverBooks(onDone: (List<ServerBook>?, String?) -> Unit) {
        serverBusy = "Listing books…"
        Thread({
            val json = engine.serverBooks()
            serverBusy = null
            val books = ArrayList<ServerBook>()
            var problem: String? = null
            try {
                val o = org.json.JSONObject(json)
                if (o.has("error")) {
                    problem = o.getString("error")
                } else {
                    val base = o.optString("base")
                    val arr = o.getJSONArray("books")
                    for (i in 0 until arr.length()) {
                        val b = arr.getJSONObject(i)
                        val id = b.getString("id")
                        books.add(
                            ServerBook(
                                id = id,
                                title = b.getString("title"),
                                author = b.optString("author"),
                                series = b.optString("series"),
                                durationMs = b.optLong("durationMs", -1),
                                addedMs = b.optLong("addedMs"),
                                files = b.optInt("files"),
                                coverUrl = if (b.optBoolean("cover") && base.isNotEmpty()) "$base/api/items/$id/cover?width=480" else null,
                                progress = b.optDouble("progress", -1.0),
                                finished = b.optBoolean("finished"),
                                playedMs = b.optLong("playedMs"),
                                open = b.optBoolean("open"),
                            ),
                        )
                    }
                    serverBookList = books
                }
            } catch (e: Exception) {
                problem = "unreadable answer from the server: $e"
            }
            mainHandler.post { if (problem == null) onDone(books, null) else onDone(null, problem) }
        }, "underscore-server").start()
    }

    /** Download a server book into the cache and load it. [onDone] gets null or a reason. */
    fun openServerBook(book: ServerBook, onDone: (String?) -> Unit) {
        bookChosen = true
        serverBusy = "Opening ${book.title}…"
        Thread({
            val problem = engine.serverOpen(book.id, libraryCache.absolutePath)
            val file = if (problem == null) engine.serverBookFile else null
            serverBusy = null
            mainHandler.post {
                if (problem == null) {
                    speechName = book.title
                    bookFromServer = true
                    rememberBook("abs:${book.id}") {
                        putString("kind", "server").putString("id", book.id)
                            .putString("file", file).putString("name", book.title)
                            .putString("author", book.author.ifEmpty { null })
                            .putString("series", book.series.ifEmpty { null })
                            .putString("cover", book.coverUrl)
                    }
                    showServerDetails(book.id, book.author.ifEmpty { null }, book.series.ifEmpty { null }, book.coverUrl)
                    downloadFailed = false
                    downloading = file == null
                    if (file != null) {
                        followWithTranscriber()
                        nameDownloads(file, book.title)
                    }
                    publishState()
                }
                onDone(problem)
            }
        }, "underscore-server").start()
    }

    /**
     * How the open book's download is going, for the player's app bar:
     * "Downloading 34%", or null when nothing is downloading.
     */
    fun downloadLine(): String? {
        if (!downloading) return null
        val d = engine.serverDownload ?: return null
        if (d.startsWith("error")) return "Download stopped"
        val (got, total) = d.split(' ').map { it.toLongOrNull() ?: 0L }.let { it[0] to it.getOrElse(1) { 0L } }
        if (total <= 0) return null
        return "Downloading ${got * 100 / total}%"
    }

    /** Once the streamed book is whole: note its file and let the transcriber at it. */
    private fun checkDownload() {
        val d = engine.serverDownload
        if (d != null && d.startsWith("error")) {
            if (!downloadFailed) toast("The download stopped: ${d.removePrefix("error ")}")
            downloadFailed = true
            return
        }
        val file = engine.serverBookFile ?: return
        downloading = false
        last.edit().putString("file", file).apply()
        speechName?.let { nameDownloads(file, it) }
        followWithTranscriber()
    }

    /** Note which book [files], a path a line, belong to, for the storage screen. */
    private fun nameDownloads(files: String, title: String) {
        val edit = downloadNames.edit()
        files.lines().forEach { edit.putString(File(it).name, title) }
        edit.apply()
    }

    /** The open server book's files, by name: kept when downloads are removed. */
    private fun openFiles(): Set<String> =
        engine.serverBookFile?.lines()?.map { File(it).name }?.toSet() ?: emptySet()

    /** One file in the download cache. [title] is null for map music. */
    data class Download(val file: File, val title: String?, val bytes: Long, val open: Boolean)

    /** What the download cache holds. Reads the folder: call it off the main thread. */
    fun downloads(): List<Download> {
        val open = openFiles()
        return (libraryCache.listFiles() ?: emptyArray())
            .filter { it.isFile && !it.name.endsWith(".part") }
            .map { Download(it, downloadNames.getString(it.name, null), it.length(), it.name in open) }
    }

    /** Bytes the cached covers take. Reads the folder: call it off the main thread. */
    fun coverBytes(): Long = (coverCache.listFiles() ?: emptyArray()).sumOf { it.length() }

    /**
     * Delete cached files. The open book is kept, since it is playing
     * from that file. Music the map needs again downloads again.
     */
    fun removeDownloads(files: List<File>, covers: Boolean = false): Int {
        val open = openFiles()
        var n = 0
        for (f in files) {
            if (f.name in open) continue
            if (f.delete()) {
                n++
                downloadNames.edit().remove(f.name).apply()
            }
        }
        if (covers) coverCache.listFiles()?.forEach { it.delete() }
        return n
    }

    /** Tell the server where the book is. Cheap when there is no server book. */
    private fun syncServer() {
        if (syncing || !engine.hasServer) return
        syncing = true
        lastSyncMs = now()
        Thread({
            if (engine.serverSync()) serverUrl?.let { saveServer(it) }
            syncing = false
        }, "underscore-sync").start()
    }

    private fun load(uri: Uri, into: (Int, String?) -> Boolean): Boolean {
        val pfd = contentResolver.openFileDescriptor(uri, "r") ?: return false
        val mime = contentResolver.getType(uri)
        // The engine takes ownership of the descriptor from here.
        val fd = pfd.detachFd()
        return into(fd, mime)
    }

    private fun audioAttributes() = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_MEDIA)
        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
        .build()

    private fun requestFocus(): Boolean {
        val request = focusRequest ?: AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(audioAttributes())
            .setWillPauseWhenDucked(true)
            .setOnAudioFocusChangeListener(focusListener, mainHandler)
            .build()
            .also { focusRequest = it }
        val result = audioManager.requestAudioFocus(request)
        lastFocusResult = "$result at ${now()}"
        return result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
    }

    /** The service's own view of things, for a bug report. */
    fun describe(): String {
        val state = session.controller.playbackState
        val meta = session.controller.metadata
        val t = track
        return buildString {
            appendLine("engine: playing ${engine.isPlaying}, position ${engine.positionMs} ms of ${engine.durationMs} ms, speed ${engine.speed}")
            appendLine("engine: music ${if (engine.musicEnabled) "on" else "off"}, chapters ${engine.chapterCount}, current ${engine.currentChapter}")
            appendLine("book: ${speechName ?: "none"}; music: ${musicName ?: "none"}")
            appendLine("music folder: ${musicDir?.absolutePath ?: "none"}; map: ${mapName ?: "none"}, loading $mapLoading")
            appendLine("settings: skip -${settings.skipBackSec}/+${settings.skipForwardSec} s, rewind ${settings.rewindOnResume}, music level ${settings.musicLevel}, keep screen on ${settings.keepScreenOn}")
            appendLine("map status: ${engine.mapStatus}")
            appendLine("music server: ${musicServerUrl ?: "none"}, signed in ${engine.hasMusicServer}, busy ${musicServerBusy ?: "no"}")
            appendLine("server: ${serverUrl ?: "none"}, signed in ${engine.hasServer}, busy ${serverBusy ?: "no"}")
            appendLine("session: active ${session.isActive}")
            if (state == null) {
                appendLine("playback state: none")
            } else {
                appendLine("playback state: ${state.state}, position ${state.position}, speed ${state.playbackSpeed}, actions 0x${java.lang.Long.toHexString(state.actions)}")
            }
            if (meta == null) {
                appendLine("metadata: none")
            } else {
                appendLine(
                    "metadata: title ${meta.getString(MediaMetadataCompat.METADATA_KEY_TITLE)}, " +
                        "artist ${meta.getString(MediaMetadataCompat.METADATA_KEY_ARTIST)}, " +
                        "duration ${meta.getLong(MediaMetadataCompat.METADATA_KEY_DURATION)}",
                )
            }
            appendLine("last published: playing $shownPlaying, title $shownTitle, chapter $shownChapter, duration $shownDurationMs")
            appendLine("audio focus: request $lastFocusResult; change $lastFocusChange; resume on gain $resumeOnFocusGain")
            appendLine("track: ${if (t == null) "none" else "state ${t.playState}, rate ${t.sampleRate}, underruns ${t.underrunCount}"}")
            append("render thread: ${renderThread?.isAlive ?: false}, running $running")
        }
    }

    private fun abandonFocus() {
        focusRequest?.let { audioManager.abandonAudioFocusRequest(it) }
    }

    private fun startAudio() {
        val minBytes = AudioTrack.getMinBufferSize(
            SAMPLE_RATE, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_FLOAT,
        )
        val blockBytes = BLOCK_FRAMES * CHANNELS * 4
        val t = AudioTrack.Builder()
            .setAudioAttributes(audioAttributes())
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
                    .setSampleRate(SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                    .build(),
            )
            .setBufferSizeInBytes(maxOf(minBytes, blockBytes * 4))
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
        track = t
        t.play()
        running = true
        renderThread = Thread({
            android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_AUDIO)
            val buf = FloatArray(BLOCK_FRAMES * CHANNELS)
            while (running) {
                val frames = engine.render(buf, now())
                val samples = if (frames > 0) frames * CHANNELS else buf.size
                if (frames <= 0) buf.fill(0f)
                t.write(buf, 0, samples, AudioTrack.WRITE_BLOCKING)
            }
        }, "underscore-render").also { it.start() }
    }

    private fun createSession() {
        session = MediaSessionCompat(this, "underscore").apply {
            setCallback(object : MediaSessionCompat.Callback() {
                override fun onPlay() = play()
                override fun onPause() = pause()
                override fun onStop() = pause()
                override fun onSkipToNext() = skipForward()
                override fun onSkipToPrevious() = skipBack()
                override fun onFastForward() = skipForward()
                override fun onRewind() = skipBack()
                override fun onSeekTo(pos: Long) = seekMs(pos)
            })
            setSessionActivity(openActivity())
            isActive = true
        }
    }

    private fun chapterLine(): String? {
        val count = engine.chapterCount
        if (count == 0) return null
        val i = engine.currentChapter
        if (i < 0) return null
        return engine.chapterTitle(i) ?: "Chapter ${i + 1} of $count"
    }

    /** Publish only when something the lock screen shows has moved. */
    private fun publishStateIfChanged() {
        val playing = engine.isPlaying
        val expected = if (shownPlaying) {
            shownPositionMs + ((now() - shownAtMs) * shownSpeed).toLong()
        } else {
            shownPositionMs
        }
        val drifted = abs(engine.positionMs - expected) > 1000
        if (playing != shownPlaying || drifted || engine.speed != shownSpeed ||
            speechName != shownTitle || chapterLine() != shownChapter || engine.durationMs != shownDurationMs
        ) {
            publishState()
        }
    }

    /** Tell the session and the notification where things stand. */
    private fun publishState() {
        val playing = engine.isPlaying
        val position = engine.positionMs
        val speed = engine.speed
        val duration = engine.durationMs
        val chapter = chapterLine()
        val state = PlaybackStateCompat.Builder()
            .setActions(ACTIONS)
            .setState(
                if (playing) PlaybackStateCompat.STATE_PLAYING else PlaybackStateCompat.STATE_PAUSED,
                position,
                if (playing) speed else 0f,
                now(),
            )
            .build()
        session.setPlaybackState(state)
        if (speechName != shownTitle || chapter != shownChapter || duration != shownDurationMs || speechCover !== shownCover) {
            val meta = MediaMetadataCompat.Builder()
                .putString(MediaMetadataCompat.METADATA_KEY_TITLE, speechName ?: getString(R.string.app_name))
                .putString(MediaMetadataCompat.METADATA_KEY_ARTIST, chapter ?: "")
            speechAuthor?.let { meta.putString(MediaMetadataCompat.METADATA_KEY_AUTHOR, it) }
            speechSeries?.let { meta.putString(MediaMetadataCompat.METADATA_KEY_ALBUM, it) }
            speechCover?.let { meta.putBitmap(MediaMetadataCompat.METADATA_KEY_ART, it) }
            if (duration > 0) meta.putLong(MediaMetadataCompat.METADATA_KEY_DURATION, duration)
            session.setMetadata(meta.build())
            shownCover = speechCover
        }
        shownPlaying = playing
        shownPositionMs = position
        shownAtMs = now()
        shownSpeed = speed
        shownTitle = speechName
        shownChapter = chapter
        shownDurationMs = duration
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        nm.notify(NOTIFICATION_ID, buildNotification(playing, chapter))
    }

    private fun createChannel() {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        nm.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                getString(R.string.notification_channel),
                NotificationManager.IMPORTANCE_LOW,
            ),
        )
    }

    private fun openActivity(): PendingIntent = PendingIntent.getActivity(
        this, 0, Intent(this, MainActivity::class.java),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    private fun serviceAction(action: String): PendingIntent = PendingIntent.getService(
        this, action.hashCode(), Intent(this, PlaybackService::class.java).setAction(action),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    private fun buildNotification(playing: Boolean, chapter: String?): Notification {
        fun action(icon: Int, label: String, act: String) =
            NotificationCompat.Action(icon, label, serviceAction(act))
        val middle = if (playing) {
            action(android.R.drawable.ic_media_pause, "Pause", ACTION_PAUSE)
        } else {
            action(android.R.drawable.ic_media_play, "Play", ACTION_PLAY)
        }
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_launcher)
            .setContentTitle(speechName ?: getString(R.string.notification_title))
            .setContentText(chapter ?: "")
            .setContentIntent(openActivity())
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .addAction(action(android.R.drawable.ic_media_rew, "Back", ACTION_BACK))
            .addAction(middle)
            .addAction(action(android.R.drawable.ic_media_ff, "Forward", ACTION_FORWARD))
            .setStyle(
                MediaStyle()
                    .setMediaSession(session.sessionToken)
                    .setShowActionsInCompactView(0, 1, 2),
            )
            .build()
    }

    private fun startForeground() {
        val n = buildNotification(playing = false, chapter = null)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIFICATION_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        } else {
            startForeground(NOTIFICATION_ID, n)
        }
    }

    companion object {
        const val SAMPLE_RATE = 48_000
        const val CHANNELS = 2
        const val BLOCK_FRAMES = 1024
        /** How often the position goes back to the server while playing. */
        private const val SYNC_EVERY_MS = 30_000L
        private const val CHANNEL_ID = "playback"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_PLAY = "io.github.ronin11.underscore.PLAY"
        private const val ACTION_PAUSE = "io.github.ronin11.underscore.PAUSE"
        private const val ACTION_TOGGLE = "io.github.ronin11.underscore.TOGGLE"
        private const val ACTION_BACK = "io.github.ronin11.underscore.BACK"
        private const val ACTION_FORWARD = "io.github.ronin11.underscore.FORWARD"
        /** The browser came back from a single sign-on; the intent's data is the address. */
        const val ACTION_OPENID_RETURN = "io.github.ronin11.underscore.OPENID_RETURN"
        private const val SAVE_EVERY_MS = 15_000L
        private const val ACTIONS =
            PlaybackStateCompat.ACTION_PLAY or
                PlaybackStateCompat.ACTION_PAUSE or
                PlaybackStateCompat.ACTION_PLAY_PAUSE or
                PlaybackStateCompat.ACTION_STOP or
                PlaybackStateCompat.ACTION_SEEK_TO or
                PlaybackStateCompat.ACTION_FAST_FORWARD or
                PlaybackStateCompat.ACTION_REWIND or
                PlaybackStateCompat.ACTION_SKIP_TO_NEXT or
                PlaybackStateCompat.ACTION_SKIP_TO_PREVIOUS
    }
}
