package io.github.blendonl.amux

import android.content.Context
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager

class Bell(context: Context) {
    private val vibrator: Vibrator =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            context.getSystemService(VibratorManager::class.java).defaultVibrator
        } else {
            context.getSystemService(Vibrator::class.java)
        }

    private var quietUntil = 0L

    fun ring(now: Long) {
        if (now < quietUntil) return
        quietUntil = now + QUIET_MILLIS
        vibrator.vibrate(VibrationEffect.createPredefined(VibrationEffect.EFFECT_TICK))
    }

    private companion object {
        const val QUIET_MILLIS = 250L
    }
}
