package io.github.ronin11.underscore

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.text.Editable
import android.text.InputType
import android.text.TextWatcher
import android.util.LruCache
import android.view.Gravity
import android.view.Menu
import android.view.View
import android.view.ViewGroup
import android.view.inputmethod.EditorInfo
import android.widget.BaseAdapter
import android.widget.EditText
import android.widget.HorizontalScrollView
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.widget.Toolbar
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.Locale
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import io.github.ronin11.underscore.PlaybackService.ServerBook

/**
 * The Audiobookshelf books on a screen of their own: covers, where the
 * listener is in each, a search across title, author and series, a filter
 * by progress, and a choice of order. Tapping a book downloads it and goes
 * back to the player, which shows the download in its app bar.
 *
 * Covers come straight from the server, which serves them without a
 * login, and are kept under the cache folder so the list opens quickly
 * next time.
 */
class BooksActivity : ServiceActivity() {

    private enum class Show(val label: String) {
        ALL("All"), STARTED("In progress"), NEW("Not started"), DONE("Finished"),
    }

    private enum class Order(val label: String) {
        PLAYED("Last played"), ADDED("Recently added"), TITLE("Title"), AUTHOR("Author"), SERIES("Series"),
    }

    private lateinit var bar: Toolbar
    private lateinit var search: EditText
    private lateinit var listView: ListView
    private lateinit var emptyView: TextView
    private val chips = LinkedHashMap<Show, TextView>()
    private val adapter = Books()

    private var all: List<ServerBook> = emptyList()
    private var shown: List<ServerBook> = emptyList()
    private var show = Show.ALL
    private var order = Order.PLAYED
    private var loading = false
    private var problem: String? = null
    private val prefs by lazy { getSharedPreferences("books", MODE_PRIVATE) }

    private val memory = LruCache<String, Bitmap>(80)
    private val pending = HashSet<String>()
    private val pool: ExecutorService = Executors.newFixedThreadPool(3)
    private val coverDir by lazy { File(cacheDir, "covers").apply { mkdirs() } }

    override fun onCreate(savedInstanceState: Bundle?) {
        order = Order.entries.firstOrNull { it.name == prefs.getString("order", null) } ?: Order.PLAYED
        show = Show.entries.firstOrNull { it.name == prefs.getString("show", null) } ?: Show.ALL
        super.onCreate(savedInstanceState)
    }

    override fun onDestroy() {
        pool.shutdownNow()
        super.onDestroy()
    }

    override fun onServiceReady() {
        val s = service ?: return
        if (!s.engine.hasServer) {
            problem = "Not signed in to Audiobookshelf"
            apply()
            serverDialog()
            return
        }
        s.serverBookList?.let { all = it }
        apply()
        reload()
    }

    override fun onCreateOptionsMenu(menu: Menu): Boolean {
        menu.add("Sort").setOnMenuItemClickListener { sortDialog(); true }
        menu.add("Refresh").setOnMenuItemClickListener { reload(); true }
        menu.add("Audiobookshelf server…").setOnMenuItemClickListener { serverDialog(); true }
        return true
    }

    private fun reload() {
        val s = service ?: return
        if (!s.engine.hasServer) {
            serverDialog()
            return
        }
        loading = true
        problem = null
        apply()
        s.serverBooks { books, why ->
            loading = false
            if (books != null) all = books else problem = why ?: "No books"
            apply()
        }
    }

    private fun sortDialog() {
        val orders = Order.entries
        AlertDialog.Builder(this)
            .setTitle("Sort by")
            .setSingleChoiceItems(orders.map { it.label }.toTypedArray(), orders.indexOf(order)) { d, which ->
                order = orders[which]
                prefs.edit().putString("order", order.name).apply()
                apply()
                d.dismiss()
            }
            .show()
    }

    /** Filter, sort, and show. Cheap enough to run on every keystroke. */
    private fun apply() {
        val words = search.text.toString().lowercase(Locale.ROOT).split(' ').filter { it.isNotBlank() }
        val matching = all.filter { b ->
            val hay = "${b.title} ${b.author} ${b.series}".lowercase(Locale.ROOT)
            words.all { hay.contains(it) }
        }
        for ((which, chip) in chips) {
            val n = matching.count { shows(which, it) }
            chip.text = if (which == Show.ALL) "${which.label}  $n" else if (n > 0) "${which.label}  $n" else which.label
            styleChip(chip, which == show)
        }
        shown = matching.filter { shows(show, it) }.sortedWith(comparator())
        adapter.notifyDataSetChanged()
        emptyView.text = when {
            shown.isNotEmpty() -> ""
            problem != null -> problem
            loading -> "Loading books…"
            all.isEmpty() -> "The server has no books this account can see"
            else -> "No books match"
        }
        emptyView.visibility = if (shown.isEmpty()) View.VISIBLE else View.GONE
        refresh()
    }

