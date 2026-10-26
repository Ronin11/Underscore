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

    /** The Sleep button's Auto: the checked sleep sources may wind the book down. */
    var sleepAuto: Boolean
        get() = prefs.getBoolean(SLEEP_AUTO, false)
        set(v) = prefs.edit().putBoolean(SLEEP_AUTO, v).apply()

    /** Shaking the phone undoes an automatic sleep stop, or restarts a manual timer. */
    var sleepShake: Boolean
        get() = prefs.getBoolean(SLEEP_SHAKE, true)
        set(v) = prefs.edit().putBoolean(SLEEP_SHAKE, v).apply()

    /**
     * How much the download cache may hold, as a step of [STORAGE_STEPS_GB]:
     * 0 keeps only the open book and books kept offline, the last is no limit.
     */
    var storageStep: Int
        get() = prefs.getInt(STORAGE_STEP, STORAGE_STEPS_GB.indexOf(5)).coerceIn(0, STORAGE_STEPS_GB.lastIndex)
        set(v) = prefs.edit().putInt(STORAGE_STEP, v.coerceIn(0, STORAGE_STEPS_GB.lastIndex)).apply()

    /** The cache's budget in bytes, or null for no limit. */
    val storageBudget: Long?
        get() = STORAGE_STEPS_GB[storageStep].takeIf { it >= 0 }?.let { it * 1_000_000_000L }

    /** Over the budget, remove the books played longest ago without asking. */
    var autoRemove: Boolean
        get() = prefs.getBoolean(AUTO_REMOVE, false)
        set(v) = prefs.edit().putBoolean(AUTO_REMOVE, v).apply()

    /** Books kept offline, and their map music, download only on Wi-Fi or another unmetered network. */
    var wifiOnly: Boolean
        get() = prefs.getBoolean(WIFI_ONLY, true)
        set(v) = prefs.edit().putBoolean(WIFI_ONLY, v).apply()

    /** Whether the sleep source with this key has been set up and tested. */
    fun sleepSetUp(key: String): Boolean = prefs.getBoolean(SLEEP_SET_UP_PREFIX + key, false)

    fun setSleepSetUp(key: String, on: Boolean) = prefs.edit().putBoolean(SLEEP_SET_UP_PREFIX + key, on).apply()

    /** Whether the sleep source with this key is checked in the Auto list. */
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

    companion object {
        /** The storage slider's stops, in gigabytes; -1 is no limit. */
        val STORAGE_STEPS_GB = longArrayOf(0, 1, 2, 5, 10, 20, 50, -1)
        private const val STORAGE_STEP = "storage_step"
        private const val WIFI_ONLY = "wifi_only"
        private const val AUTO_REMOVE = "auto_remove"
        private const val SKIP_BACK = "skip_back_sec"
        private const val SKIP_FORWARD = "skip_forward_sec"
        private const val REWIND = "rewind_on_resume"
        private const val MUSIC_LEVEL = "music_level"
        private const val KEEP_SCREEN_ON = "keep_screen_on"
        private const val MUSIC_DIR = "music_dir"
        private const val SLEEP_PREFIX = "sleep_from_"
        private const val SLEEP_SET_UP_PREFIX = "sleep_set_up_"
        private const val SLEEP_AUTO = "sleep_auto"
        private const val SLEEP_SHAKE = "sleep_shake"
    }
}
