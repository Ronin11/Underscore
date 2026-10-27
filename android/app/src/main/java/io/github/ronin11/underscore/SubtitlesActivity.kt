package io.github.ronin11.underscore

import android.graphics.Color
import android.graphics.Typeface
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.RelativeSizeSpan
import android.text.style.StyleSpan
import android.util.TypedValue
import android.view.Choreographer
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.Button
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Subtitles: the book scrolls past like a teleprompter, following the
 * voice. Each speaker starts a new line with their name in their color;
 * in the line being read, the words already spoken are bright and the
 * rest dim, so the eye has a place to follow. For listeners who are hard
 * of hearing, or anyone reading along. The screen stays on while it is
 * open.
 *
 * There are no word timings, only each line's start and end, so a line's
 * words are spread over its time by their length. Close enough to follow,
 * and it moves every frame rather than a line at a time.
 */
class SubtitlesActivity : ServiceActivity() {

    override val tab: Tab? = null

    private lateinit var title: TextView
    private lateinit var note: TextView
    private lateinit var stage: FrameLayout
    private lateinit var column: LinearLayout
    private lateinit var playButton: Button

    /** The lines shown, by their index in the book: [first] to [first] + views - 1. */
    private var first = -1
    private var shownKey: String? = null
    private var shownSize = 0
    private var shownCurrent = -1
    private var shownWord = -1

    private val frame = object : Choreographer.FrameCallback {
        override fun doFrame(frameTimeNanos: Long) {
            follow()
            Choreographer.getInstance().postFrameCallback(this)
        }
    }

    override fun onResume() {
        super.onResume()
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        Choreographer.getInstance().postFrameCallback(frame)
    }

    override fun onPause() {
        super.onPause()
        Choreographer.getInstance().removeFrameCallback(frame)
    }

    override fun refresh() {
        val s = service ?: return
        title.text = s.speechName ?: "Nothing open"
        val sub = s.subtitles
        note.text = when {
            sub.lines.isEmpty() -> "No transcript for this book yet. The map server makes one with its map."
            sub.voiced -> "Speakers in color"
            else -> "Narration only. Speakers show once the map server works out who says what."
        }
        playButton.text = if (s.engine.isPlaying) "Pause" else "Play"
    }

    /** Where the voice is: the line, and how far through it, 0 to 1. */
    private fun place(sub: Subtitles, ms: Long): Pair<Int, Float> {
        // Before the first line, wait at it.
        val i = sub.at(ms).coerceAtLeast(0)
        val line = sub.lines.getOrNull(i) ?: return i to 0f
        val span = (line.endMs - line.startMs).coerceAtLeast(1)
        return i to ((ms - line.startMs).toFloat() / span).coerceIn(0f, 1f)
    }

    /**
     * Once a frame: keep the window of lines around the voice, light the
     * words spoken, and scroll to them. The window slides: a line that
     * leaves the top is taken off and the scroll moves by its height, so
     * nothing on screen jumps. Only a seek far off rebuilds it.
     */
    private fun follow() {
        val s = service ?: return
        val sub = s.subtitles
        val size = s.settings.subtitleSize
        val (cur, through) = place(sub, s.engine.positionMs)
        val lines = sub.lines
        // A jump (a chapter, a seek, a skip of more than a few lines) starts
        // the page again; the voice moving on just slides it.
        val jumped = shownCurrent >= 0 && (cur < shownCurrent || cur > shownCurrent + JUMP)
        if (sub.key != shownKey || size != shownSize || first < 0 || jumped || cur < first || cur > first + column.childCount - 1) {
            build(sub, cur, size)
            light(sub, cur, through)
            measureNow()
            val v = column.getChildAt(cur - first) ?: return
            column.translationY = stage.height * 0.42f - (v.top + v.height * through)
            return
        }
        var shifted = 0f
        while (first < cur - BEFORE && column.childCount > 0) {
            shifted += column.getChildAt(0).height
            column.removeViewAt(0)
            first++
        }
        val lastWanted = (cur + AFTER).coerceAtMost(lines.size - 1)
        while (first + column.childCount - 1 < lastWanted) {
            column.addView(lineView(sub, first + column.childCount, size, AHEAD))
        }
        light(sub, cur, through)
        if (shifted != 0f) {
            // The lines below moved up by what was taken off; follow them this frame, measure next.
            column.translationY += shifted
            return
        }
        val v = column.getChildAt(cur - first) ?: return
        if (v.height == 0) return
        // The point being read sits a little above the middle, and moves with the voice.
        column.translationY = stage.height * 0.42f - (v.top + v.height * through)
    }

    /** Light the words spoken in line [cur], and put the line before back to plain. */
    private fun light(sub: Subtitles, cur: Int, through: Float) {
        val line = sub.lines.getOrNull(cur) ?: return
        val v = column.getChildAt(cur - first) as? TextView ?: return
        val word = (through * line.text.split(' ').size).toInt()
        if (cur == shownCurrent && word == shownWord) return
        if (cur != shownCurrent) {
            (column.getChildAt(shownCurrent - first) as? TextView)?.let { old ->
                sub.lines.getOrNull(shownCurrent)?.let { old.text = styled(sub, it, -1f) }
                old.alpha = if (shownCurrent < cur) PAST else AHEAD
            }
        }
        v.text = styled(sub, line, through)
        v.alpha = 1f
        shownCurrent = cur
        shownWord = word
    }

