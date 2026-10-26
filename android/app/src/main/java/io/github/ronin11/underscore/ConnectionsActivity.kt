package io.github.ronin11.underscore

import android.Manifest
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.DocumentsContract
import android.view.View
import android.widget.LinearLayout
import android.widget.ScrollView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import java.io.File

/**
 * What the app talks to: the Audiobookshelf server for books, the music
 * server or a folder on the phone for maps' music, and the sleep sources,
 * such as the watch.
 */
class ConnectionsActivity : ServiceActivity() {

    override val tab = Tab.CONNECTIONS

    private val settings by lazy { Settings(this) }
    private val sleepSetup = SleepSetup(this, { service }) { refresh() }
    private lateinit var server: Row
    private lateinit var musicServer: Row
    private lateinit var musicFolder: Row
    private lateinit var sleep: Row

    private val pickMusicFolder = registerForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let { chooseMusicFolder(it) }
    }
    private val askAudioRead = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (!granted) toast("Without audio access the music folder cannot be read")
    }

    override fun refresh() {
        val s = service
        server.summary = when {
            s == null -> "Starting…"
            s.serverBusy != null -> s.serverBusy
            s.engine.hasServer -> "Signed in to ${s.serverUrl}"
            s.serverUrl != null -> "${s.serverUrl}, not signed in"
            else -> "Not signed in. Books come from here."
        }
        musicServer.summary = when {
            s == null -> "Starting…"
            s.musicServerBusy != null -> s.musicServerBusy
            s.engine.hasMusicServer -> "Signed in to ${s.musicServerUrl}. Maps play from it."
            s.musicServerUrl != null -> "${s.musicServerUrl}, not signed in"
            else -> "Not signed in. Maps need this or a music folder."
        }
        musicFolder.summary = settings.musicDir ?: "Not set. Used when no music server is signed in."
        val listening = Sleep.sources.filter { Sleep.listening(settings, it) }.map { it.title }
        val setUp = Sleep.sources.filter { settings.sleepSetUp(it.key) }.map { it.title }
        sleep.summary = when {
            listening.isNotEmpty() -> "Auto is on: ${listening.joinToString()}"
            setUp.isNotEmpty() -> "Set up: ${setUp.joinToString()}. Auto is off."
            else -> "None set up. The watch, this phone, or another app can tell the player you fell asleep."
        }
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

        list.addView(heading("Books"))
        server = row("Audiobookshelf", null) { serverDialog() }
        list.addView(server.view)

        list.addView(divider())
        list.addView(heading("Music for maps"))
        musicServer = row("Navidrome", null) { musicServerDialog() }
        list.addView(musicServer.view)
        musicFolder = row("Music folder", null) { pickMusicFolder.launch(null) }
        list.addView(musicFolder.view)

        list.addView(divider())
        list.addView(heading("Sleep"))
        sleep = row("Sleep sources", null) { sleepSetup.auto() }
        list.addView(sleep.view)

        root.addView(ScrollView(this).apply { addView(list) })
        return root
    }
}
