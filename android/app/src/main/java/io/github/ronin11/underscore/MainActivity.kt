package io.github.ronin11.underscore

import android.Manifest
import android.content.pm.PackageManager
import android.content.res.ColorStateList
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.Button
import android.widget.ImageButton
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.SeekBar
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.widget.PopupMenu
import androidx.core.content.ContextCompat

/**
 * The player screen: a row of tools along the top, then the book, the
 * chapter, a scrub bar across the chapter, and transport. The Music tile
 * opens the maps screen; CC opens the subtitles. The library,
 * connections, and settings are the other tabs.
 */
class MainActivity : ServiceActivity() {

    override val tab = Tab.PLAYER

    private lateinit var bar: androidx.appcompat.widget.Toolbar
    private lateinit var coverView: android.widget.ImageView
    private lateinit var titleView: TextView
    private lateinit var seriesView: TextView
    private lateinit var sourceView: TextView
    private lateinit var chapterView: TextView
    private lateinit var emptyRow: View
    private lateinit var scrub: SeekBar
    private lateinit var elapsed: TextView
    private lateinit var remaining: TextView
    private lateinit var bookLine: TextView
    private lateinit var transport: Transport
    private lateinit var speedTile: Tile
    private lateinit var sleepTile: Tile
    private lateinit var musicTile: Tile
    private lateinit var textTile: Tile
    private var scrubbing = false
    private val sleepSetup = SleepSetup(this, { service }) { refresh() }

