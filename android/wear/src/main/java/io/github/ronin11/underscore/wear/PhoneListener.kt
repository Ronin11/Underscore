package io.github.ronin11.underscore.wear

import com.google.android.gms.wearable.MessageEvent
import com.google.android.gms.wearable.WearableListenerService
import java.util.concurrent.Executor

/** The phone's setup screen asks how things stand; answer it. */
class PhoneListener : WearableListenerService() {
    override fun onMessageReceived(event: MessageEvent) {
        if (event.path != Watch.PATH_PING) return
        val app = applicationContext
        // Run the answer on whichever thread hears the capabilities.
        Watch.status(app, Executor { it.run() }) { Watch.send(app, Watch.PATH_STATUS, it) }
    }
}
