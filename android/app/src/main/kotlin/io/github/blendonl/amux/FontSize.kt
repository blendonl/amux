package io.github.blendonl.amux

import android.content.Context
import android.util.TypedValue
import kotlin.math.roundToInt

class FontSize(context: Context) {
    private val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
    private val metrics = context.resources.displayMetrics
    private val smallest = pixels(SMALLEST_SP)
    private val largest = pixels(LARGEST_SP)
    private val step = pixels(STEP_SP).coerceAtLeast(1)

    var current: Int = preferences.getInt(KEY, pixels(DEFAULT_SP)).coerceIn(smallest, largest)
        private set

    fun adjust(larger: Boolean): Int {
        current = (current + if (larger) step else -step).coerceIn(smallest, largest)
        preferences.edit().putInt(KEY, current).apply()
        return current
    }

    private fun pixels(sp: Float): Int =
        TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, sp, metrics).roundToInt()

    private companion object {
        const val PREFERENCES = "terminal"
        const val KEY = "font_size"
        const val DEFAULT_SP = 12f
        const val SMALLEST_SP = 6f
        const val LARGEST_SP = 32f
        const val STEP_SP = 1f
    }
}
