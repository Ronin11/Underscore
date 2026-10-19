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
    val positionMs: Long get() = nativePositionMs(h())
    val durationMs: Long get() = nativeDurationMs(h())

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
    private external fun nativePositionMs(handle: Long): Long
    private external fun nativeDurationMs(handle: Long): Long
    private external fun nativeRender(handle: Long, out: FloatArray, nowMs: Long): Int

    companion object {
        init {
            System.loadLibrary("underscore_android")
        }
    }
}
