package io.github.ronin11.underscore

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.text.InputType
import android.util.TypedValue
import android.view.Gravity
import android.view.MenuItem
import android.view.View
import android.view.ViewGroup
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.appcompat.widget.Toolbar
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import java.util.Locale

/**
 * What every screen shares: a binding to [PlaybackService], a half-second
 * refresh while the screen is in front, a toolbar, the tabs along the
 * bottom, and the small helpers for building views in code. Screens
 * override [refresh] and fill their own layout.
 */
abstract class ServiceActivity : AppCompatActivity() {

    /** The four screens along the bottom. */
    enum class Tab(val label: String, val icon: Int, val screen: Class<out ServiceActivity>) {
        PLAYER("Player", R.drawable.ic_tab_player, MainActivity::class.java),
        LIBRARY("Library", R.drawable.ic_tab_library, BooksActivity::class.java),
        CONNECTIONS("Connections", R.drawable.ic_tab_connections, ConnectionsActivity::class.java),
        SETTINGS("Settings", R.drawable.ic_tab_settings, SettingsActivity::class.java),
    }

    private companion object {
        /** The fill already asked about, across screens. */
        var askedFull: PlaybackService.Full? = null
    }

    /** The tab this screen is, or null for a screen with no tabs, such as Debug. */
    protected open val tab: Tab? = null

