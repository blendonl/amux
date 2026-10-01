package io.github.blendonl.amux.keyboard

enum class Side { LEFT, RIGHT }

typealias KeyRows = List<List<Key>>

data class Layer(val name: String, val left: KeyRows?, val right: KeyRows?) {
    fun rows(side: Side): KeyRows? = when (side) {
        Side.LEFT -> left
        Side.RIGHT -> right
    }
}

data class KeyboardLayout(
    val leftPercent: Float,
    val rightPercent: Float,
    val layers: Map<String, Layer>,
    val holdMs: Long = DEFAULT_HOLD_MS,
    val tapsMs: Long = DEFAULT_TAPS_MS,
) {
    init {
        val base = requireNotNull(layers[BASE]) { "a layout needs a $BASE layer" }
        require(base.left != null && base.right != null) { "the $BASE layer needs both halves" }
    }

    val base: Layer
        get() = layers.getValue(BASE)

    val terminalPercent: Float
        get() = 100f - leftPercent - rightPercent

    fun baseRows(side: Side): KeyRows = base.rows(side).orEmpty()

    companion object {
        const val BASE = "base"
        const val DEFAULT_HOLD_MS = 300L
        const val DEFAULT_TAPS_MS = 250L
        private const val PLACEHOLDER_PERCENT = 21f

        val BLANK = KeyboardLayout(
            PLACEHOLDER_PERCENT,
            PLACEHOLDER_PERCENT,
            mapOf(BASE to Layer(BASE, listOf(listOf(Key.BLANK)), listOf(listOf(Key.BLANK)))),
        )
    }
}
