package io.github.ronin11.underscore

import android.app.Activity
import android.content.Intent
import android.os.Bundle

/**
 * The browser lands here after an Audiobookshelf single sign-on, at
 * `underscore://oauth` or `audiobookshelf://oauth` with a code and a state.
 * Nothing to show: the address goes to [PlaybackService], which finishes
 * the sign-in, and the settings screen comes back to the front.
 */
class SignInReturnActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        intent?.data?.let { uri ->
            val handoff = Intent(this, PlaybackService::class.java)
                .setAction(PlaybackService.ACTION_OPENID_RETURN)
                .setData(uri)
            // This activity is in front, so a plain start is allowed.
            startService(handoff)
        }
        startActivity(
            Intent(this, SettingsActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        )
        finish()
    }
}