    private fun lineView(sub: Subtitles, i: Int, size: Int, alpha: Float) = TextView(this).apply {
        setTextColor(Color.WHITE)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, size.toFloat())
        setLineSpacing(0f, 1.15f)
        setPadding(0, 0, 0, dp(14))
        text = styled(sub, sub.lines[i], -1f)
        this.alpha = alpha
    }

    /**
     * Lay the lines out now rather than at the next frame, so a page built
     * after a jump is placed right away instead of a frame late in the old
     * place.
     */
    private fun measureNow() {
        val w = stage.width
        if (w == 0) return
        column.measure(
            View.MeasureSpec.makeMeasureSpec(w, View.MeasureSpec.EXACTLY),
            View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
        )
        column.layout(0, 0, w, column.measuredHeight)
    }

    /** Put the lines around [cur] on screen, from scratch. */
    private fun build(sub: Subtitles, cur: Int, size: Int) {
        shownKey = sub.key
        shownSize = size
        shownCurrent = -1
        shownWord = -1
        column.removeAllViews()
        first = (cur - BEFORE).coerceAtLeast(0)
        if (sub.lines.isEmpty()) return
        val last = (cur + AFTER).coerceIn(0, sub.lines.size - 1)
        for (i in first..last) column.addView(lineView(sub, i, size, if (i < cur) PAST else AHEAD))
    }

    /**
     * A line as text: each speaker from a new line, their name before their
     * words in their color; short speech tags such as "I say," stay with
     * the words around them. With [through] from 0 to 1, the words spoken
     * so far are bright and the rest dim.
     */
    private fun styled(sub: Subtitles, l: Subtitles.Line, through: Float): CharSequence {
        val out = SpannableStringBuilder()
        var lastSpeaker: String? = null
        for (span in l.spans) {
            val tag = span.who == Subtitles.NARRATOR && span.text.split(' ').size <= TAG_WORDS && lastSpeaker != null
            val speaker = if (tag) lastSpeaker else span.who
            if (out.isNotEmpty()) out.append(if (speaker != lastSpeaker) "\n" else " ")
            if (speaker != lastSpeaker && span.who != Subtitles.NARRATOR) {
                val start = out.length
                out.append(sub.name(span.who) ?: span.who).append("  ")
                out.setSpan(ForegroundColorSpan(sub.color(span.who)), start, out.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                out.setSpan(StyleSpan(Typeface.BOLD), start, out.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                out.setSpan(RelativeSizeSpan(0.62f), start, out.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
            val start = out.length
            out.append(span.text)
            out.setSpan(ForegroundColorSpan(sub.color(span.who)), start, out.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            if (span.who != Subtitles.NARRATOR) {
                out.setSpan(StyleSpan(Typeface.BOLD), start, out.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
            if (!tag) lastSpeaker = speaker
        }
        if (through >= 0f) dimUnspoken(out, l, through)
        return out
    }

    /** Dim the words not reached yet, counting through the line's words by length. */
    private fun dimUnspoken(out: SpannableStringBuilder, l: Subtitles.Line, through: Float) {
        val spoken = l.text
        val reach = (spoken.length * through).toInt()
        // Walk the shown text, skipping names and breaks, to find where that many characters of speech end.
        var seen = 0
        var cut = out.length
        var i = 0
        val names = out.getSpans(0, out.length, RelativeSizeSpan::class.java).map { out.getSpanStart(it) until out.getSpanEnd(it) }
        while (i < out.length) {
            if (names.any { i in it } || out[i] == '\n') {
                i++
                continue
            }
            if (seen >= reach) {
                // Finish the word being said, so it lights whole.
                while (i < out.length && out[i] != ' ' && out[i] != '\n') i++
                cut = i
                break
            }
            seen++
            i++
        }
        if (cut < out.length) {
            out.setSpan(ForegroundColorSpan(0x66FFFFFF), cut, out.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        }
    }

    private fun textView(alpha: Float): TextView = TextView(this).apply {
        setTextColor(Color.WHITE)
        this.alpha = alpha
        gravity = Gravity.CENTER
    }

    private fun control(label: String, onClick: () -> Unit) = Button(this).apply {
        text = label
        setTextColor(Color.WHITE)
        setBackgroundColor(0x22FFFFFF)
        setOnClickListener { onClick(); shownKey = null; refresh() }
    }

    override fun buildUi(): View {
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.BLACK)
            setPadding(dp(20), dp(12), dp(20), dp(16))
        }
        title = textView(0.8f).apply { textSize = 15f; setTypeface(typeface, Typeface.BOLD) }
        note = textView(0.5f).apply { textSize = 12f; setPadding(0, dp(2), 0, dp(8)) }
        root.addView(title)
        root.addView(note)

        column = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        stage = FrameLayout(this).apply {
            clipChildren = true
            addView(column, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
            setOnClickListener { service?.toggle(); refresh() }
        }
        root.addView(stage, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))

        val s = { service }
        playButton = control("Play") { s()?.toggle() }
        root.addView(
            LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                setPadding(0, dp(10), 0, 0)
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

    private companion object {
        /** Lines kept on screen before and after the one being read. */
        const val BEFORE = 8
        const val AFTER = 10
        /** A narrator span this short between speech is a speech tag: it stays on the speaker's line. */
        const val TAG_WORDS = 4
        /** Lines ahead the voice may move in one frame before it counts as a jump. */
        const val JUMP = 3
        const val PAST = 0.4f
        const val AHEAD = 0.55f
    }
}