    private fun shows(which: Show, b: ServerBook): Boolean = when (which) {
        Show.ALL -> true
        Show.STARTED -> b.progress >= 0 && !b.finished
        Show.NEW -> b.progress < 0 && !b.finished
        Show.DONE -> b.finished
    }

    private fun comparator(): Comparator<ServerBook> {
        val byTitle = compareBy<ServerBook> { titleKey(it.title) }
        return when (order) {
            Order.PLAYED -> compareByDescending<ServerBook> { it.playedMs }.then(byTitle)
            Order.ADDED -> compareByDescending<ServerBook> { it.addedMs }.then(byTitle)
            Order.TITLE -> byTitle
            Order.AUTHOR -> compareBy<ServerBook>({ it.author.isEmpty() }, { it.author.lowercase(Locale.ROOT) })
                .then(seriesOrder()).then(byTitle)
            Order.SERIES -> seriesOrder().then(byTitle)
        }
    }

    /** Books in a series first, by series name, then in reading order. */
    private fun seriesOrder(): Comparator<ServerBook> = compareBy(
        { it.series.isEmpty() },
        { seriesName(it.series).lowercase(Locale.ROOT) },
        { seriesNumber(it.series) },
    )

    override fun refresh() {
        val s = service ?: return
        bar.subtitle = s.serverBusy
            ?: if (loading) "Checking the server…"
            else s.serverUrl?.removePrefix("https://")?.removePrefix("http://")
    }

    private fun open(book: ServerBook) {
        val s = service ?: return
        if (book.open) {
            finish()
            return
        }
        if (!book.playable) {
            toast("\"${book.title}\" is ${book.files} audio files. Only single-file books play for now.")
            return
        }
        val app = applicationContext
        s.openServerBook(book) { why ->
            Toast.makeText(app, why ?: "Loaded ${book.title}", Toast.LENGTH_LONG).show()
        }
        finish()
    }

    override fun buildUi(): View {
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            // Keeps the search box from taking focus and raising the keyboard on arrival.
            isFocusableInTouchMode = true
        }
        bar = toolbar("Books", up = true)
        root.addView(bar)

