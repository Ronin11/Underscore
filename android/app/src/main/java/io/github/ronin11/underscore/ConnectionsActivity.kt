package io.github.ronin11.underscore

import android.Manifest
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.DocumentsContract
import android.view.Menu
import android.view.MenuItem
import android.view.View
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AlertDialog
import androidx.core.content.ContextCompat
import java.io.File

/**
 * What the app talks to: the Audiobookshelf server for books, the music
 * server or a folder on the phone for maps' music, and the sleep sources,
 * such as the watch. Each section lists what is added, or offers to add
 * one when it is empty. The + in the app bar adds any kind.
 */
class ConnectionsActivity : ServiceActivity() {

    override val tab = Tab.CONNECTIONS

    private val settings by lazy { Settings(this) }
    private val sleepSetup = SleepSetup(this, { service }) { refresh() }
    private lateinit var books: LinearLayout
    private lateinit var music: LinearLayout
    private lateinit var mapsSection: LinearLayout
    private lateinit var sleep: LinearLayout
    /** What the sections were last built from, so a tick only rebuilds when it changes. */
    private var built: String? = null
    private val rows = HashMap<String, Row>()

    private val pickMusicFolder = registerForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let { chooseMusicFolder(it) }
    }
    private val askAudioRead = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (!granted) toast("Without audio access the music folder cannot be read")
    }

    override fun onCreateOptionsMenu(menu: Menu): Boolean {
        menu.add("Add a connection").apply {
            setIcon(R.drawable.ic_add)
            icon?.setTint(ContextCompat.getColor(this@ConnectionsActivity, R.color.paper))
            setShowAsAction(MenuItem.SHOW_AS_ACTION_ALWAYS)
            setOnMenuItemClickListener { addDialog(); true }
        }
        return true
    }

    override fun refresh() {
        val s = service
        val setUp = Sleep.sources.filter { settings.sleepSetUp(it.key) }
        val shape = listOf(
            s?.serverUrl != null, s?.musicServerUrl != null, settings.musicDir != null, s?.mapServerUrl != null,
        ).joinToString() + setUp.joinToString { it.key }
        if (shape != built) {
            built = shape
            build(s?.serverUrl != null, s?.musicServerUrl != null, settings.musicDir != null, s?.mapServerUrl != null, setUp)
        }
        rows["maps"]?.summary = when {
            s == null -> "Starting…"
            s.serverUrl == null -> "${s.mapServerUrl}. Signs in through Audiobookshelf, so add that too."
            s.mapServerProblem != null -> "${s.mapServerUrl}: ${s.mapServerProblem}"
            else -> "${s.mapServerUrl}. Maps for your books, made when a book has none."
        }
        rows["server"]?.summary = when {
            s == null -> "Starting…"
            s.serverBusy != null -> s.serverBusy
            s.engine.hasServer -> "Signed in to ${s.serverUrl}"
            else -> "${s.serverUrl}, not signed in. Tap to sign in."
        }
        rows["music"]?.summary = when {
            s == null -> "Starting…"
            s.musicServerBusy != null -> s.musicServerBusy
            s.engine.hasMusicServer -> "Signed in to ${s.musicServerUrl}. Maps play from it."
            else -> "${s.musicServerUrl}, not signed in. Tap to sign in."
        }
        rows["folder"]?.summary = settings.musicDir
        for (source in setUp) {
            rows["sleep:${source.key}"]?.summary = when {
                Sleep.listening(settings, source) -> "Auto is on and listening"
                settings.sleepSource(source.key) && settings.sleepAuto -> "Auto is on"
                else -> "Set up. Not used by Auto."
            }
        }
    }

    /** Fill each section with what is added, or an Add row when it is empty. */
    private fun build(server: Boolean, musicServer: Boolean, folder: Boolean, mapServer: Boolean, setUp: List<SleepSource>) {
        rows.clear()
        books.removeAllViews()
        if (server) {
            books.addView(keep("server", row("Audiobookshelf", null) { serverDialog() }))
        } else {
            books.addView(addRow("Add an Audiobookshelf server") { serverDialog() })
        }
        music.removeAllViews()
        if (musicServer) music.addView(keep("music", row("Navidrome", null) { musicServerDialog() }))
        if (folder) music.addView(keep("folder", row("Music folder", null) { folderDialog() }))
        if (!musicServer && !folder) {
            music.addView(addRow("Add a music server") { musicServerDialog() })
            music.addView(addRow("Add a music folder on this phone") { pickMusicFolder.launch(null) })
        }
        mapsSection.removeAllViews()
        if (mapServer) {
            mapsSection.addView(keep("maps", row("Map server", null) { mapServerDialog() }))
        } else {
            mapsSection.addView(addRow("Add a map server") { mapServerDialog() })
        }
        sleep.removeAllViews()
        for (source in setUp) sleep.addView(keep("sleep:${source.key}", row(source.title, null) { sleepSetup.auto() }))
        if (setUp.isEmpty()) sleep.addView(addRow("Add a sleep source") { sleepSetup.add() })
    }

    private fun keep(key: String, row: Row): View {
        rows[key] = row
        return row.view
    }

    /** An empty section's row: a + and what it adds, in the accent color. */
    private fun addRow(text: String, onClick: () -> Unit): View = TextView(this).apply {
        this.text = text
        textSize = 16f
        setTextColor(themeColor(androidx.appcompat.R.attr.colorAccent))
        val plus = ContextCompat.getDrawable(context, R.drawable.ic_add)?.mutate()
        plus?.setTint(currentTextColor)
        setCompoundDrawablesRelativeWithIntrinsicBounds(plus, null, null, null)
        compoundDrawablePadding = dp(16)
        setPadding(dp(16), dp(14), dp(16), dp(14))
        isClickable = true
        isFocusable = true
        setBackgroundResource(selectableBackground())
        setOnClickListener { onClick() }
    }

    /** The + in the app bar: every kind of connection, and what each is for. */
    private fun addDialog() {
        val s = service
        data class Kind(val title: String, val summary: String, val note: String?, val add: (() -> Unit)?)
        val kinds = listOf(
            Kind(
                "Audiobookshelf server", "Your books, positions, and maps kept beside them.",
                if (s?.serverUrl != null) "One for now. Remove the one you have first." else null,
                { serverDialog() }.takeIf { s?.serverUrl == null },
            ),
            Kind(
                "Music server", "Navidrome or another Subsonic server. Maps play their music from it.",
                if (s?.musicServerUrl != null) "One for now. Remove the one you have first." else null,
                { musicServerDialog() }.takeIf { s?.musicServerUrl == null },
            ),
            Kind(
                "Music folder", "Music on this phone, for maps, when there is no music server.",
                if (settings.musicDir != null) "Picking another replaces ${settings.musicDir}." else null,
                { pickMusicFolder.launch(null) },
            ),
            Kind("Sleep source", "The watch, this phone, or another app tells the player you fell asleep.", null) { sleepSetup.add() },
            Kind(
                "Map server", "Keeps maps for your books, and makes one when a book has none.",
                if (s?.mapServerUrl != null) "One for now. Remove the one you have first." else null,
                { mapServerDialog() }.takeIf { s?.mapServerUrl == null },
            ),
        )
        val list = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, dp(8), 0, dp(8))
        }
        val dialog = AlertDialog.Builder(this)
            .setTitle("Add a connection")
            .setView(ScrollView(this).apply { addView(list) })
            .setNegativeButton("Cancel", null)
            .create()
        for (k in kinds) {
            list.addView(
                LinearLayout(this).apply {
                    orientation = LinearLayout.VERTICAL
                    setPadding(dp(24), dp(12), dp(24), dp(12))
                    addView(TextView(context).apply { text = k.title; textSize = 16f })
                    addView(TextView(context).apply { text = k.summary; textSize = 13f; alpha = 0.7f })
                    if (k.note != null) addView(TextView(context).apply { text = k.note; textSize = 13f; alpha = 0.7f })
                    val add = k.add
                    if (add != null) {
                        isClickable = true
                        isFocusable = true
                        setBackgroundResource(selectableBackground())
                        setOnClickListener { dialog.dismiss(); add() }
                    } else {
                        alpha = 0.45f
                    }
                },
            )
        }
        dialog.show()
    }

    /** Add the map server by its address, or remove the one there is. */
    private fun mapServerDialog() {
        val s = service ?: return
        val current = s.mapServerUrl
        if (current != null) {
            AlertDialog.Builder(this)
                .setTitle("Map server")
                .setMessage(current)
                .setPositiveButton("Remove") { _, _ -> s.mapServerForget(); refresh() }
                .setNegativeButton("Close", null)
                .show()
            return
        }
        val url = android.widget.EditText(this).apply {
            hint = "Map server address"
            setText("https://")
            inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_VARIATION_URI
        }
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(8), dp(16), 0)
            addView(url)
        }
        AlertDialog.Builder(this)
            .setTitle("Map server")
            .setMessage("Your own underscore map server. It signs in with your Audiobookshelf account, so it needs only its address.")
            .setView(box)
            .setPositiveButton("Add") { _, _ ->
                s.mapServerAdd(url.text.toString()) { problem ->
                    toast(problem ?: "Added")
                    refresh()
                }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun folderDialog() {
        AlertDialog.Builder(this)
            .setTitle("Music folder")
            .setMessage(settings.musicDir)
            .setPositiveButton("Change") { _, _ -> pickMusicFolder.launch(null) }
            .setNeutralButton("Remove") { _, _ -> settings.musicDir = null; refresh() }
            .setNegativeButton("Close", null)
            .show()
    }

    private fun chooseMusicFolder(uri: Uri) {
        val dir = folderPath(uri)
        if (dir == null) {
            toast("Pick a folder on this device's storage")
            return
        }
        settings.musicDir = dir.absolutePath
        val permission = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            Manifest.permission.READ_MEDIA_AUDIO
        } else {
            Manifest.permission.READ_EXTERNAL_STORAGE
        }
        if (ContextCompat.checkSelfPermission(this, permission) != PackageManager.PERMISSION_GRANTED) {
            askAudioRead.launch(permission)
        }
        refresh()
    }

    /**
     * A document-tree URI as a path, when it is on device storage. The
     * map's folder is scanned by path, so a folder only a provider can
     * read, such as a cloud drive, will not do.
     */
    private fun folderPath(uri: Uri): File? {
        if (uri.authority != "com.android.externalstorage.documents") return null
        val docId = DocumentsContract.getTreeDocumentId(uri)
        val volume = docId.substringBefore(':')
        val rel = docId.substringAfter(':', "")
        val root = if (volume == "primary") {
            Environment.getExternalStorageDirectory()
        } else {
            File("/storage/$volume")
        }
        return File(root, rel).takeIf { it.isDirectory }
    }

    override fun buildUi(): View {
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        root.addView(toolbar("Connections", up = false))
        val list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }

        books = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        music = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        mapsSection = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        sleep = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        list.addView(heading("Books"))
        list.addView(books)
        list.addView(divider())
        list.addView(heading("Music for maps"))
        list.addView(music)
        list.addView(mapsSection)
        list.addView(divider())
        list.addView(heading("Sleep"))
        list.addView(sleep)
        built = null

        root.addView(ScrollView(this).apply { addView(list) })
        return root
    }
}