    private val pickSpeech = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        uri?.let { loadBook(it) }
    }
    private val askNotifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) { }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            askNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    private fun loadBook(uri: Uri) {
        val s = service ?: return
        val name = displayName(uri)
        if (!s.loadSpeech(uri, name)) toast("Could not open ${name ?: "file"}")
        refresh()
    }

    override fun refresh() {
        val s = service ?: run {
            bar.subtitle = "Starting…"
            return
        }
        val e = s.engine
        val pos = e.positionMs
        val dur = e.durationMs
        val speed = e.speed
        val playing = e.isPlaying
        val loaded = s.speechName != null

        bar.subtitle = s.serverBusy ?: if (s.mapLoading) "Loading the map, scanning the music folder…" else s.downloadLine()

        titleView.text = s.speechName ?: "No book open"
        val where = when {
            !loaded -> "Open a file, or a book from your Audiobookshelf server."
            s.bookFromServer -> (s.serverUrl ?: "Audiobookshelf").removePrefix("http://").removePrefix("https://")
            else -> "On this phone"
        }
        sourceView.text = if (loaded && s.speechAuthor != null) "${s.speechAuthor}  ·  $where" else where
        val series = if (loaded) s.speechSeries else null
        seriesView.text = series?.let { "$it  ›" } ?: ""
        seriesView.visibility = if (series == null) View.GONE else View.VISIBLE
        val cover = if (loaded) s.speechCover else null
        if (coverView.tag !== cover) {
            coverView.setImageBitmap(cover)
            coverView.tag = cover
        }
        coverView.visibility = if (cover == null) View.GONE else View.VISIBLE
        emptyRow.visibility = if (loaded) View.GONE else View.VISIBLE

        val chapters = e.chapterCount
        val chapter = e.currentChapter
        val inChapter = chapters > 0 && chapter >= 0
        chapterView.text = when {
            !loaded -> ""
            chapters == 0 -> "No chapter marks"
            !inChapter -> "$chapters chapters"
            // The book's own name for it: its first marks are often credits, so a count is off.
            else -> e.chapterTitle(chapter) ?: "Chapter ${chapter + 1} of $chapters"
        }
        chapterView.visibility = if (loaded) View.VISIBLE else View.GONE

        // The scrub bar spans the chapter, as on a CD; the whole book without marks.
        val start = if (inChapter) e.chapterStartMs(chapter) else 0L
        val end = if (inChapter) s.chapterEndMs(chapter) else dur
        val span = (end - start).coerceAtLeast(1)
        if (!scrubbing) {
            scrub.max = 1000
            scrub.progress = if (dur > 0) (((pos - start) * 1000) / span).toInt().coerceIn(0, 1000) else 0
        }
        scrub.isEnabled = dur > 0
        elapsed.text = fmt(pos - start)
        remaining.text = if (dur > 0) "-${fmt(end - pos)}" else "--:--"
        bookLine.text = if (dur > 0) {
            val left = ((dur - pos) / speed).toLong()
            "${fmt(pos)} of ${fmt(dur)}  ·  ${fmt(left)} left at ${fmtSpeed(speed)}"
        } else {
            ""
        }

        showTransport(transport)

        speedTile.value.text = fmtSpeed(speed)
        val left = e.sleepRemainingMs(s.now())
        sleepTile.value.text = when {
            left >= 0 -> fmt(left)
            e.sleepAtChapterEnd -> "Chapter end"
            s.settings.sleepAuto -> "Auto"
            else -> "Off"
        }
        musicTile.value.text = if (!e.musicEnabled) "Off" else if (e.hasMap) "Map" else "On"
        // A loaded map shows its icon beside the word.
        val showMapIcon = e.musicEnabled && e.hasMap
        if (showMapIcon != (musicTile.value.compoundDrawablesRelative[0] != null)) {
            val icon = if (showMapIcon) mapIcon(14) else null
            musicTile.value.setCompoundDrawablesRelative(icon, null, null, null)
            musicTile.value.compoundDrawablePadding = dp(4)
        }

        if (s.settings.keepScreenOn && playing) {
            window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        } else {
            window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }

    /** A list of the book's chapters; picking one jumps to it. */
    private fun showChapters() {
        val e = service?.engine ?: return
        val count = e.chapterCount
        if (count == 0) return
        val items = Array(count) { i ->
            val title = e.chapterTitle(i) ?: "Chapter ${i + 1}"
            "$title   ${fmt(e.chapterStartMs(i))}"
        }
        AlertDialog.Builder(this)
            .setTitle("Chapters")
            .setSingleChoiceItems(items, e.currentChapter) { dialog, which ->
                e.seekChapter(which)
                dialog.dismiss()
                refresh()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /** A slider across the speed range with the usual presets under it. */
    private fun speedDialog() {
        val e = service?.engine ?: return
        val readout = label(28f)
        val slider = SeekBar(this).apply {
            max = ((MAX_SPEED - MIN_SPEED) / SPEED_STEP).toInt()
            progress = ((e.speed - MIN_SPEED) / SPEED_STEP + 0.5f).toInt()
        }
        fun show() { readout.text = fmtSpeed(e.speed) }
        slider.setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
            override fun onProgressChanged(sb: SeekBar, p: Int, fromUser: Boolean) {
                if (fromUser) { e.setSpeed(MIN_SPEED + p * SPEED_STEP); show() }
            }
            override fun onStartTrackingTouch(sb: SeekBar) {}
            override fun onStopTrackingTouch(sb: SeekBar) {}
        })
        val presets = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        for (p in floatArrayOf(0.8f, 1.0f, 1.2f, 1.5f, 2.0f)) {
            presets.addView(
                Button(this, null, androidx.appcompat.R.attr.borderlessButtonStyle).apply {
                    text = fmtSpeed(p)
                    setOnClickListener {
                        e.setSpeed(p)
                        slider.progress = ((p - MIN_SPEED) / SPEED_STEP + 0.5f).toInt()
                        show()
                    }
                },
                LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
            )
        }
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(24), dp(8), dp(24), 0)
            addView(readout)
            addView(slider)
            addView(presets)
        }
        show()
        AlertDialog.Builder(this)
            .setTitle("Narration speed")
            .setView(box)
            .setPositiveButton("Done") { _, _ -> refresh() }
            .show()
    }

    /**
     * Auto, Manual, or Off. Manual holds the timers; Auto lists the sleep
     * sources set up so far, with Add… to set up another.
     */
    private fun sleepMenu(anchor: View) {
        val s = service ?: return
        val e = s.engine
        val menu = PopupMenu(this, anchor)
        val running = e.sleepRemainingMs(s.now()) >= 0
        val manual = running || e.sleepAtChapterEnd
        val auto = s.settings.sleepAuto
        menu.menu.add(0, 0, 0, "Auto…").apply {
            isCheckable = true
            isChecked = auto && !manual
            setOnMenuItemClickListener { sleepSetup.auto(); true }
        }
        val timers = menu.menu.addSubMenu(0, 1, 1, "Manual")
        timers.item.isCheckable = true
        timers.item.isChecked = manual
        fun timer(set: () -> Unit) {
            if (s.settings.sleepAuto) sleepSetup.setAuto(false)
            set()
            refresh()
        }
        for (minutes in intArrayOf(10, 15, 20, 30, 45, 60)) {
            timers.add("$minutes minutes").setOnMenuItemClickListener {
                timer { s.setSleepTimer(minutes * 60_000L) }; true
            }
        }
        timers.add("End of this chapter").apply {
            isEnabled = e.chapterCount > 0
            setOnMenuItemClickListener { timer { e.setSleepEndOfChapter(s.now()) }; true }
        }
        if (running) {
            timers.add("10 more minutes").setOnMenuItemClickListener {
                e.extendSleepMs(10 * 60_000L, s.now()); refresh(); true
            }
        }
        menu.menu.add(0, 2, 2, "Off").apply {
            isCheckable = true
            isChecked = !auto && !manual
            setOnMenuItemClickListener {
                e.setSleepMs(0, s.now())
                if (s.settings.sleepAuto) sleepSetup.setAuto(false)
                refresh()
                true
            }
        }
        val dozed = s.dozedAtMs
        if (dozed >= 0) {
            menu.menu.add(1, 3, 3, "Back to where I fell asleep (${fmt(dozed)})").setOnMenuItemClickListener {
                s.backToDozed(); refresh(); true
            }
        }
        menu.menu.setGroupCheckable(0, true, true)
        menu.show()
    }

    /** The music under the book, and its maps, on their own screen. */
    private fun openMaps() {
        startActivity(android.content.Intent(this, MapsActivity::class.java))
    }

    private fun openSubtitles() {
        startActivity(android.content.Intent(this, SubtitlesActivity::class.java))
    }

    /** The open book's series on the book screen, in reading order. */
    private fun openSeries() {
        val series = service?.speechSeries ?: return
        go(Tab.LIBRARY) { putExtra(BooksActivity.EXTRA_SERIES, series) }
    }

    /** The server's books, on their own screen; without a server, the sign-in. */
    private fun serverBooks() {
        val s = service ?: return
        if (!s.engine.hasServer) {
            serverDialog()
            return
        }
        go(Tab.LIBRARY)
    }

    override fun buildUi(): View {
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        bar = toolbar("underscore", up = false)
        root.addView(bar)

        // Speed, sleep and music sit in the header, at the end. Chapters
        // are the chapter line under the title.
        speedTile = tile("Speed") { speedDialog() }
        sleepTile = tile("Sleep") { sleepMenu(it) }
        musicTile = tile("Music") { openMaps() }
        textTile = tile("Text") { openSubtitles() }
        textTile.value.text = "CC"
        headerTiles(bar, listOf(speedTile, sleepTile, musicTile, textTile))

        val body = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(24), dp(24), dp(24), dp(16))
        }

        coverView = android.widget.ImageView(this).apply {
            scaleType = android.widget.ImageView.ScaleType.CENTER_CROP
            clipToOutline = true
            background = GradientDrawable().apply { cornerRadius = dp(10).toFloat(); setColor(0x22808080) }
            contentDescription = "Cover"
            visibility = View.GONE
        }
        titleView = label(22f).apply {
            setTypeface(typeface, android.graphics.Typeface.BOLD)
            maxLines = 3
        }
        seriesView = label(14f).apply {
            setTextColor(themeColor(androidx.appcompat.R.attr.colorAccent))
            setPadding(0, dp(6), 0, 0)
            visibility = View.GONE
            setOnClickListener { openSeries() }
        }
        sourceView = label(13f).apply { alpha = 0.7f; setPadding(0, dp(4), 0, 0) }
        chapterView = label(15f).apply {
            setPadding(0, dp(16), 0, 0)
            // The chapters icon sits just before the chapter's name; tapping either lists them.
            ContextCompat.getDrawable(context, R.drawable.ic_chapters)?.mutate()?.let { icon ->
                icon.setTint(currentTextColor)
                icon.setBounds(0, 0, dp(18), dp(18))
                setCompoundDrawablesRelative(icon, null, null, null)
            }
            compoundDrawablePadding = dp(6)
            setOnClickListener { showChapters() }
        }
        val coverSize = minOf(resources.displayMetrics.widthPixels - dp(96), dp(260))
        body.addView(
            coverView,
            LinearLayout.LayoutParams(coverSize, coverSize).apply {
                gravity = Gravity.CENTER_HORIZONTAL
                bottomMargin = dp(16)
            },
        )
        body.addView(titleView)
        body.addView(seriesView)
        body.addView(sourceView)
        body.addView(
            chapterView,
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                gravity = Gravity.CENTER_HORIZONTAL
            },
        )

        emptyRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(0, dp(16), 0, 0)
            addView(
                Button(this@MainActivity).apply {
                    text = "Open a file"
                    setOnClickListener { pickSpeech.launch(AUDIO_TYPES) }
                },
                LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
            )
            addView(
                Button(this@MainActivity).apply {
                    text = "Server books"
                    setOnClickListener { serverBooks() }
                },
                LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
            )
        }
        body.addView(emptyRow)

        scrub = SeekBar(this).apply {
            setPadding(dp(4), dp(28), dp(4), dp(4))
            setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(sb: SeekBar, p: Int, fromUser: Boolean) {}
                override fun onStartTrackingTouch(sb: SeekBar) { scrubbing = true }
                override fun onStopTrackingTouch(sb: SeekBar) {
                    scrubbing = false
                    val s = service ?: return
                    val e = s.engine
                    val chapter = e.currentChapter
                    val inChapter = e.chapterCount > 0 && chapter >= 0
                    val start = if (inChapter) e.chapterStartMs(chapter) else 0L
                    val end = if (inChapter) s.chapterEndMs(chapter) else e.durationMs
                    if (end > start) s.seekMs(start + (end - start) * sb.progress / 1000)
                }
            })
        }
        body.addView(scrub)
        elapsed = label(13f, Gravity.START)
        remaining = label(13f, Gravity.END)
        body.addView(
            LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                addView(elapsed, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                addView(remaining, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            },
        )
        bookLine = label(12f).apply { alpha = 0.7f; setPadding(0, dp(4), 0, 0) }
        body.addView(bookLine)

        // Transport: previous chapter, back, play, forward, next chapter.
        transport = transport()
        body.addView(
            transport.view,
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                gravity = Gravity.CENTER_HORIZONTAL
            },
        )

        root.addView(
            ScrollView(this).apply { addView(body) },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f),
        )
        return root
    }

    companion object {
        private val AUDIO_TYPES = arrayOf("audio/*", "application/ogg", "video/mp4")
        // The engine's own limits; the slider covers the same range.
        private const val MIN_SPEED = 0.5f
        private const val MAX_SPEED = 3.5f
        private const val SPEED_STEP = 0.1f
    }
}
