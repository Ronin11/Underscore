package io.github.ronin11.underscore.wear

import android.app.Activity
import android.os.Bundle
import android.view.Gravity
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.core.content.ContextCompat

/**
 * One screen: turn sleep watching on or off, allow the permission, send
 * the phone a test, and see how things stand. Built in code, like the
 * phone app.
 */
class WatchActivity : Activity() {
    private lateinit var status: TextView
    private lateinit var toggle: Button
    private val main by lazy { ContextCompat.getMainExecutor(this) }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Round screens clip the corners; keep the content in the middle.
        val side = (resources.displayMetrics.widthPixels * 0.14).toInt()
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(side, side, side, side)
        }
        box.addView(TextView(this).apply { text = "underscore sleep"; textSize = 16f; gravity = Gravity.CENTER })
        toggle = Button(this).apply { isAllCaps = false; setOnClickListener { flip() } }
        box.addView(toggle)
        box.addView(
            Button(this).apply {
                text = "Send a test to the phone"
                isAllCaps = false
                setOnClickListener {
                    Watch.sendAsleep(this@WatchActivity, System.currentTimeMillis() - 5 * 60_000L, test = true)
                    Watch.note(this@WatchActivity, "Sent a test")
                    refresh()
                }
            },
        )
        status = TextView(this).apply { textSize = 12f; gravity = Gravity.CENTER; setPadding(0, side / 3, 0, 0) }
        box.addView(status)
        setContentView(ScrollView(this).apply { addView(box) })
    }

    override fun onResume() {
        super.onResume()
        // Health Services forgets the listener when the app is updated; set it again.
        if (Watch.isOn(this) && Watch.hasPermission(this)) Watch.setOn(this, true, main) { refresh() }
        refresh()
    }

    private fun flip() {
        if (Watch.isOn(this)) {
            Watch.setOn(this, false, main) { refresh() }
        } else if (!Watch.hasPermission(this)) {
            requestPermissions(arrayOf(Watch.permission), ASK)
        } else {
            Watch.setOn(this, true, main) { problem -> problem?.let { Watch.note(this, it) }; refresh() }
        }
        refresh()
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == ASK && Watch.hasPermission(this)) Watch.setOn(this, true, main) { refresh() }
        refresh()
    }

    private fun refresh() {
        toggle.text = if (Watch.isOn(this)) "Stop watching for sleep" else "Watch for sleep"
        Watch.status(this, main) { status.text = it }
    }

    private companion object {
        const val ASK = 1
    }
}
