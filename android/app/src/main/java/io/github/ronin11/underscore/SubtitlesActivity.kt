package io.github.ronin11.underscore

import android.graphics.Color
import android.graphics.Typeface
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.StyleSpan
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Subtitles: the line being read, large, with each speaker's words in
 * their own color and their name above when someone speaks; the lines
 * before and after, faded. For listeners who are hard of hearing, or
 * anyone who wants to read along. The screen stays on while it is open.
 */
class SubtitlesActivity : ServiceActivity() {

    override val tab: Tab? = null

    private lateinit var title: TextView
    private lateinit var note: TextView
    private lateinit var before: List<TextView>
    private lateinit var who: TextView
    private lateinit var line: TextView
    private lateinit var after: TextView
    private lateinit var playButton: Button
    private var shownIndex = -2
    private var shownSize = 0
    private var shownKey: String? = null

    private val fast = object : Runnable {
        override fun run() {
            show()
            ui.postDelayed(this, 200)
        }
    }

    override fun onResume() {
        super.onResume()
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        ui.post(fast)
    }

    override fun onPause() {
        super.onPause()
        ui.removeCallbacks(fast)
    }

    override fun refresh() {
        val s = service ?: return
        title.text = s.speechName ?: "Nothing open"
        val sub = s.subtitles
        note.text = when {
            sub.lines.isEmpty() -> "No transcript for this book yet. The map server makes one with its map."
            sub.voiced -> "Speakers in color: " + sub.cast.values.joinToString { it.name }
            else -> "Narration only. Speakers show once the map server works out who says what."
        }
        playButton.text = if (s.engine.isPlaying) "Pause" else "Play"
    }

    /** Put the current line and its neighbours on screen, when they change. */
    private fun show() {
        val s = service ?: return
        val sub = s.subtitles
        val size = s.settings.subtitleSize
        val at = sub.at(s.engine.positionMs)
        if (at == shownIndex && size == shownSize && sub.key == shownKey) return
        shownIndex = at
        shownSize = size
        shownKey = sub.key
        val lines = sub.lines
        val small = size * 0.6f
        before.forEachIndexed { i, v ->
            val l = lines.getOrNull(at - before.size + i)
            v.text = l?.let { styled(sub, it) } ?: ""
            v.setTextSize(TypedValue.COMPLEX_UNIT_SP, small)
        }
        val cur = lines.getOrNull(at)
        line.text = cur?.let { styled(sub, it) } ?: ""
        line.setTextSize(TypedValue.COMPLEX_UNIT_SP, size.toFloat())
        val speakers = cur?.spans?.map { it.who }?.filter { it != Subtitles.NARRATOR }?.distinct().orEmpty()
        who.text = SpannableStringBuilder().apply {
            speakers.forEachIndexed { i, id ->
                if (i > 0) append("  ·  ")
                val start = length
                append(sub.name(id) ?: id)
                setSpan(ForegroundColorSpan(sub.color(id)), start, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
        }
        who.setTextSize(TypedValue.COMPLEX_UNIT_SP, size * 0.55f)
        who.visibility = if (speakers.isEmpty()) View.INVISIBLE else View.VISIBLE
        after.text = lines.getOrNull(at + 1)?.let { styled(sub, it) } ?: ""
        after.setTextSize(TypedValue.COMPLEX_UNIT_SP, small)
    }

    /** A line with each speaker's words in their color; narration plain. */
    private fun styled(sub: Subtitles, l: Subtitles.Line): CharSequence = SpannableStringBuilder().apply {
        for (span in l.spans) {
            if (isNotEmpty()) append(' ')
            val start = length
            append(span.text)
            setSpan(ForegroundColorSpan(sub.color(span.who)), start, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            if (span.who != Subtitles.NARRATOR) setSpan(StyleSpan(Typeface.BOLD), start, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        }
    }

    private fun textView(alpha: Float): TextView = TextView(this).apply {
        setTextColor(Color.WHITE)
        this.alpha = alpha
        gravity = Gravity.CENTER
        setLineSpacing(0f, 1.15f)
    }

    private fun control(label: String, onClick: () -> Unit) = Button(this).apply {
        text = label
        setTextColor(Color.WHITE)
        setBackgroundColor(0x22FFFFFF)
        setOnClickListener { onClick(); shownIndex = -2; show(); refresh() }
    }

    override fun buildUi(): View {
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.BLACK)
            setPadding(dp(20), dp(12), dp(20), dp(16))
        }
        title = textView(0.8f).apply { textSize = 15f; setTypeface(typeface, Typeface.BOLD) }
        note = textView(0.5f).apply { textSize = 12f; setPadding(0, dp(2), 0, 0) }
        root.addView(title)
        root.addView(note)

        val middle = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER
        }
        before = List(2) { i -> textView(if (i == 0) 0.25f else 0.4f).apply { setPadding(0, 0, 0, dp(10)) } }
        before.forEach { middle.addView(it) }
        who = textView(1f).apply { setTypeface(typeface, Typeface.BOLD); setPadding(0, dp(8), 0, dp(4)) }
        line = textView(1f)
        after = textView(0.35f).apply { setPadding(0, dp(14), 0, 0) }
        middle.addView(who)
        middle.addView(line)
        middle.addView(after)
        middle.setOnClickListener { service?.toggle(); refresh() }
        root.addView(middle, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))

        val s = { service }
        playButton = control("Play") { s()?.toggle() }
        root.addView(
            LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                val lp = { LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { marginEnd = dp(6) } }
                addView(control("Back") { s()?.skipBack() }, lp())
                addView(playButton, lp())
                addView(control("A−") { s()?.settings?.let { it.subtitleSize = it.subtitleSize - 2 } }, lp())
                addView(control("A+") { s()?.settings?.let { it.subtitleSize = it.subtitleSize + 2 } }, lp())
                addView(control("Close") { finish() }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            },
        )
        return root
    }
}
