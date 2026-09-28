package io.github.blendonl.amux

import android.content.Context
import android.net.wifi.WifiManager
import android.os.PowerManager

class ServiceLocks(context: Context) {
    private val multicast: WifiManager.MulticastLock? = context.applicationContext
        .getSystemService(WifiManager::class.java)
        ?.createMulticastLock(TAG)
        ?.apply { setReferenceCounted(false) }

    private val wake = context.getSystemService(PowerManager::class.java)
        .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "$TAG:keep-awake")
        .apply { setReferenceCounted(false) }

    val keepingAwake: Boolean
        get() = wake.isHeld

    fun holdMulticast() {
        multicast?.acquire()
    }

    fun toggleKeepAwake() {
        if (wake.isHeld) wake.release() else wake.acquire()
    }

    fun releaseAll() {
        if (wake.isHeld) wake.release()
        if (multicast?.isHeld == true) multicast.release()
    }

    private companion object {
        const val TAG = "amux"
    }
}
