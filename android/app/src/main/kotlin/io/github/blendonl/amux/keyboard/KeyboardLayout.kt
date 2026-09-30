package io.github.blendonl.amux.keyboard

enum class Side { LEFT, RIGHT }

typealias KeyRows = List<List<Key>>

data class Layer(val name: String, val left: KeyRows, val right: KeyRows) {
    fun rows(side: Side): KeyRows = when (side) {
        Side.LEFT -> left
        Side.RIGHT -> right
    }
}

data class KeyboardLayout(val leftPercent: Float, val rightPercent: Float, val layers: Map<String, Layer>) {
    init {
        require(BASE in layers) { "a layout needs a $BASE layer" }
    }

    val base: Layer
        get() = layers.getValue(BASE)

    val terminalPercent: Float
        get() = 100f - leftPercent - rightPercent

    companion object {
        const val BASE = "base"
        private const val PLACEHOLDER_PERCENT = 25f

        val BLANK = KeyboardLayout(
            PLACEHOLDER_PERCENT,
            PLACEHOLDER_PERCENT,
            mapOf(BASE to Layer(BASE, listOf(listOf(Key.BLANK)), listOf(listOf(Key.BLANK)))),
        )
    }
}