        search = EditText(this).apply {
            hint = "Search title, author, series"
            isSingleLine = true
            inputType = InputType.TYPE_CLASS_TEXT
            imeOptions = EditorInfo.IME_ACTION_SEARCH
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(t: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun onTextChanged(t: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun afterTextChanged(e: Editable?) { apply() }
            })
        }
        root.addView(
            search,
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                setMargins(dp(12), dp(8), dp(12), 0)
            },
        )

        val chipRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(dp(12), dp(8), dp(12), dp(8))
        }
        for (which in Show.entries) {
            val chip = TextView(this).apply {
                text = which.label
                textSize = 14f
                setPadding(dp(14), dp(6), dp(14), dp(6))
                isClickable = true
                setOnClickListener {
                    show = which
                    prefs.edit().putString("show", show.name).apply()
                    apply()
                }
            }
            chips[which] = chip
            chipRow.addView(
                chip,
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                    rightMargin = dp(8)
                },
            )
        }
        root.addView(HorizontalScrollView(this).apply {
            isHorizontalScrollBarEnabled = false
            addView(chipRow)
        })
        root.addView(divider())

        emptyView = label(15f).apply {
            alpha = 0.7f
            setPadding(dp(24), dp(48), dp(24), dp(24))
        }
        root.addView(emptyView)

        listView = ListView(this).apply {
            adapter = this@BooksActivity.adapter
            divider = null
            setOnItemClickListener { _, _, position, _ -> shown.getOrNull(position)?.let { open(it) } }
        }
        root.addView(listView, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        return root
    }

    private fun styleChip(chip: TextView, on: Boolean) {
        val accent = themeColor(androidx.appcompat.R.attr.colorAccent)
        chip.background = GradientDrawable().apply {
            cornerRadius = dp(16).toFloat()
            if (on) setColor(accent) else setStroke(dp(1), 0x66808080)
        }
        chip.setTextColor(if (on) getColor(R.color.ink) else themeColor(android.R.attr.textColorPrimary))
    }

    private class Holder(
        val cover: ImageView,
        val title: TextView,
        val by: TextView,
        val meta: TextView,
        val progress: ProgressBar,
    )

    private inner class Books : BaseAdapter() {
        override fun getCount() = shown.size
        override fun getItem(position: Int) = shown[position]
        override fun getItemId(position: Int) = position.toLong()

        override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
            val view = convertView ?: rowView()
            val h = view.tag as Holder
            val b = shown[position]
            h.title.text = b.title
            h.by.text = listOf(b.author, b.series).filter { it.isNotEmpty() }.joinToString("  ·  ")
            h.by.visibility = if (h.by.text.isEmpty()) View.GONE else View.VISIBLE
            h.meta.text = meta(b)
            val started = b.progress >= 0 && !b.finished
            h.progress.visibility = if (started) View.VISIBLE else View.GONE
            h.progress.progress = (b.progress.coerceIn(0.0, 1.0) * 1000).toInt()
            view.alpha = if (b.playable) 1f else 0.5f
            cover(b, h.cover)
            return view
        }
    }

    private fun meta(b: ServerBook): String {
        val parts = ArrayList<String>()
        if (b.open) parts.add("Open now")
        when {
            !b.playable -> parts.add("${b.files} files, does not play yet")
            b.finished -> parts.add("Finished")
            b.progress >= 0 && b.durationMs > 0 ->
                parts.add("${(b.progress * 100).toInt()}%  ·  ${fmt(((1 - b.progress) * b.durationMs).toLong())} left")
            b.progress >= 0 -> parts.add("${(b.progress * 100).toInt()}%")
        }
        if (b.durationMs > 0 && (b.progress < 0 || b.finished || !b.playable)) parts.add(fmt(b.durationMs))
        return parts.joinToString("  ·  ")
    }

    private fun rowView(): View {
        val cover = ImageView(this).apply {
            scaleType = ImageView.ScaleType.CENTER_CROP
            clipToOutline = true
            background = GradientDrawable().apply { cornerRadius = dp(6).toFloat(); setColor(0x22808080) }
        }
        val title = TextView(this).apply {
            textSize = 16f
            maxLines = 2
            ellipsize = android.text.TextUtils.TruncateAt.END
            setTypeface(typeface, android.graphics.Typeface.BOLD)
        }
        val by = TextView(this).apply {
            textSize = 13f
            alpha = 0.75f
            maxLines = 1
            ellipsize = android.text.TextUtils.TruncateAt.END
        }
        val meta = TextView(this).apply { textSize = 12f; alpha = 0.6f; maxLines = 1 }
        val progress = ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply { max = 1000 }
        val text = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(12), 0, 0, 0)
            addView(title)
            addView(by)
            addView(meta)
            addView(progress, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(4)).apply { topMargin = dp(6) })
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(16), dp(10), dp(16), dp(10))
            addView(cover, LinearLayout.LayoutParams(dp(64), dp(64)))
            addView(text, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            tag = Holder(cover, title, by, meta, progress)
        }
    }

    /** Show the book's cover in [into], fetching it in the background the first time. */
    private fun cover(book: ServerBook, into: ImageView) {
        into.setTag(R.id.cover_book, book.id)
        val cached = memory.get(book.id)
        into.setImageBitmap(cached)
        val url = book.coverUrl
        if (cached != null || url == null || !pending.add(book.id)) return
        pool.execute {
            val bitmap = runCatching { loadCover(book.id, url) }.getOrNull()
            ui.post {
                pending.remove(book.id)
                if (bitmap == null) return@post
                memory.put(book.id, bitmap)
                for (i in 0 until listView.childCount) {
                    val h = listView.getChildAt(i).tag as? Holder ?: continue
                    if (h.cover.getTag(R.id.cover_book) == book.id) h.cover.setImageBitmap(bitmap)
                }
            }
        }
    }

    /** From the cache folder, or from the server into it. Runs on [pool]. */
    private fun loadCover(id: String, url: String): Bitmap? {
        val file = File(coverDir, id.replace(Regex("[^A-Za-z0-9_-]"), "_"))
        if (!file.isFile) {
            val conn = URL(url).openConnection() as HttpURLConnection
            conn.connectTimeout = 10_000
            conn.readTimeout = 20_000
            try {
                if (conn.responseCode != 200) return null
                val bytes = conn.inputStream.use { it.readBytes() }
                val part = File(coverDir, "${file.name}.part")
                part.writeBytes(bytes)
                part.renameTo(file)
            } finally {
                conn.disconnect()
            }
        }
        return BitmapFactory.decodeFile(file.path)
    }

    companion object {
        private val ARTICLE = Regex("^(the|a|an)\\s+", RegexOption.IGNORE_CASE)
        private val NUMBERED = Regex("^(.*?)\\s*#\\s*([0-9]+(?:\\.[0-9]+)?)")

        /** Sorts "The Hobbit" under H. */
        fun titleKey(title: String): String = title.replace(ARTICLE, "").lowercase(Locale.ROOT)

        /** "Flying Gas Can Series #1" is "Flying Gas Can Series". */
        fun seriesName(series: String): String =
            NUMBERED.find(series)?.groupValues?.get(1) ?: series.substringBefore(", ")

        /** "Flying Gas Can Series #1" is 1. No number sorts last. */
        fun seriesNumber(series: String): Double =
            NUMBERED.find(series)?.groupValues?.get(2)?.toDoubleOrNull() ?: Double.MAX_VALUE
    }
}
