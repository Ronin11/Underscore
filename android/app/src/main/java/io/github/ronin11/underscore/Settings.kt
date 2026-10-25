package io.github.ronin11.underscore

import android.content.Context
import android.content.SharedPreferences

/**
 * The listener's choices, kept in one preferences file. The service applies
 * them to the engine at start and again whenever one changes, so the
 * settings screen never talks to the engine itself.
 */
class Settings(context: Context) {
    private val prefs: SharedPreferences = context.getSharedPreferences("settings", Context.MODE_PRIVATE)

    /** Seconds the skip-back button jumps. */
    var skipBackSec: Int
        get() = prefs.getInt(SKIP_BACK, 15)
        set(v) = prefs.edit().putInt(SKIP_BACK, v).apply()

    /** Seconds the skip-forward button jumps. */
    var skipForwardSec: Int
        get() = prefs.getInt(SKIP_FORWARD, 30)
        set(v) = prefs.edit().putInt(SKIP_FORWARD, v).apply()

    /** Rewind a little on resume, more after a long pause. */
    var rewindOnResume: Boolean
        get() = prefs.getBoolean(REWIND, true)
        set(v) = prefs.edit().putBoolean(REWIND, v).apply()

    /** Background music level before ducking, 0 to 100. */
    var musicLevel: Int
        get() = prefs.getInt(MUSIC_LEVEL, 60)
        set(v) = prefs.edit().putInt(MUSIC_LEVEL, v.coerceIn(0, 100)).apply()

    /** Keep the screen on while the book plays and the app is in front. */
    var keepScreenOn: Boolean
        get() = prefs.getBoolean(KEEP_SCREEN_ON, false)
        set(v) = prefs.edit().putBoolean(KEEP_SCREEN_ON, v).apply()

    /** The folder maps play music from, as a path on device storage. */
    var musicDir: String?
        get() = prefs.getString(MUSIC_DIR, null)
        set(v) = prefs.edit().putString(MUSIC_DIR, v).apply()

    /** Whether the sleep source with this key may wind the book down. All start off. */
    fun sleepSource(key: String): Boolean = prefs.getBoolean(SLEEP_PREFIX + key, false)

    fun setSleepSource(key: String, on: Boolean) = prefs.edit().putBoolean(SLEEP_PREFIX + key, on).apply()

    /** Push every engine-side setting into [engine]. Cheap; safe to repeat. */
    fun applyTo(engine: Engine) {
        engine.setSkipMs(skipBackSec * 1000L, skipForwardSec * 1000L)
        engine.setRewind(rewindOnResume)
        engine.setMusicLevel(musicLevel / 100f)
    }

    /**
     * Call [onChange] after any setting changes. The listener is held only
     * weakly by Android, so the caller keeps the returned object alive.
     */
    fun listen(onChange: () -> Unit): SharedPreferences.OnSharedPreferenceChangeListener {
        val l = SharedPreferences.OnSharedPreferenceChangeListener { _, _ -> onChange() }
        prefs.registerOnSharedPreferenceChangeListener(l)
        return l
    }

    fun unlisten(l: SharedPreferences.OnSharedPreferenceChangeListener) {
        prefs.unregisterOnSharedPreferenceChangeListener(l)
    }

    private companion object {
        const val SKIP_BACK = "skip_back_sec"
        const val SKIP_FORWARD = "skip_forward_sec"
        const val REWIND = "rewind_on_resume"
        const val MUSIC_LEVEL = "music_level"
        const val KEEP_SCREEN_ON = "keep_screen_on"
        const val MUSIC_DIR = "music_dir"
        const val SLEEP_PREFIX = "sleep_from_"
    }
}
