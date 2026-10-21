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
    var musicName: String? = null
        private set
    var mapName: String? = null
        private set
    /** The folder a map plays from. Picked by the activity, scanned by the engine. */
    var musicDir: File? = null
    /** True while a map is being loaded and its folder scanned. */
    @Volatile var mapLoading = false
        private set

    // What the session and notification last showed, to publish only on change.
    private var shownPlaying = false
    private var shownPositionMs = 0L
    private var shownAtMs = 0L
    private var shownSpeed = 0f
    private var shownTitle: String? = null
    private var shownChapter: String? = null
    private var shownDurationMs = -1L

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
            mainHandler.postDelayed(this, 1000)
        }
    }

    override fun onCreate() {
        super.onCreate()
        engine = Engine(SAMPLE_RATE, CHANNELS)
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
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_PLAY -> play()
            ACTION_PAUSE -> pause()
            ACTION_TOGGLE -> toggle()
            ACTION_BACK -> skipBack()
            ACTION_FORWARD -> skipForward()
        }
        return START_STICKY
    }

    override fun onDestroy() {
        mainHandler.removeCallbacks(stateTick)
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

    /** Open [uri] read-only and hand the descriptor to the engine. */
    fun loadSpeech(uri: Uri, name: String?): Boolean {
        val ok = load(uri) { fd, mime -> engine.loadSpeech(fd, mime) }
        if (ok) speechName = name
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
        if (dir == null) {
            onDone("Pick a music folder first")
            return
        }
        val cache = File(cacheDir, "library")
        mapLoading = true
        Thread({
            val problem = engine.loadMap(toml, dir.absolutePath, cache.absolutePath)
            if (problem == null) mapName = name
            mapLoading = false
            mainHandler.post { onDone(problem) }
        }, "underscore-map-load").start()
    }

    fun clearMap() {
        engine.clearMap()
        mapName = null
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
            appendLine("map status: ${engine.mapStatus}")
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
        if (speechName != shownTitle || chapter != shownChapter || duration != shownDurationMs) {
            val meta = MediaMetadataCompat.Builder()
                .putString(MediaMetadataCompat.METADATA_KEY_TITLE, speechName ?: getString(R.string.app_name))
                .putString(MediaMetadataCompat.METADATA_KEY_ARTIST, chapter ?: "")
            if (duration > 0) meta.putLong(MediaMetadataCompat.METADATA_KEY_DURATION, duration)
            session.setMetadata(meta.build())
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
        private const val CHANNEL_ID = "playback"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_PLAY = "io.github.ronin11.underscore.PLAY"
        private const val ACTION_PAUSE = "io.github.ronin11.underscore.PAUSE"
        private const val ACTION_TOGGLE = "io.github.ronin11.underscore.TOGGLE"
        private const val ACTION_BACK = "io.github.ronin11.underscore.BACK"
        private const val ACTION_FORWARD = "io.github.ronin11.underscore.FORWARD"
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
