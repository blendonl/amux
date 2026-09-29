package io.github.blendonl.amux.keyboard

enum class Modifier { CTRL, ALT, SHIFT }

sealed interface KeyAction {
    data class Type(val text: String, val shifted: String = text) : KeyAction

    data class Press(val keyCode: Int) : KeyAction

    data class Modify(val modifier: Modifier) : KeyAction

    data class UseLayer(val name: String) : KeyAction

    data class Send(val text: String) : KeyAction

    data object Paste : KeyAction

    data object Hide : KeyAction

    data object Blank : KeyAction

    data object Transparent : KeyAction
}

data class Key(
    val action: KeyAction,
    val label: String,
    val shiftedLabel: String = label,
    val width: Float = 1f,
    val repeats: Boolean = false,
) {
    val latches: Boolean
        get() = action is KeyAction.Modify || action is KeyAction.UseLayer

    fun label(shifted: Boolean): String = if (shifted) shiftedLabel else label

    companion object {
        val BLANK = Key(KeyAction.Blank, "")
        val TRANSPARENT = Key(KeyAction.Transparent, "")
    }
}
