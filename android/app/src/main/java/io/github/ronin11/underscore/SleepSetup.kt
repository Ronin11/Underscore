package io.github.ronin11.underscore

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Typeface
import android.os.Handler
import android.os.Looper
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat

/**
 * The Sleep button's Auto: the list of sleep sources set up so far, and
 * Add…, which walks through setting one up and testing it. Built by the
 * player, which must create it before it starts, since it asks for
 * permissions.
 */
class SleepSetup(
    private val activity: AppCompatActivity,
    private val service: () -> PlaybackService?,
    private val onChange: () -> Unit,
) {
    private val settings = Settings(activity)
    private val main = Handler(Looper.getMainLooper())
    private var afterPermission: ((Boolean) -> Unit)? = null
    private val askPermission = activity.registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        afterPermission?.invoke(granted)
        afterPermission = null
    }

    /** Auto: check the sources to use. With none set up yet, go straight to Add. */
    fun auto() {
        val setUp = Sleep.sources.filter { it.available && settings.sleepSetUp(it.key) }
        if (setUp.isEmpty()) {
            add()
            return
        }
        val names = Array(setUp.size) { setUp[it].title }
        val checked = BooleanArray(setUp.size) { settings.sleepSource(setUp[it].key) || !settings.sleepAuto }
        AlertDialog.Builder(activity)
            .setTitle("Notice when I fall asleep")
            .setMultiChoiceItems(names, checked) { _, which, on -> checked[which] = on }
            .setPositiveButton("Turn on") { _, _ ->
                setUp.forEachIndexed { i, s -> settings.setSleepSource(s.key, checked[i]) }
                setAuto(checked.any { it })
            }
            .setNeutralButton("Add…") { _, _ -> add() }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /** Switch Auto on or off. On clears a manual timer: one or the other. */
    fun setAuto(on: Boolean) {
        settings.sleepAuto = on
        val s = service()
        if (on && s != null) s.engine.setSleepMs(0, s.now())
        val problems = s?.applySleepSources() ?: Sleep.apply(activity, settings)
        for ((key, problem) in problems) {
            val title = Sleep.sources.firstOrNull { it.key == key }?.title ?: key
            toast("$title: $problem")
        }
        onChange()
    }

    /** Every kind of source, to set up a new one or test one again. */
    private fun add() {
        val list = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, dp(8), 0, dp(8))
        }
        val dialog = AlertDialog.Builder(activity)
            .setTitle("Add a way to notice sleep")
            .setView(ScrollView(activity).apply { addView(list) })
            .setNegativeButton("Cancel", null)
            .create()
        for (source in Sleep.sources) {
            val note = when {
                !source.available -> " (not built yet)"
                settings.sleepSetUp(source.key) -> " (set up)"
                else -> ""
            }
            list.addView(
                LinearLayout(activity).apply {
                    orientation = LinearLayout.VERTICAL
                    setPadding(dp(24), dp(12), dp(24), dp(12))
                    addView(TextView(activity).apply { text = source.title + note; textSize = 16f })
                    addView(TextView(activity).apply { text = source.summary; textSize = 13f; alpha = 0.7f })
                    if (source.available) {
                        isClickable = true
                        isFocusable = true
                        setBackgroundResource(selectable())
                        setOnClickListener { dialog.dismiss(); setUp(source) }
                    } else {
                        alpha = 0.45f
                    }
                },
            )
        }
        dialog.show()
    }

    /**
     * One source's setup: how it works, what it needs, and a live test.
     * While this is open the source's reports come here and not to the
     * player, so testing never pauses the book.
     */
    private fun setUp(source: SleepSource) {
        val box = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(24), dp(8), dp(24), dp(8))
        }
        box.addView(TextView(activity).apply { text = source.howItWorks; textSize = 14f })
        val status = TextView(activity).apply {
            textSize = 14f
            setTypeface(typeface, Typeface.BOLD)
            setPadding(0, dp(16), 0, dp(8))
        }
        val heard = TextView(activity).apply { textSize = 13f; alpha = 0.8f }
        val actions = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
        box.addView(actions)
        box.addView(status)
        box.addView(heard)

        val lines = ArrayList<String>()
        fun hear(line: String) {
            lines += java.text.DateFormat.getTimeInstance().format(java.util.Date()) + "  " + line
            heard.text = lines.takeLast(6).joinToString("\n")
        }

        when (source) {
            PhoneSleep -> {
                fun listen() {
                    val problem = PhoneSleep.start(activity)
                    status.text = problem ?: "Listening. The first reading comes within about 10 minutes. You can save now and it keeps going."
                    PhoneSleep.latest(activity)?.let { hear("Last reading: $it") }
                }
                val permission = PhoneSleep.permission
                if (granted(permission)) {
                    listen()
                } else {
                    status.text = "Needs the physical activity permission first."
                    actions.addView(
                        button("Allow physical activity") { b ->
                            afterPermission = { ok ->
                                if (ok) { b.visibility = View.GONE; listen() } else status.text = "Without that permission the phone cannot guess."
                            }
                            askPermission.launch(permission)
                        },
                    )
                }
            }
            OtherAppsSleep -> {
                actions.addView(
                    button("Copy the broadcast name") {
                        val clip = activity.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                        clip.setPrimaryClip(ClipData.newPlainText("broadcast", Sleep.ACTION_ASLEEP))
                        toast("Copied")
                    },
                )
                actions.addView(
                    button("Send a test from this phone") {
                        activity.sendBroadcast(
                            Intent(Sleep.ACTION_ASLEEP)
                                .setClass(activity, SleepReceiver::class.java)
                                .putExtra(Sleep.EXTRA_SOURCE, "this phone's test button")
                                .putExtra(Sleep.EXTRA_SINCE, System.currentTimeMillis() - 5 * 60_000L),
                        )
                    },
                )
                status.text = "Waiting for a test. Have your other app send the broadcast now."
            }
            else -> status.text = "Waiting to hear from it."
        }

        Sleep.testing = { from, line ->
            if (from == source) {
                hear(line)
                if (source == OtherAppsSleep) status.text = "It works. Save to use it."
            }
        }
        val saved = settings.sleepSetUp(source.key)
        val builder = AlertDialog.Builder(activity)
            .setTitle(source.title)
            .setView(ScrollView(activity).apply { addView(box) })
            .setPositiveButton("Save") { _, _ ->
                settings.setSleepSetUp(source.key, true)
                settings.setSleepSource(source.key, true)
                main.post { auto() }
            }
            .setNegativeButton("Cancel", null)
        if (saved) {
            builder.setNeutralButton("Remove") { _, _ ->
                settings.setSleepSetUp(source.key, false)
                settings.setSleepSource(source.key, false)
                if (Sleep.sources.none { Sleep.listening(settings, it) }) settings.sleepAuto = false
            }
        }
        builder.setOnDismissListener {
            Sleep.testing = null
            // Back to what the settings say: a test may have started a source no one uses.
            service()?.applySleepSources() ?: Sleep.apply(activity, settings)
            onChange()
        }
        builder.show()
    }

    private fun granted(permission: String?): Boolean =
        permission == null || ContextCompat.checkSelfPermission(activity, permission) == PackageManager.PERMISSION_GRANTED

    private fun button(text: String, onClick: (Button) -> Unit): Button = Button(activity).apply {
        this.text = text
        isAllCaps = false
        setOnClickListener { onClick(this) }
    }

    private fun selectable(): Int {
        val out = android.util.TypedValue()
        activity.theme.resolveAttribute(android.R.attr.selectableItemBackground, out, true)
        return out.resourceId
    }

    private fun dp(v: Int): Int = (v * activity.resources.displayMetrics.density).toInt()

    private fun toast(text: String) = Toast.makeText(activity, text, Toast.LENGTH_LONG).show()
}