    protected var service: PlaybackService? = null
        private set
    private var appBar: Toolbar? = null
    protected val ui = Handler(Looper.getMainLooper())

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            service = (binder as PlaybackService.LocalBinder).service
            onServiceReady()
            refresh()
        }

        override fun onServiceDisconnected(name: ComponentName) {
            service = null
        }
    }

    private val tick = object : Runnable {
        override fun run() {
            checkFull()
            refresh()
            ui.postDelayed(this, 500)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val content = buildUi()
        val here = tab
        setContentView(
            if (here == null) content
            else LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                addView(content, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
                addView(divider())
                addView(tabBar(here))
            },
        )
        fitSystemBars()
        val intent = Intent(this, PlaybackService::class.java)
        ContextCompat.startForegroundService(this, intent)
        bindService(intent, connection, Context.BIND_AUTO_CREATE)
    }

    override fun onStart() {
        super.onStart()
        ui.post(tick)
    }

    override fun onStop() {
        ui.removeCallbacks(tick)
        super.onStop()
    }

    override fun onDestroy() {
        unbindService(connection)
        super.onDestroy()
    }

    override fun onOptionsItemSelected(item: MenuItem): Boolean {
        if (item.itemId == android.R.id.home) {
            finish()
            return true
        }
        return super.onOptionsItemSelected(item)
    }

    /**
     * Android 15 draws the app under the status and navigation bars. Push
     * the app bar's content down by the status bar's height, keeping its
     * dark background behind the status bar's light icons, and keep the
     * bottom of the screen above the navigation bar.
     */
    private fun fitSystemBars() {
        val content = findViewById<View>(android.R.id.content)
        WindowCompat.getInsetsController(window, content).isAppearanceLightStatusBars = false
        ViewCompat.setOnApplyWindowInsetsListener(content) { v, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout(),
            )
            v.setPadding(bars.left, 0, bars.right, bars.bottom)
            appBar?.setPadding(0, bars.top, 0, 0)
            WindowInsetsCompat.CONSUMED
        }
    }

    /**
     * Go to another tab. The player stays underneath the others, so Back
     * from any tab lands on it; the tab being left closes unless it is the
     * player.
     */
    protected fun go(to: Tab, extras: Intent.() -> Unit = {}) {
        if (to == tab) return
        startActivity(Intent(this, to.screen).addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT).apply(extras))
        @Suppress("DEPRECATION")
        overridePendingTransition(0, 0)
        if (tab != Tab.PLAYER) {
            finish()
            @Suppress("DEPRECATION")
            overridePendingTransition(0, 0)
        }
    }

    private fun tabBar(here: Tab): View = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        val accent = themeColor(androidx.appcompat.R.attr.colorAccent)
        val plain = themeColor(android.R.attr.textColorSecondary)
        for (t in Tab.entries) {
            val color = if (t == here) accent else plain
            val item = LinearLayout(context).apply {
                orientation = LinearLayout.VERTICAL
                gravity = Gravity.CENTER
                setPadding(0, dp(8), 0, dp(6))
                isClickable = true
                isFocusable = true
                contentDescription = t.label
                setBackgroundResource(selectableBackground())
                addView(
                    android.widget.ImageView(context).apply {
                        setImageResource(t.icon)
                        imageTintList = android.content.res.ColorStateList.valueOf(color)
                    },
                    LinearLayout.LayoutParams(dp(24), dp(24)),
                )
                addView(TextView(context).apply {
                    text = t.label
                    textSize = 11f
                    setTextColor(color)
                    gravity = Gravity.CENTER
                })
                setOnClickListener { go(t) }
            }
            addView(item, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        }
    }

    /**
     * The phone went over its storage limit and only removing books would
     * make room: ask which. The books played longest ago that would make
     * room come checked. Asked once per time it fills, on whichever screen
     * is in front.
     */
    private fun checkFull() {
        val s = service ?: return
        val full = s.storageFull ?: return
        if (full === askedFull) return
        askedFull = full
        val sizes = s.storage.sizes(s.libraryCache)
        val open = s.openKey
        val books = s.storage.books()
            .filter { !it.pinned && it.key != open && (sizes[it.key]?.first ?: 0) > 0 }
            .sortedBy { it.played }
        if (books.isEmpty()) return
        val size = { b: Long -> android.text.format.Formatter.formatShortFileSize(this, b) }
        val labels = books.map { b ->
            val played = if (b.played > 0) "played ${android.text.format.DateUtils.getRelativeTimeSpanString(b.played)}" else "not played here"
            "${b.title}\n${size(sizes[b.key]?.first ?: 0)}  ·  $played"
        }.toTypedArray()
        val checked = BooleanArray(books.size) { books[it].key in full.suggest }
        AlertDialog.Builder(this)
            .setTitle("Storage is full: ${size(full.bytes)} of ${size(full.budget)}")
            .setMultiChoiceItems(labels, checked) { _, which, on -> checked[which] = on }
            .setPositiveButton("Remove") { _, _ ->
                val keys = books.filterIndexed { i, _ -> checked[i] }.map { it.key }
                if (keys.isEmpty()) s.putOffFull() else s.removeBooks(keys) { refresh() }
            }
            .setNeutralButton("Raise the limit") { _, _ ->
                s.putOffFull()
                go(Tab.SETTINGS)
            }
            .setNegativeButton("Not now") { _, _ -> s.putOffFull() }
            .setOnCancelListener { s.putOffFull() }
            .show()
    }

    /** A picked file's name, for showing. */
    protected fun displayName(uri: android.net.Uri): String? =
        contentResolver.query(uri, arrayOf(android.provider.OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }

    /** The screen's whole layout, built once after the activity is up. */
    protected abstract fun buildUi(): View

    /** The service is bound; one-time setup that needs it goes here. */
    protected open fun onServiceReady() {}

    /** Bring the screen up to date. Runs twice a second while in front. */
    protected abstract fun refresh()

    protected fun dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()

    protected fun toast(text: String, long: Boolean = true) {
        Toast.makeText(this, text, if (long) Toast.LENGTH_LONG else Toast.LENGTH_SHORT).show()
    }

    /** A theme color such as `androidx.appcompat.R.attr.colorAccent`. */
    protected fun themeColor(attr: Int): Int {
        val v = TypedValue()
        theme.resolveAttribute(attr, v, true)
        return if (v.resourceId != 0) ContextCompat.getColor(this, v.resourceId) else v.data
    }

    /** The app bar: dark, with the given title and a back arrow when [up]. */
    protected fun toolbar(title: String, up: Boolean): Toolbar {
        val bar = Toolbar(this).apply {
            this.title = title
            setBackgroundColor(ContextCompat.getColor(context, R.color.ink))
            setTitleTextColor(ContextCompat.getColor(context, R.color.paper))
            setSubtitleTextColor(ContextCompat.getColor(context, R.color.sky))
            popupTheme = androidx.appcompat.R.style.ThemeOverlay_AppCompat_DayNight
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT,
            )
        }
        setSupportActionBar(bar)
        appBar = bar
        supportActionBar?.setDisplayHomeAsUpEnabled(up)
        bar.navigationIcon?.setTint(ContextCompat.getColor(this, R.color.paper))
        bar.overflowIcon?.setTint(ContextCompat.getColor(this, R.color.paper))
        return bar
    }

    /** A bold, small heading above a group of rows. */
    protected fun heading(text: String): TextView = TextView(this).apply {
        this.text = text
        setTextColor(themeColor(androidx.appcompat.R.attr.colorAccent))
        textSize = 13f
        setPadding(dp(16), dp(20), dp(16), dp(6))
    }

    /**
     * One settings-style row: a title, a summary below it, and a tap action.
     * The returned row's [Row.summary] can be updated from [refresh].
     */
    protected fun row(title: String, summary: String?, onClick: (() -> Unit)?): Row {
        val titleView = TextView(this).apply {
            text = title
            textSize = 16f
        }
        val summaryView = TextView(this).apply {
            text = summary ?: ""
            textSize = 13f
            alpha = 0.7f
            visibility = if (summary == null) View.GONE else View.VISIBLE
        }
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(12), dp(16), dp(12))
            addView(titleView)
            addView(summaryView)
            if (onClick != null) {
                isClickable = true
                isFocusable = true
                setBackgroundResource(selectableBackground())
                setOnClickListener { onClick(); refresh() }
            }
        }
        return Row(box, titleView, summaryView)
    }

    protected fun selectableBackground(): Int {
        val v = TypedValue()
        theme.resolveAttribute(android.R.attr.selectableItemBackground, v, true)
        return v.resourceId
    }

    class Row(val view: LinearLayout, val title: TextView, private val summaryView: TextView) {
        var summary: String?
            get() = summaryView.text.toString().ifEmpty { null }
            set(v) {
                summaryView.text = v ?: ""
                summaryView.visibility = if (v == null) View.GONE else View.VISIBLE
            }
    }

    /** `h:mm:ss`, or `m:ss` under an hour. */
    protected fun fmt(ms: Long): String {
        val total = ms.coerceAtLeast(0) / 1000
        val h = total / 3600
        val m = (total % 3600) / 60
        val sec = total % 60
        return if (h > 0) String.format(Locale.US, "%d:%02d:%02d", h, m, sec)
        else String.format(Locale.US, "%d:%02d", m, sec)
    }

    /** `1.2x`. */
    protected fun fmtSpeed(speed: Float): String = String.format(Locale.US, "%.1fx", speed)

    /** Sign in to an Audiobookshelf server, or sign out of the current one. */
    protected fun serverDialog() {
        val s = service ?: return
        if (s.engine.hasServer) {
            AlertDialog.Builder(this)
                .setTitle("Audiobookshelf")
                .setMessage("Signed in to ${s.serverUrl}")
                .setPositiveButton("Remove") { _, _ -> s.serverForget(); refresh() }
                .setNegativeButton("Close", null)
                .show()
            return
        }
        val pad = dp(16)
        val url = EditText(this).apply {
            hint = "Server address"
            setText(s.serverUrl ?: "http://")
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
        }
        val user = EditText(this).apply {
            hint = "Username"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
        }
        val secret = EditText(this).apply {
            hint = "Password"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
        }
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad / 2, pad, 0)
            addView(url); addView(user); addView(secret)
        }
        AlertDialog.Builder(this)
            .setTitle("Audiobookshelf")
            .setMessage("Your own server. The password is used once to sign in and is not kept. If your account signs in through Authentik or another single sign-on, fill in only the address and use that button instead.")
            .setView(box)
            .setNeutralButton("Sign in with Authentik") { _, _ ->
                s.serverOpenIdStart(url.text.toString()) { authorize, problem ->
                    if (authorize != null) {
                        startActivity(Intent(Intent.ACTION_VIEW, android.net.Uri.parse(authorize)))
                    } else {
                        toast(problem ?: "Single sign-on did not start")
                    }
                    refresh()
                }
                refresh()
            }
            .setPositiveButton("Sign in") { _, _ ->
                s.serverLogin(url.text.toString(), user.text.toString(), secret.text.toString()) { problem ->
                    toast(problem ?: "Signed in")
                    refresh()
                }
                refresh()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /** Sign in to the music server (Navidrome), or sign out of the current one. */
    protected fun musicServerDialog() {
        val s = service ?: return
        if (s.engine.hasMusicServer) {
            AlertDialog.Builder(this)
                .setTitle("Music server")
                .setMessage("Signed in to ${s.musicServerUrl}")
                .setPositiveButton("Remove") { _, _ -> s.musicServerForget(); refresh() }
                .setNegativeButton("Close", null)
                .show()
            return
        }
        val pad = dp(16)
        val url = EditText(this).apply {
            hint = "Server address"
            setText(s.musicServerUrl ?: "http://")
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
        }
        val user = EditText(this).apply {
            hint = "Username"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
        }
        val secret = EditText(this).apply {
            hint = "Password"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
        }
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad / 2, pad, 0)
            addView(url); addView(user); addView(secret)
        }
        AlertDialog.Builder(this)
            .setTitle("Music server")
            .setMessage("Navidrome or any Subsonic-compatible server of your own. Maps play their music from it. The password is used once to sign in and is not kept.")
            .setView(box)
            .setPositiveButton("Sign in") { _, _ ->
                s.musicServerLogin(url.text.toString(), user.text.toString(), secret.text.toString()) { problem ->
                    toast(problem ?: "Signed in")
                    refresh()
                }
                refresh()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /** A horizontal rule between groups. */
    protected fun divider(): View = View(this).apply {
        setBackgroundColor(0x22808080)
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(1))
    }

    /** A centered single-line label. */
    protected fun label(size: Float, gravityFlags: Int = Gravity.CENTER): TextView = TextView(this).apply {
        textSize = size
        gravity = gravityFlags
    }
}
