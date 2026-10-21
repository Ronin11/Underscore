package io.github.ronin11.underscore

import android.app.Application

/**
 * Exists to catch a crash on the way out and write its stack trace where
 * the next bug report will find it. The default handler still runs, so the
 * app dies the normal way.
 */
class UnderscoreApp : Application() {
    override fun onCreate() {
        super.onCreate()
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, error ->
            try {
                BugReport.recordCrash(this, thread, error)
            } catch (_: Throwable) {
                // Nothing to do; the crash itself is more important.
            }
            previous?.uncaughtException(thread, error)
        }
    }
}
