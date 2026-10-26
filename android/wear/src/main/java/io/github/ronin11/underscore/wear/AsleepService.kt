package io.github.ronin11.underscore.wear

import androidx.health.services.client.PassiveListenerService
import androidx.health.services.client.data.UserActivityInfo
import androidx.health.services.client.data.UserActivityState

/** Health Services says the wearer's state changed; asleep goes to the phone. */
class AsleepService : PassiveListenerService() {
    override fun onUserActivityInfoReceived(info: UserActivityInfo) {
        val state = info.userActivityState
        val at = info.stateChangeTime.toEpochMilli()
        Watch.note(this, "State: ${state.name}")
        if (!Watch.isOn(this)) return
        Watch.sendState(this, state.name.removePrefix("USER_ACTIVITY_"), at)
        if (state == UserActivityState.USER_ACTIVITY_ASLEEP) Watch.sendAsleep(this, at, test = false)
    }

    override fun onPermissionLost() {
        Watch.note(this, "Permission lost; open the app to allow it again")
    }
}
