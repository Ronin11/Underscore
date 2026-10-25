package io.github.ronin11.underscore.wear

import androidx.health.services.client.PassiveListenerService
import androidx.health.services.client.data.UserActivityInfo
import androidx.health.services.client.data.UserActivityState

/** Health Services says the wearer's state changed; asleep goes to the phone. */
class AsleepService : PassiveListenerService() {
    override fun onUserActivityInfoReceived(info: UserActivityInfo) {
        val state = info.userActivityState
        Watch.note(this, "State: ${state.name}")
        if (state == UserActivityState.USER_ACTIVITY_ASLEEP && Watch.isOn(this)) {
            Watch.sendAsleep(this, info.stateChangeTime.toEpochMilli(), test = false)
        }
    }

    override fun onPermissionLost() {
        Watch.note(this, "Permission lost; open the app to allow it again")
    }
}
