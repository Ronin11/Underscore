package io.github.ronin11.underscore

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.net.Uri
import android.os.Binder
import android.os.Build
import android.os.IBinder
import android.os.SystemClock
import androidx.core.app.NotificationCompat
import java.io.File

/**
 * Owns the engine and the audio device. Runs in the foreground so playback
 * survives the screen turning off. The activity binds to it for control.
 *
 * The render thread asks the engine for one block at a time and writes it
 * straight to the [AudioTrack]. Nothing is written anywhere else.
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

    override fun onCreate() {
        super.onCreate()
        engine = Engine(SAMPLE_RATE, CHANNELS)
        createChannel()
        startForeground()
        startAudio()
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onDestroy() {
        running = false
        renderThread?.join(1000)
        track?.stop()
        track?.release()
        engine.close()
        super.onDestroy()
    }

    fun now(): Long = SystemClock.elapsedRealtime()

    /** Open [uri] read-only and hand the descriptor to the engine. */
    fun loadSpeech(uri: Uri, name: String?): Boolean {
        val ok = load(uri) { fd, mime -> engine.loadSpeech(fd, mime) }
        if (ok) speechName = name
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

    private val mainHandler = android.os.Handler(android.os.Looper.getMainLooper())

    private fun load(uri: Uri, into: (Int, String?) -> Boolean): Boolean {
        val pfd = contentResolver.openFileDescriptor(uri, "r") ?: return false
        val mime = contentResolver.getType(uri)
        // The engine takes ownership of the descriptor from here.
        val fd = pfd.detachFd()
        return into(fd, mime)
    }

    private fun startAudio() {
        val minBytes = AudioTrack.getMinBufferSize(
            SAMPLE_RATE, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_FLOAT,
        )
        val blockBytes = BLOCK_FRAMES * CHANNELS * 4
        val t = AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build(),
            )
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

    private fun startForeground() {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val n: Notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_launcher)
            .setContentTitle(getString(R.string.notification_title))
            .setContentIntent(open)
            .setOngoing(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()
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
    }
}
